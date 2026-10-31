//! Kept files: files a Build moved out of a Managed Folder into the folder's retention area,
//! `.title-select-retained/<generation>/`, rather than deleting them. The retention area and each folder in it are
//! created under a temporary name recorded by identity, made durable, renamed into place without replacing anything,
//! and recorded by identity before anything is moved into them; one found with another identity, or not recorded at
//! all, is never used. A file is kept aside by one rename within the folder that never replaces anything, only while
//! it is still the file the approved plan names, with the intent recorded first. It is deleted only by its own
//! approved plan: moved, with the intent recorded first, into the private folder `.deleting` in the retention area,
//! and deleted there only once what was moved is found to be the file that was kept.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use rustix::fs::FileType;
use sha2::{Digest, Sha256};
use title_select_api::build::{
    Approval, FileStat, KeepAside, KeepReason, KeptDeletion, KeptDeletionContent,
    KeptDeletionOutcome, KeptFile, Marker, PlanKeptDeletion, RETENTION, kept_name,
};

use super::builds::{record_temp, sha256_of, temps};
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

/// Why nothing else is done to Managed Folder `folder` now, when an approved deletion of its kept files is being
/// carried out, or waits for recovery.
pub(super) fn deletion_active(c: &Connection, folder: i64) -> Result<Option<String>> {
    let found: Option<(i64, Option<String>)> = c
        .query_row(
            "SELECT id, recovery_error FROM kept_deletions
             WHERE folder_id = ?1 AND approved_at IS NOT NULL AND finished_at IS NULL",
            [folder],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(found.map(|(id, error)| match error {
        None => format!("kept files of managed folder {folder} are being deleted: deletion {id}"),
        Some(error) => format!(
            "deletion {id} of kept files of managed folder {folder} stopped and was not finished: {error}. The \
             service finishes it when it next starts, once the folder can be shown to be this store's"
        ),
    }))
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

/// What marks a directory's temporary name while it is created: `<name>.new-<unique>`.
const DIR_TEMP: &str = ".new-";

/// The private folder in the retention area a kept file is moved into, by one rename that never replaces anything,
/// to be deleted there once it is found to be the file that was kept.
pub(super) const DELETING: &str = ".deleting";

/// Whether `name`, in a Managed Folder, is the temporary name its retention area is created under.
pub(super) fn is_dir_temp(name: &str) -> bool {
    name.strip_prefix(RETENTION)
        .is_some_and(|rest| rest.starts_with(DIR_TEMP))
}

/// A name no other attempt, in this process or another, uses.
fn unique() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!(
        "{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// `key` with its last component replaced by `name`.
fn sibling(key: &str, name: &str) -> String {
    match key.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{name}"),
        None => name.to_owned(),
    }
}

fn is_dir(st: &rustix::fs::Stat) -> bool {
    FolderDir::kind(st) == FileType::Directory
}

/// Creates the directory `name` in `parent`, which `folder_dirs` records under `key`, for this service's user only.
/// It is made under a temporary name recorded in `folder_temps` by identity, made durable, and renamed to `name`
/// without replacing anything; its parent is then made durable, and it is used only if what is at `name` is still
/// the directory made, which is then recorded. Recovery removes, or adopts, one a stop left behind.
fn create_dir(
    store: &Store,
    folder: i64,
    parent: &FolderDir,
    name: &str,
    key: &str,
) -> Result<std::result::Result<FolderDir, String>> {
    let temp = format!("{name}{DIR_TEMP}{}", unique());
    let temp_key = sibling(key, &temp);
    if let Err(e) = parent.mkdir_private(&temp) {
        return Ok(Err(e.to_string()));
    }
    let made = match parent
        .sub(&temp)
        .and_then(|made| Ok((made.identity()?, made)))
    {
        Ok(made) => made,
        Err(e) => return Ok(Err(e.to_string())),
    };
    let (id, made) = made;
    record_temp(store, folder, &temp_key, id)?;
    made.sync().map_err(|e| Error::Io(e.to_string()))?;
    drop(made);
    store.fault_point(Fault::BeforePublishDir);
    if let Err(e) = parent.rename_new(&temp, parent, name) {
        let ours =
            matches!(parent.stat(&temp), Ok(Some(st)) if is_dir(&st) && Identity::of(&st) == id);
        if ours && parent.rmdir(&temp).and_then(|()| parent.sync()).is_ok() {
            forget_temp(store, folder, &temp_key)?;
        }
        return Ok(Err(match e.kind() {
            io::ErrorKind::AlreadyExists => not_ours(parent, name),
            _ => e.to_string(),
        }));
    }
    parent.sync().map_err(|e| Error::Io(e.to_string()))?;
    let dir = match parent.sub(name) {
        Ok(dir) => dir,
        Err(e) => return Ok(Err(e.to_string())),
    };
    match dir.identity() {
        Ok(found) if found == id => {}
        Ok(_) => {
            forget_temp(store, folder, &temp_key)?;
            return Ok(Err(not_ours(parent, name)));
        }
        Err(e) => return Ok(Err(e.to_string())),
    }
    let (dev, ino) = id.to_sql();
    store.write(|tx| {
        tx.execute(
            "INSERT OR REPLACE INTO folder_dirs (folder_id, name, dev, ino) VALUES (?1, ?2, ?3, ?4)",
            params![folder, key, dev, ino],
        )?;
        tx.execute(
            "DELETE FROM folder_temps WHERE folder_id = ?1 AND name = ?2",
            params![folder, temp_key],
        )?;
        Ok(())
    })?;
    Ok(Ok(dir))
}

fn forget_temp(store: &Store, folder: i64, key: &str) -> Result<()> {
    store.write(|tx| {
        tx.execute(
            "DELETE FROM folder_temps WHERE folder_id = ?1 AND name = ?2",
            params![folder, key],
        )?;
        Ok(())
    })
}

/// Opens the directory `name` in `parent`, which `folder_dirs` records under `key`, and checks it is the directory
/// recorded. When nothing is there and `create` is set, creates it with [`create_dir`]; otherwise returns `None`.
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
        None => Ok(create_dir(store, folder, parent, name, key)?.map(Some)),
        Some(st) => {
            if !is_dir(&st) || recorded != Some(Identity::of(&st)) {
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

/// Opens the private folder kept files are deleted from, in the retention area `area` of Managed Folder `folder`.
fn open_deleting(
    store: &Store,
    folder: i64,
    area: &FolderDir,
    create: bool,
) -> Result<std::result::Result<Option<FolderDir>, String>> {
    own_dir(
        store,
        folder,
        area,
        DELETING,
        &format!("{RETENTION}/{DELETING}"),
        create,
    )
}

/// Removes the directory `name` in `parent`, recorded under `key`, if it is empty and still the directory recorded.
fn remove_if_empty(
    store: &Store,
    folder: i64,
    parent: &FolderDir,
    name: &str,
    key: &str,
) -> Result<()> {
    let Ok(Some(dir)) = own_dir(store, folder, parent, name, key, false)? else {
        return Ok(());
    };
    if !dir.entries().is_ok_and(|entries| entries.is_empty()) {
        return Ok(());
    }
    drop(dir);
    let own = store.read(|c| folder_dirs(c, folder))?.get(key).copied();
    let still = matches!(
        parent.stat(name),
        Ok(Some(st)) if is_dir(&st) && Some(Identity::of(&st)) == own
    );
    if still && parent.rmdir(name).is_ok() {
        parent.sync().map_err(|e| Error::Io(e.to_string()))?;
        store.write(|tx| {
            tx.execute(
                "DELETE FROM folder_dirs WHERE folder_id = ?1 AND name = ?2",
                params![folder, key],
            )?;
            Ok(())
        })?;
    }
    Ok(())
}

/// Settles each directory Title Select was creating in Managed Folder `folder`, opened as `dir`, when it stopped:
/// one still under its recorded temporary name is removed, and one already renamed into place, still the directory
/// made, is recorded. Anything else at either name is left as it is. Run only once the folder's marker shows it is
/// the store's. Returns why the folder could not be read, if it could not.
pub(super) fn settle_dir_temps(
    store: &Store,
    folder: i64,
    dir: &FolderDir,
) -> Result<std::result::Result<(), String>> {
    let mut pending: Vec<(String, Identity)> = store
        .read(|c| temps(c, folder))?
        .into_iter()
        .filter(|(key, _)| {
            let (parent, last) = match key.split_once('/') {
                Some((parent, last)) => (Some(parent), last),
                None => (None, key.as_str()),
            };
            parent.is_none_or(|p| p == RETENTION)
                && !last.contains('/')
                && (parent.is_some() || last.starts_with(RETENTION))
                && last
                    .split_once(DIR_TEMP)
                    .is_some_and(|(dest, _)| !dest.is_empty())
        })
        .collect();
    // The retention area before what is created in it.
    pending.sort_by_key(|(key, _)| key.contains('/'));
    for (key, id) in pending {
        let area;
        let (parent, last) = match key.split_once('/') {
            None => (dir, key.as_str()),
            Some((_, last)) => match own_dir(store, folder, dir, RETENTION, RETENTION, false)? {
                Ok(Some(found)) => {
                    area = found;
                    (&area, last)
                }
                Ok(None) => {
                    forget_temp(store, folder, &key)?;
                    continue;
                }
                Err(reason) => return Ok(Err(reason)),
            },
        };
        let dest = last
            .split_once(DIR_TEMP)
            .map(|(dest, _)| dest)
            .unwrap_or(last);
        let dest_key = sibling(&key, dest);
        match parent.stat(last) {
            Err(e) => return Ok(Err(e.to_string())),
            Ok(Some(st)) if is_dir(&st) && Identity::of(&st) == id => {
                if let Err(e) = parent.rmdir(last).and_then(|()| parent.sync()) {
                    return Ok(Err(format!(
                        "{}/{last} could not be removed ({e}); it was left as it is",
                        parent.path().display()
                    )));
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                let recorded = store
                    .read(|c| folder_dirs(c, folder))?
                    .contains_key(&dest_key);
                let made = matches!(
                    parent.stat(dest),
                    Ok(Some(st)) if is_dir(&st) && Identity::of(&st) == id
                );
                if made && !recorded {
                    parent.sync().map_err(|e| Error::Io(e.to_string()))?;
                    let (dev, ino) = id.to_sql();
                    store.write(|tx| {
                        tx.execute(
                            "INSERT INTO folder_dirs (folder_id, name, dev, ino) VALUES (?1, ?2, ?3, ?4)",
                            params![folder, dest_key, dev, ino],
                        )?;
                        Ok(())
                    })?;
                }
            }
        }
        forget_temp(store, folder, &key)?;
    }
    Ok(Ok(()))
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
            if let Some(busy) = deletion_active(tx, folder)? {
                return Err(Error::Conflict(busy));
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
        match self.delete_kept(config, id, content) {
            Ok(outcomes) => self.write(|tx| {
                tx.execute(
                    "UPDATE kept_deletions SET finished_at = ?2, outcomes = ?3 WHERE id = ?1",
                    params![id, now(), to_json(&outcomes)],
                )?;
                deletion_by_id(tx, id)
            }),
            // A kept file may have been mid-deletion: the deletion stays unfinished, and recovery settles it.
            Err(err) => {
                self.write(|tx| {
                    tx.execute(
                        "UPDATE kept_deletions SET recovery_error = ?2 WHERE id = ?1",
                        params![id, err.to_string()],
                    )?;
                    Ok(())
                })?;
                Err(err)
            }
        }
    }

    fn delete_kept(
        &self,
        config: &Config,
        deletion: i64,
        content: &KeptDeletionContent,
    ) -> Result<Vec<KeptDeletionOutcome>> {
        let folder = content.folder;
        let (row, store) = self.read(|c| Ok((folder_by_id(c, folder)?, store_id(c)?)))?;
        let dir = match open_writable_folder(config, &row.root, &row.path) {
            Ok(dir) => dir,
            Err(reason) => {
                return Ok(content
                    .files
                    .iter()
                    .map(|f| left(f, reason.clone()))
                    .collect());
            }
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
            let outcome = match open_retention(self, folder, &dir, file.generation, false)? {
                Ok(Some(retention)) => self.delete_one(&retention, file, deletion)?,
                Ok(None) => self.gone(file, deletion)?,
                Err(reason) => left(file, reason),
            };
            if outcome.deleted {
                emptied.insert(file.generation);
            }
            outcomes.push(outcome);
        }
        self.remove_emptied(folder, &dir, emptied)?;
        Ok(outcomes)
    }

    /// Deletes kept file `file` from `retention`, for deletion `deletion`: only while its kept entry is the file that
    /// was kept. The intent is committed first; the file is then moved, by one rename that never replaces anything,
    /// into the private folder kept files are deleted from, and deleted there only once what was moved is found to be
    /// the file. Anything else moved is put back.
    fn delete_one(
        &self,
        retention: &Retention,
        file: &KeptFile,
        deletion: i64,
    ) -> Result<KeptDeletionOutcome> {
        let id = Identity::from(file.identity);
        let (gen_dir, kept) = (&retention.generation, file.kept_name.as_str());
        match gen_dir.stat(kept) {
            Err(e) => return Ok(left(file, e.to_string())),
            Ok(None) => return self.gone(file, deletion),
            Ok(Some(_)) if !is_kept_file(gen_dir, kept, id, Some(&file.file)) => {
                return Ok(left(file, NOT_THE_FILE.into()));
            }
            Ok(Some(_)) => {}
        }
        let deleting = match open_deleting(self, file.folder, &retention.area, true)? {
            Ok(Some(deleting)) => deleting,
            Ok(None) => return Ok(left(file, "it could not be moved to be deleted".into())),
            Err(reason) => return Ok(left(file, reason)),
        };
        let slot = file.id.to_string();
        self.write(|tx| {
            tx.execute(
                "UPDATE kept_files SET state = 'deleting', deletion_id = ?2 WHERE id = ?1 AND state = 'kept'",
                params![file.id, deletion],
            )?;
            Ok(())
        })?;
        let keep = |why: String| -> Result<KeptDeletionOutcome> {
            self.write(|tx| still_kept(tx, file.id))?;
            Ok(left(file, why))
        };
        if !is_kept_file(gen_dir, kept, id, Some(&file.file)) {
            return keep("it changed while it was deleted, so it is never deleted".into());
        }
        self.fault_point(Fault::BeforeDeleteMove(file.id));
        if let Err(e) = gen_dir.rename_new(kept, &deleting, &slot) {
            return keep(format!(
                "it could not be moved to be deleted ({e}), so it is never deleted"
            ));
        }
        gen_dir
            .sync()
            .and_then(|()| deleting.sync())
            .map_err(|e| Error::Io(e.to_string()))?;
        self.fault_point(Fault::AfterDeleteMove(file.id));
        if !is_kept_file(&deleting, &slot, id, Some(&file.file)) {
            let back = put_back(gen_dir, &deleting, &slot, kept);
            return keep(format!("{MOVED_NOT_THE_FILE}; {back}"));
        }
        if let Err(e) = deleting.unlink(&slot) {
            let back = put_back(gen_dir, &deleting, &slot, kept);
            return keep(format!("it could not be deleted ({e}); {back}"));
        }
        deleting.sync().map_err(|e| Error::Io(e.to_string()))?;
        self.fault_point(Fault::AfterUnlink(file.id));
        self.write(|tx| deleted(tx, file.id))?;
        Ok(deleted_outcome(file))
    }

    /// Records kept file `file`, found gone when deletion `deletion` was carried out, no longer kept.
    fn gone(&self, file: &KeptFile, deletion: i64) -> Result<KeptDeletionOutcome> {
        self.write(|tx| {
            tx.execute(
                "UPDATE kept_files SET state = 'gone', deletion_id = ?2, deleted_at = ?3, error = ?4
                 WHERE id = ?1 AND state IN ('kept', 'deleting')",
                params![
                    file.id,
                    deletion,
                    now(),
                    "it was gone when its deletion was carried out"
                ],
            )?;
            drop_citations(tx, file.id)
        })?;
        Ok(gone_outcome(file))
    }

    /// Removes the private folder kept files are deleted from, and each folder of `generations`, of Managed Folder
    /// `folder`, opened as `dir`, that is left empty.
    fn remove_emptied(
        &self,
        folder: i64,
        dir: &FolderDir,
        generations: BTreeSet<u64>,
    ) -> Result<()> {
        let Ok(Some(area)) = own_dir(self, folder, dir, RETENTION, RETENTION, false)? else {
            return Ok(());
        };
        remove_if_empty(
            self,
            folder,
            &area,
            DELETING,
            &format!("{RETENTION}/{DELETING}"),
        )?;
        for generation in generations {
            remove_if_empty(
                self,
                folder,
                &area,
                &generation.to_string(),
                &generation_dir(generation),
            )?;
        }
        Ok(())
    }

    /// Run when the service starts, after [`Store::recover_builds`]: finishes each approved deletion of kept files
    /// the service stopped while carrying out, settling each kept file it was deleting from where that file is now.
    /// Nothing is settled unless the folder's root is writable and its marker shows it is the store's folder; a
    /// deletion left so keeps its kept files recorded and its folder's plans blocked until the next start. Returns,
    /// for each, the deletion and why.
    pub fn recover_kept_deletions(&self, config: &Config) -> Result<Vec<(i64, String)>> {
        let unfinished: Vec<(i64, i64)> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT id, folder_id FROM kept_deletions WHERE approved_at IS NOT NULL AND finished_at IS NULL",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })?;
        let mut blocked = Vec::new();
        for (deletion, folder) in unfinished {
            let error = self.finish_deletion(config, deletion, folder)?.err();
            self.write(|tx| {
                tx.execute(
                    "UPDATE kept_deletions SET recovery_error = ?2 WHERE id = ?1",
                    params![deletion, error],
                )?;
                Ok(())
            })?;
            if let Some(reason) = error {
                blocked.push((deletion, reason));
            }
        }
        Ok(blocked)
    }

    fn finish_deletion(
        &self,
        config: &Config,
        deletion: i64,
        folder: i64,
    ) -> Result<std::result::Result<(), String>> {
        let plan = self.kept_deletion(deletion)?;
        let (row, store) = self.read(|c| Ok((folder_by_id(c, folder)?, store_id(c)?)))?;
        let dir = match open_writable_folder(config, &row.root, &row.path) {
            Ok(dir) => dir,
            Err(reason) => return Ok(Err(reason)),
        };
        let expected = Marker {
            store,
            folder,
            generation: row.generation,
        };
        if let Err(reason) = check_marker(&dir, &expected) {
            return Ok(Err(reason));
        }
        if let Err(reason) = settle_dir_temps(self, folder, &dir)? {
            return Ok(Err(reason));
        }
        let deleting: Vec<KeptFile> = self
            .read(|c| kept_where(c, "k.deletion_id = ?1 AND k.state = 'deleting'", [deletion]))?;
        let mut notes = HashMap::new();
        let mut emptied = BTreeSet::new();
        for file in deleting {
            if let Err(reason) = check_marker(&dir, &expected) {
                return Ok(Err(reason));
            }
            match self.settle_deleting(&dir, &file)? {
                Ok(note) => {
                    if let Some(note) = note {
                        notes.insert(file.id, note);
                    }
                    emptied.insert(file.generation);
                }
                Err(reason) => return Ok(Err(reason)),
            }
        }
        self.remove_emptied(folder, &dir, emptied)?;
        let states: BTreeMap<i64, String> = self.read(|c| {
            let mut stmt = c.prepare("SELECT id, state FROM kept_files WHERE deletion_id = ?1")?;
            let rows = stmt.query_map([deletion], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })?;
        let outcomes: Vec<KeptDeletionOutcome> = plan
            .content
            .files
            .iter()
            .map(|f| match states.get(&f.id).map(String::as_str) {
                Some("deleted") => deleted_outcome(f),
                Some("gone") => gone_outcome(f),
                _ => left(
                    f,
                    notes
                        .remove(&f.id)
                        .unwrap_or_else(|| "the service stopped before it was deleted".into()),
                ),
            })
            .collect();
        self.write(|tx| {
            tx.execute(
                "UPDATE kept_deletions SET finished_at = ?2, outcomes = ?3 WHERE id = ?1",
                params![deletion, now(), to_json(&outcomes)],
            )?;
            Ok(())
        })?;
        Ok(Ok(()))
    }

    /// Settles kept file `file`, which a deletion was deleting when the service stopped, in its folder opened as `dir`:
    /// deleted once it is found in the private folder it is deleted from, or gone from its kept entry; still kept
    /// when it is at its kept entry. Anything else is never deleted. Returns a note for its outcome, if it is still
    /// kept, or why the folder could not be read.
    fn settle_deleting(
        &self,
        dir: &FolderDir,
        file: &KeptFile,
    ) -> Result<std::result::Result<Option<String>, String>> {
        let folder = file.folder;
        let id = Identity::from(file.identity);
        let slot = file.id.to_string();
        let area = match own_dir(self, folder, dir, RETENTION, RETENTION, false)? {
            Ok(Some(area)) => area,
            Ok(None) => {
                self.write(|tx| deleted(tx, file.id))?;
                return Ok(Ok(None));
            }
            Err(reason) => return Ok(Err(reason)),
        };
        let deleting = match open_deleting(self, folder, &area, false)? {
            Ok(deleting) => deleting,
            Err(reason) => return Ok(Err(reason)),
        };
        let name = file.generation.to_string();
        let generation = match own_dir(
            self,
            folder,
            &area,
            &name,
            &generation_dir(file.generation),
            false,
        )? {
            Ok(generation) => generation,
            Err(reason) => return Ok(Err(reason)),
        };
        let kept = |note: &str| -> Result<std::result::Result<Option<String>, String>> {
            self.write(|tx| still_kept(tx, file.id))?;
            Ok(Ok(Some(note.to_owned())))
        };
        if let Some(deleting) = &deleting {
            match deleting.stat(&slot) {
                Err(e) => return Ok(Err(e.to_string())),
                Ok(Some(_)) if is_kept_file(deleting, &slot, id, Some(&file.file)) => {
                    if let Err(e) = deleting.unlink(&slot).and_then(|()| deleting.sync()) {
                        return Ok(Err(format!(
                            "{}/{slot} could not be deleted ({e})",
                            deleting.path().display()
                        )));
                    }
                    self.write(|tx| deleted(tx, file.id))?;
                    return Ok(Ok(None));
                }
                Ok(Some(_)) => {
                    let back = match &generation {
                        Some(generation) => put_back(generation, deleting, &slot, &file.kept_name),
                        None => format!("it is at {RETENTION}/{DELETING}/{slot}"),
                    };
                    return kept(&format!("{MOVED_NOT_THE_FILE}; {back}"));
                }
                Ok(None) => {}
            }
        }
        let Some(generation) = generation else {
            self.write(|tx| deleted(tx, file.id))?;
            return Ok(Ok(None));
        };
        match generation.stat(&file.kept_name) {
            Err(e) => Ok(Err(e.to_string())),
            Ok(None) => {
                self.write(|tx| deleted(tx, file.id))?;
                Ok(Ok(None))
            }
            Ok(Some(_)) if is_kept_file(&generation, &file.kept_name, id, Some(&file.file)) => {
                kept("the service stopped before it was deleted")
            }
            Ok(Some(_)) => kept(NOT_THE_FILE),
        }
    }
}

const NOT_THE_FILE: &str =
    "what is there now is not the file that was kept, so it is never deleted";
const MOVED_NOT_THE_FILE: &str =
    "what was moved to be deleted is not the file that was kept, so it is never deleted";

/// Moves `slot` in the private folder `deleting` back to `kept` in `generation`, without replacing anything, and
/// says where it is.
fn put_back(generation: &FolderDir, deleting: &FolderDir, slot: &str, kept: &str) -> String {
    match deleting
        .rename_new(slot, generation, kept)
        .and_then(|()| generation.sync())
        .and_then(|()| deleting.sync())
    {
        Ok(()) => format!("it was moved back to {kept}"),
        Err(_) => format!("it is at {RETENTION}/{DELETING}/{slot}"),
    }
}

/// Records kept file `id`, which was being deleted, still kept.
fn still_kept(tx: &Transaction, id: i64) -> Result<()> {
    tx.execute(
        "UPDATE kept_files SET state = 'kept', deletion_id = NULL WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

fn deleted_outcome(file: &KeptFile) -> KeptDeletionOutcome {
    KeptDeletionOutcome {
        id: file.id,
        kept_name: file.kept_name.clone(),
        generation: file.generation,
        deleted: true,
        detail: None,
    }
}

fn gone_outcome(file: &KeptFile) -> KeptDeletionOutcome {
    KeptDeletionOutcome {
        id: file.id,
        kept_name: file.kept_name.clone(),
        generation: file.generation,
        deleted: false,
        detail: Some("it is gone, so it is no longer listed as kept".into()),
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
    drop_citations(tx, id)
}

/// Drops what kept file `id` cited, so that pruning no longer holds it.
fn drop_citations(tx: &Transaction, id: i64) -> Result<()> {
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
        "SELECT id, digest, created_at, content, approval_digest, actor, approved_at, finished_at, outcomes,
                recovery_error
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
                recovery_error: r.get(9)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("no deletion {id}")))
}
