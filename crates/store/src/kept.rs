//! Kept files: files a Build moved out of a Managed Folder into the folder's retention area,
//! `.title-select-retained/<generation>/`, rather than deleting them. The retention area and each generation's folder
//! in it are created without following a symlink and recorded by identity before anything is moved into them; one
//! found with another identity, or not recorded at all, is never used. A file is kept aside by one rename within the
//! folder that never replaces anything, only while it is still the file the approved plan names, with the intent
//! recorded first; and it is deleted only by its own approved plan, only while the kept entry is still that file.

use std::collections::{BTreeSet, HashMap};
use std::io;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use rustix::fs::FileType;
use sha2::{Digest, Sha256};
use title_select_api::build::{
    Approval, FileStat, KeepAside, KeepReason, KeptDeletion, KeptDeletionContent,
    KeptDeletionOutcome, KeptFile, Marker, PlanKeptDeletion, RETENTION, kept_name,
};

use super::builds::sha256_of;
use super::folder_dir::{FolderDir, Identity, file_stat};
use super::folders::{check_marker, folder_by_id, open_writable_folder, store_id};
use super::plans::{json, to_json};
use super::{Error, Fault, Result, Store, now};
use crate::config::Config;

/// What a kept file cites, in `audit_citations` and `selection_citations`: the provenance of the file a Build wrote,
/// so that pruning never drops what its record needs.
fn cited_by(kept: i64) -> String {
    format!("kept-file:{kept}")
}

/// The directories Builds created in Managed Folder `folder` that outlive them, by name.
pub(super) fn folder_dirs(c: &Connection, folder: i64) -> Result<HashMap<String, Identity>> {
    let mut stmt = c.prepare("SELECT name, dev, ino FROM folder_dirs WHERE folder_id = ?1")?;
    let rows = stmt.query_map([folder], |r| {
        Ok((r.get(0)?, Identity::from_sql(r.get(1)?, r.get(2)?)))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The approved deletion of kept files of Managed Folder `folder` being carried out, if any.
pub(super) fn deletion_active(c: &Connection, folder: i64) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT id FROM kept_deletions WHERE folder_id = ?1 AND approved_at IS NOT NULL AND finished_at IS NULL",
        [folder],
        |r| r.get(0),
    )
    .optional()?)
}

/// The name a generation's folder is recorded under in `folder_dirs`.
fn generation_dir(generation: u64) -> String {
    format!("{RETENTION}/{generation}")
}

/// Why an entry in the retention area is not to be used.
fn not_ours(dir: &FolderDir, name: &str) -> String {
    format!(
        "{}/{name} is not something a build of this store created; it was left as it is. Move it out of the folder, \
         then try again",
        dir.path().display()
    )
}

/// A Managed Folder's retention area and one generation's folder in it, each opened without following a symlink
/// and found to be the directory a Build recorded creating.
pub(super) struct Retention {
    pub area: FolderDir,
    pub generation: FolderDir,
}

/// Opens the directory `name` in `parent`, which `folder_dirs` records under `key`, and checks it is the directory
/// recorded. When nothing is there and `create` is set, creates it for this service's user only, and records it
/// before it is used; otherwise returns `None`.
fn own_dir(
    store: &Store,
    folder: i64,
    parent: &FolderDir,
    name: &str,
    key: &str,
    create: bool,
) -> Result<std::result::Result<Option<FolderDir>, String>> {
    let recorded = store.read(|c| folder_dirs(c, folder))?.get(key).copied();
    let found = match parent.stat(name) {
        Ok(found) => found,
        Err(e) => return Ok(Err(e.to_string())),
    };
    match found {
        None if !create => Ok(Ok(None)),
        None => {
            if let Err(e) = parent.mkdir_private(name) {
                return Ok(Err(e.to_string()));
            }
            let dir = match parent.sub(name) {
                Ok(dir) => dir,
                Err(e) => return Ok(Err(e.to_string())),
            };
            let id = match dir.identity() {
                Ok(id) => id,
                Err(e) => return Ok(Err(e.to_string())),
            };
            let (dev, ino) = id.to_sql();
            store.write(|tx| {
                tx.execute(
                    "INSERT OR REPLACE INTO folder_dirs (folder_id, name, dev, ino) VALUES (?1, ?2, ?3, ?4)",
                    params![folder, key, dev, ino],
                )?;
                Ok(())
            })?;
            Ok(Ok(Some(dir)))
        }
        Some(st) => {
            if FolderDir::kind(&st) != FileType::Directory || recorded != Some(Identity::of(&st)) {
                return Ok(Err(not_ours(parent, name)));
            }
            let dir = match parent.sub(name) {
                Ok(dir) => dir,
                Err(e) => return Ok(Err(e.to_string())),
            };
            match dir.identity() {
                Ok(id) if Some(id) == recorded => Ok(Ok(Some(dir))),
                Ok(_) => Ok(Err(not_ours(parent, name))),
                Err(e) => Ok(Err(e.to_string())),
            }
        }
    }
}

/// Opens the retention area of Managed Folder `folder`, through its handle `dir`, and the folder of `generation` in
/// it; with `create`, creating either that is absent. `None` when one is absent and `create` is not set.
pub(super) fn open_retention(
    store: &Store,
    folder: i64,
    dir: &FolderDir,
    generation: u64,
    create: bool,
) -> Result<std::result::Result<Option<Retention>, String>> {
    let area = match own_dir(store, folder, dir, RETENTION, RETENTION, create)? {
        Ok(Some(area)) => area,
        other => return Ok(other.map(|_| None)),
    };
    let name = generation.to_string();
    let key = generation_dir(generation);
    Ok(own_dir(store, folder, &area, &name, &key, create)?
        .map(|found| found.map(|generation| Retention { area, generation })))
}

/// Whether `name` in `dir` is, without following a symlink, the regular file `id` with `file`'s size and
/// modification time.
fn is_kept_file(dir: &FolderDir, name: &str, id: Identity, file: Option<&FileStat>) -> bool {
    matches!(
        dir.stat(name),
        Ok(Some(st)) if FolderDir::kind(&st) == FileType::RegularFile
            && Identity::of(&st) == id
            && file.is_none_or(|f| *f == file_stat(&st))
    )
}

/// One file to keep aside: the item `seq` of Build `build`, which keeps it in the folder of `generation`.
pub(super) struct Keep<'a> {
    pub folder: i64,
    pub build: i64,
    pub seq: i64,
    pub generation: u64,
    pub keep: &'a KeepAside,
}

/// Records kept file `id` kept: its state, the citations of its provenance while not pruned, and, the file being no
/// longer at its name in the folder, the end of its built file's record. A removal's item is then applied.
pub(super) fn settle_kept(tx: &Transaction, id: i64) -> Result<()> {
    let (folder, build, seq, name): (i64, i64, i64, String) = tx.query_row(
        "SELECT folder_id, build_id, seq, name FROM kept_files WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    tx.execute(
        "UPDATE kept_files SET state = 'kept', kept_at = ?2, error = NULL WHERE id = ?1",
        params![id, now()],
    )?;
    let key = cited_by(id);
    tx.execute(
        "INSERT OR IGNORE INTO selection_citations (selection_id, cited_by)
         SELECT k.selection_id, ?2 FROM kept_files k JOIN selections s ON s.id = k.selection_id
         WHERE k.id = ?1 AND s.pruned_at IS NULL",
        params![id, key],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO audit_citations (audit_id, cited_by)
         SELECT k.audit_id, ?2 FROM kept_files k JOIN audits a ON a.id = k.audit_id
         WHERE k.id = ?1 AND a.pruned_at IS NULL",
        params![id, key],
    )?;
    tx.execute(
        "DELETE FROM built_files WHERE folder_id = ?1 AND target = ?2",
        params![folder, name],
    )?;
    tx.execute(
        "UPDATE build_items SET state = 'applied', error = NULL WHERE build_id = ?1 AND seq = ?2 AND kind = 'remove'",
        params![build, seq],
    )?;
    Ok(())
}

impl Store {
    /// Keeps aside, through the folder's handle `dir`, the file `k.keep` names: only while it is the regular file the
    /// plan found there, with the SHA-256 the plan recorded, if it did. Its intent is recorded first; it is then
    /// moved by one rename that never replaces anything into the retention area `retention`, and recorded kept once
    /// it is found there. Returns the kept file's id, or, when it was left in place, why; an error stops the Build.
    pub(super) fn keep_aside(
        &self,
        dir: &FolderDir,
        retention: &Retention,
        k: &Keep,
    ) -> Result<std::result::Result<i64, String>> {
        let name = k.keep.name.as_str();
        let id = Identity::from(k.keep.identity);
        let left = |why: &str| {
            Ok(Err(format!(
                "{name} was not kept aside: {why}; it was left as it is"
            )))
        };
        if !is_kept_file(dir, name, id, Some(&k.keep.file)) {
            return left("it changed since you approved the plan");
        }
        let file = match dir.open_file(name) {
            Ok(file) => file,
            Err(e) => return left(&e.to_string()),
        };
        let same = |st: &rustix::fs::Stat| Identity::of(st) == id && file_stat(st) == k.keep.file;
        match rustix::fs::fstat(&file) {
            Ok(st) if same(&st) => {}
            _ => return left("it changed while it was read"),
        }
        let (sha256, size) = match sha256_of(&file) {
            Ok(read) => read,
            Err(e) => return left(&e.to_string()),
        };
        match rustix::fs::fstat(&file) {
            Ok(st) if same(&st) && is_kept_file(dir, name, id, Some(&k.keep.file)) => {}
            _ => return left("it changed while it was read"),
        }
        drop(file);
        if k.keep.sha256.as_ref().is_some_and(|s| *s != sha256) {
            return left("it is not the file the plan was made with");
        }
        let taken: BTreeSet<String> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT kept_name FROM kept_files WHERE folder_id = ?1 AND generation = ?2
                 AND state IN ('keeping', 'kept', 'deleting')",
            )?;
            let rows = stmt.query_map(params![k.folder, k.generation as i64], |r| r.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })?;
        let mut nth = 1;
        let kept = loop {
            let candidate = kept_name(name, nth);
            match retention.generation.stat(&candidate) {
                Ok(None) if !taken.contains(&candidate) => break candidate,
                Ok(_) => nth += 1,
                Err(e) => return Err(Error::Io(e.to_string())),
            }
        };
        let (dev, ino) = id.to_sql();
        let row = self.write(|tx| {
            tx.execute(
                "INSERT INTO kept_files (folder_id, build_id, seq, generation, name, kept_name, reason, built_by, game,
                                         roms, selection_id, audit_id, dat_revision_id, sha256, size, mtime_ns, dev,
                                         ino, state, created_at)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, f.build_id, f.game, f.roms, f.selection_id, f.audit_id,
                        f.dat_revision_id, ?8, ?9, ?10, ?11, ?12, 'keeping', ?13
                 FROM (SELECT 1) LEFT JOIN built_files f ON f.folder_id = ?1 AND f.target = ?5 AND ?7 <> 'conflict'",
                params![
                    k.folder,
                    k.build,
                    k.seq,
                    k.generation as i64,
                    name,
                    kept,
                    k.keep.reason.as_str(),
                    sha256,
                    size as i64,
                    k.keep.file.mtime_ns as i64,
                    dev,
                    ino,
                    now()
                ],
            )?;
            Ok(tx.last_insert_rowid())
        })?;
        self.fault_point(Fault::BeforeKeep(k.seq));
        let forget = |why: String| -> Result<std::result::Result<i64, String>> {
            self.write(|tx| {
                tx.execute("DELETE FROM kept_files WHERE id = ?1", [row])?;
                Ok(())
            })?;
            Ok(Err(why))
        };
        if !is_kept_file(dir, name, id, Some(&k.keep.file)) {
            return forget(format!(
                "{name} was not kept aside: it changed while it was read; it was left as it is"
            ));
        }
        if let Err(e) = dir.rename_new(name, &retention.generation, &kept) {
            return match e.kind() {
                io::ErrorKind::AlreadyExists => forget(format!(
                    "{name} was not kept aside: {kept} appeared in the retention area; it was left as it is"
                )),
                _ => {
                    let _ = forget(e.to_string())?;
                    Err(Error::Io(e.to_string()))
                }
            };
        }
        retention
            .generation
            .sync()
            .and_then(|()| dir.sync())
            .map_err(|e| Error::Io(e.to_string()))?;
        self.fault_point(Fault::AfterKeep(k.seq));
        if !is_kept_file(&retention.generation, &kept, id, None) {
            let error = format!(
                "{}/{kept} is not the file moved there; it was left as it is",
                retention.generation.path().display()
            );
            self.write(|tx| {
                tx.execute(
                    "UPDATE kept_files SET state = 'lost', error = ?2 WHERE id = ?1",
                    params![row, error],
                )?;
                Ok(())
            })?;
            return Err(Error::Refused(error));
        }
        self.write(|tx| settle_kept(tx, row))?;
        Ok(Ok(row))
    }

    /// Settles each kept file Build `build` was keeping aside when it stopped, from what is in its folder, opened as
    /// `dir`: kept when the kept entry is the file, dropped when the file is still in place, and lost, and reported
    /// on its item, when it is neither. Returns why the folder could not be read, if it could not.
    pub(super) fn settle_keeping(
        &self,
        dir: &FolderDir,
        folder: i64,
        build: i64,
    ) -> Result<std::result::Result<(), String>> {
        type Row = (i64, i64, u64, String, String, Identity);
        let keeping: Vec<Row> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT id, seq, generation, name, kept_name, dev, ino FROM kept_files
                 WHERE build_id = ?1 AND state = 'keeping' ORDER BY id",
            )?;
            let rows = stmt.query_map([build], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get::<_, i64>(2)? as u64,
                    r.get(3)?,
                    r.get(4)?,
                    Identity::from_sql(r.get(5)?, r.get(6)?),
                ))
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })?;
        for (id, seq, generation, name, kept, identity) in keeping {
            let retention = match open_retention(self, folder, dir, generation, false)? {
                Ok(retention) => retention,
                Err(reason) => return Ok(Err(reason)),
            };
            let moved = retention
                .as_ref()
                .is_some_and(|r| is_kept_file(&r.generation, &kept, identity, None));
            let in_place = is_kept_file(dir, &name, identity, None);
            self.write(|tx| {
                let note = |state: &str, error: String| {
                    tx.execute(
                        "UPDATE build_items SET state = ?3, error = ?4
                         WHERE build_id = ?1 AND seq = ?2 AND state IN ('pending', 'publishing')",
                        params![build, seq, state, error],
                    )
                };
                if moved {
                    settle_kept(tx, id)?;
                    tx.execute(
                        "UPDATE build_items SET error = ?3 WHERE build_id = ?1 AND seq = ?2 AND kind = 'remove'",
                        params![
                            build,
                            seq,
                            format!(
                                "the build stopped while keeping {name} aside; it is kept as \
                                 {RETENTION}/{generation}/{kept}"
                            )
                        ],
                    )?;
                    note(
                        "failed",
                        format!(
                            "the build stopped after keeping {name} aside as {RETENTION}/{generation}/{kept}, before \
                             it wrote the new file"
                        ),
                    )?;
                } else if in_place {
                    tx.execute("DELETE FROM kept_files WHERE id = ?1", [id])?;
                    note(
                        "failed",
                        format!("the build stopped before it kept {name} aside; it is still in place"),
                    )?;
                } else {
                    let error = format!(
                        "the build stopped while keeping {name} aside, and it is now neither in place nor at \
                         {RETENTION}/{generation}/{kept}; something else moved it"
                    );
                    tx.execute(
                        "UPDATE kept_files SET state = 'lost', error = ?2 WHERE id = ?1",
                        params![id, error],
                    )?;
                    note("uncertain", error)?;
                }
                Ok(())
            })?;
        }
        Ok(Ok(()))
    }

    /// The kept files of Managed Folder `folder`, by generation, newest first.
    pub fn kept_files(&self, folder: i64) -> Result<Vec<KeptFile>> {
        self.read(|c| {
            folder_by_id(c, folder)?;
            kept_where(c, "k.folder_id = ?1 AND k.state = 'kept'", [folder])
        })
    }

    /// Plans deleting the kept files of Managed Folder `folder` that `request` picks, and stores the plan for review.
    /// Nothing is deleted until you approve it.
    pub fn plan_kept_deletion(
        &self,
        folder: i64,
        request: &PlanKeptDeletion,
    ) -> Result<KeptDeletion> {
        self.write(|tx| {
            folder_by_id(tx, folder)?;
            let kept = kept_where(tx, "k.folder_id = ?1 AND k.state = 'kept'", [folder])?;
            let files: Vec<KeptFile> = if request.all {
                kept
            } else {
                let mut files = Vec::new();
                for id in request.ids.iter().collect::<BTreeSet<_>>() {
                    match kept.iter().find(|k| k.id == *id) {
                        Some(k) => files.push(k.clone()),
                        None => {
                            return Err(Error::Refused(format!(
                                "managed folder {folder} has no kept file {id}"
                            )));
                        }
                    }
                }
                files
            };
            if files.is_empty() {
                return Err(Error::Refused("pick at least one kept file to delete".into()));
            }
            let content = KeptDeletionContent { folder, files };
            tx.execute(
                "INSERT INTO kept_deletions (folder_id, content, digest, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![folder, to_json(&content), digest(&content), now()],
            )?;
            deletion_by_id(tx, tx.last_insert_rowid())
        })
    }

    pub fn kept_deletion(&self, id: i64) -> Result<KeptDeletion> {
        self.read(|c| deletion_by_id(c, id))
    }

    /// Approves deleting kept files as reviewed with `digest`, and carries it out. Each file is deleted only while
    /// its kept entry is the file that was kept; anything else there is left, and its outcome says why. Refused while
    /// a Build of the folder is queued or running, and when the kept files changed since the plan.
    pub fn approve_kept_deletion(
        &self,
        config: &Config,
        id: i64,
        digest_reviewed: &str,
        actor: &str,
    ) -> Result<KeptDeletion> {
        let plan = self.kept_deletion(id)?;
        if plan.approval.is_some() {
            return Err(Error::Refused(format!("deletion {id} is already approved")));
        }
        if digest_reviewed != plan.digest {
            return Err(Error::Conflict(format!(
                "you reviewed a different version of deletion {id}; review it again"
            )));
        }
        let content = &plan.content;
        let folder = content.folder;
        self.write(|tx| {
            let active: Option<i64> = tx
                .query_row(
                    "SELECT id FROM jobs WHERE managed_folder_id = ?1 AND kind = 'build'
                     AND state IN ('queued', 'running')",
                    [folder],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(job) = active {
                return Err(Error::Conflict(format!(
                    "managed folder {folder} has a build queued or running: job {job}"
                )));
            }
            if let Some(other) = deletion_active(tx, folder)? {
                return Err(Error::Conflict(format!(
                    "kept files of managed folder {folder} are being deleted: deletion {other}"
                )));
            }
            let ids: Vec<i64> = content.files.iter().map(|f| f.id).collect();
            let now_kept = kept_where(tx, "k.folder_id = ?1 AND k.state = 'kept'", [folder])?;
            let still: Vec<&KeptFile> = ids
                .iter()
                .filter_map(|id| now_kept.iter().find(|k| k.id == *id))
                .collect();
            if still.len() != ids.len() || still.iter().zip(&content.files).any(|(a, b)| *a != b) {
                return Err(Error::Conflict(format!(
                    "the kept files changed since you reviewed deletion {id}; plan it again"
                )));
            }
            tx.execute(
                "UPDATE kept_deletions SET approval_digest = ?2, actor = ?3, approved_at = ?4 WHERE id = ?1",
                params![id, plan.digest, actor, now()],
            )?;
            Ok(())
        })?;
        let outcomes = self.delete_kept(config, id, content);
        let outcomes = match outcomes {
            Ok(outcomes) => outcomes,
            Err(err) => {
                let reason = err.to_string();
                content
                    .files
                    .iter()
                    .map(|f| left(f, reason.clone()))
                    .collect()
            }
        };
        self.write(|tx| {
            tx.execute(
                "UPDATE kept_deletions SET finished_at = ?2, outcomes = ?3 WHERE id = ?1",
                params![id, now(), to_json(&outcomes)],
            )?;
            deletion_by_id(tx, id)
        })
    }

    fn delete_kept(
        &self,
        config: &Config,
        deletion: i64,
        content: &KeptDeletionContent,
    ) -> Result<Vec<KeptDeletionOutcome>> {
        let folder = content.folder;
        let (row, store) = self.read(|c| Ok((folder_by_id(c, folder)?, store_id(c)?)))?;
        let all_left = |reason: String| {
            content
                .files
                .iter()
                .map(|f| left(f, reason.clone()))
                .collect()
        };
        let dir = match open_writable_folder(config, &row.root, &row.path) {
            Ok(dir) => dir,
            Err(reason) => return Ok(all_left(reason)),
        };
        let expected = Marker {
            store,
            folder,
            generation: row.generation,
        };
        let mut outcomes = Vec::new();
        let mut emptied = BTreeSet::new();
        for file in &content.files {
            if let Err(reason) = check_marker(&dir, &expected) {
                outcomes.push(left(file, reason));
                continue;
            }
            let retention = match open_retention(self, folder, &dir, file.generation, false)? {
                Ok(Some(retention)) => retention,
                Ok(None) => {
                    outcomes.push(left(file, "it is gone".into()));
                    continue;
                }
                Err(reason) => {
                    outcomes.push(left(file, reason));
                    continue;
                }
            };
            let id = Identity::from(file.identity);
            let gen_dir = &retention.generation;
            if !is_kept_file(gen_dir, &file.kept_name, id, Some(&file.file)) {
                let what = match gen_dir.stat(&file.kept_name) {
                    Ok(None) => "it is gone".to_owned(),
                    Ok(Some(_)) => {
                        "what is there now is not the file that was kept, so it is never deleted"
                            .to_owned()
                    }
                    Err(e) => e.to_string(),
                };
                outcomes.push(left(file, what));
                continue;
            }
            self.write(|tx| {
                tx.execute(
                    "UPDATE kept_files SET state = 'deleting', deletion_id = ?2 WHERE id = ?1 AND state = 'kept'",
                    params![file.id, deletion],
                )?;
                Ok(())
            })?;
            let removed = if is_kept_file(gen_dir, &file.kept_name, id, Some(&file.file)) {
                gen_dir
                    .unlink(&file.kept_name)
                    .and_then(|()| gen_dir.sync())
                    .map_err(|e| e.to_string())
            } else {
                Err("it changed while it was deleted, so it is never deleted".to_owned())
            };
            self.write(|tx| match &removed {
                Ok(()) => deleted(tx, file.id),
                Err(_) => {
                    tx.execute(
                        "UPDATE kept_files SET state = 'kept', deletion_id = NULL WHERE id = ?1",
                        [file.id],
                    )?;
                    Ok(())
                }
            })?;
            match removed {
                Ok(()) => {
                    emptied.insert(file.generation);
                    outcomes.push(KeptDeletionOutcome {
                        id: file.id,
                        kept_name: file.kept_name.clone(),
                        generation: file.generation,
                        deleted: true,
                        detail: None,
                    });
                }
                Err(reason) => outcomes.push(left(file, reason)),
            }
        }
        // A generation's folder left empty goes too, while it is still the directory a Build created.
        for generation in emptied {
            let Ok(Some(retention)) = open_retention(self, folder, &dir, generation, false)? else {
                continue;
            };
            let empty = retention
                .generation
                .entries()
                .is_ok_and(|entries| entries.is_empty());
            let key = generation_dir(generation);
            let own = self.read(|c| folder_dirs(c, folder))?.get(&key).copied();
            let name = generation.to_string();
            let still = matches!(
                retention.area.stat(&name),
                Ok(Some(st)) if FolderDir::kind(&st) == FileType::Directory && Some(Identity::of(&st)) == own
            );
            if empty && still && retention.area.rmdir(&name).is_ok() {
                retention
                    .area
                    .sync()
                    .map_err(|e| Error::Io(e.to_string()))?;
                self.write(|tx| {
                    tx.execute(
                        "DELETE FROM folder_dirs WHERE folder_id = ?1 AND name = ?2",
                        params![folder, key],
                    )?;
                    Ok(())
                })?;
            }
        }
        Ok(outcomes)
    }

    /// Run when the service starts: settles each kept file a deletion was deleting when the service stopped, from
    /// whether its kept entry is still there, and finishes the deletion.
    pub(super) fn recover_kept_deletions(&self, config: &Config) -> Result<()> {
        let unfinished: Vec<(i64, i64)> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT id, folder_id FROM kept_deletions WHERE approved_at IS NOT NULL AND finished_at IS NULL",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })?;
        for (deletion, folder) in unfinished {
            let plan = self.kept_deletion(deletion)?;
            let deleting: Vec<KeptFile> = self.read(|c| {
                kept_where(c, "k.deletion_id = ?1 AND k.state = 'deleting'", [deletion])
            })?;
            let row = self.read(|c| folder_by_id(c, folder))?;
            let dir = open_writable_folder(config, &row.root, &row.path).ok();
            for file in deleting {
                let id = Identity::from(file.identity);
                let present = match &dir {
                    // The folder cannot be reached: the file is kept until it can be seen gone.
                    None => true,
                    Some(dir) => match open_retention(self, folder, dir, file.generation, false)? {
                        Ok(Some(r)) => {
                            is_kept_file(&r.generation, &file.kept_name, id, None)
                                || r.generation.stat(&file.kept_name).is_err()
                        }
                        Ok(None) => false,
                        Err(_) => true,
                    },
                };
                self.write(|tx| {
                    if present {
                        tx.execute(
                            "UPDATE kept_files SET state = 'kept', deletion_id = NULL WHERE id = ?1",
                            [file.id],
                        )?;
                        Ok(())
                    } else {
                        deleted(tx, file.id)
                    }
                })?;
            }
            let outcomes: Vec<KeptDeletionOutcome> = self.read(|c| {
                plan.content
                    .files
                    .iter()
                    .map(|f| {
                        let state: String = c.query_row(
                            "SELECT state FROM kept_files WHERE id = ?1",
                            [f.id],
                            |r| r.get(0),
                        )?;
                        Ok(if state == "deleted" {
                            KeptDeletionOutcome {
                                id: f.id,
                                kept_name: f.kept_name.clone(),
                                generation: f.generation,
                                deleted: true,
                                detail: None,
                            }
                        } else {
                            left(f, "the service stopped before it was deleted".into())
                        })
                    })
                    .collect()
            })?;
            self.write(|tx| {
                tx.execute(
                    "UPDATE kept_deletions SET finished_at = ?2, outcomes = ?3 WHERE id = ?1",
                    params![deletion, now(), to_json(&outcomes)],
                )?;
                Ok(())
            })?;
        }
        Ok(())
    }
}

/// The outcome of a kept file left as it is, and why.
fn left(file: &KeptFile, why: String) -> KeptDeletionOutcome {
    KeptDeletionOutcome {
        id: file.id,
        kept_name: file.kept_name.clone(),
        generation: file.generation,
        deleted: false,
        detail: Some(format!("{why}; it was left as it is")),
    }
}

/// Records kept file `id` deleted, and drops what it cited.
fn deleted(tx: &Transaction, id: i64) -> Result<()> {
    tx.execute(
        "UPDATE kept_files SET state = 'deleted', deleted_at = ?2 WHERE id = ?1",
        params![id, now()],
    )?;
    let key = cited_by(id);
    tx.execute(
        "DELETE FROM selection_citations WHERE cited_by = ?1",
        [&key],
    )?;
    tx.execute("DELETE FROM audit_citations WHERE cited_by = ?1", [&key])?;
    Ok(())
}

fn digest(content: &KeptDeletionContent) -> String {
    let bytes = serde_json::to_vec(content).expect("a deletion always serializes");
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn kept_from_row(r: &Row) -> rusqlite::Result<KeptFile> {
    let reason: String = r.get(5)?;
    Ok(KeptFile {
        id: r.get(0)?,
        folder: r.get(1)?,
        generation: r.get::<_, i64>(2)? as u64,
        name: r.get(3)?,
        kept_name: r.get(4)?,
        reason: KeepReason::parse(&reason).unwrap_or(KeepReason::Removal),
        build: r.get(6)?,
        built_by: r.get(7)?,
        game: r.get(8)?,
        sha256: r.get(9)?,
        file: FileStat {
            size: r.get::<_, i64>(10)? as u64,
            mtime_ns: r.get::<_, i64>(11)? as i128,
        },
        identity: Identity::from_sql(r.get(12)?, r.get(13)?).into(),
        kept_at: r.get::<_, Option<String>>(14)?.unwrap_or_default(),
    })
}

fn kept_where(
    c: &Connection,
    condition: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<KeptFile>> {
    let mut stmt = c.prepare(&format!(
        "SELECT k.id, k.folder_id, k.generation, k.name, k.kept_name, k.reason, k.build_id, k.built_by, k.game,
                k.sha256, k.size, k.mtime_ns, k.dev, k.ino, k.kept_at
         FROM kept_files k WHERE {condition} ORDER BY k.generation DESC, k.id"
    ))?;
    let rows = stmt.query_map(params, kept_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn deletion_by_id(c: &Connection, id: i64) -> Result<KeptDeletion> {
    c.query_row(
        "SELECT id, digest, created_at, content, approval_digest, actor, approved_at, finished_at, outcomes
         FROM kept_deletions WHERE id = ?1",
        [id],
        |r| {
            let approval = match r.get::<_, Option<String>>(4)? {
                Some(digest) => Some(Approval {
                    digest,
                    actor: r.get(5)?,
                    at: r.get(6)?,
                }),
                None => None,
            };
            let outcomes: Option<String> = r.get(8)?;
            Ok(KeptDeletion {
                id: r.get(0)?,
                digest: r.get(1)?,
                created_at: r.get(2)?,
                content: json(r, 3)?,
                approval,
                finished_at: r.get(7)?,
                outcomes: outcomes
                    .map(|text| serde_json::from_str(&text))
                    .transpose()
                    .map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            8,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?
                    .unwrap_or_default(),
            })
        },
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("no deletion {id}")))
}
