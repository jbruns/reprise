//! Builds: carrying out an approved Change Plan in its Managed Folder. The folder is opened once without following a
//! symlink, and every change is made through that handle (see [`FolderDir`]). Each file is written and verified in
//! the folder's staging directory, its SHA-256 recorded as the item's publish intent, the marker checked again, and
//! the file renamed into place without replacing anything; it is recorded applied in its own transaction. The
//! Build's finalize intent is committed before the marker advances, and the marker advances before the transaction
//! that finishes the Build. Recovery changes a folder only for a Build the store records as unfinished, and only once
//! the folder's marker shows it is that folder.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};
use title_select_api::build::{
    BuildDetail, BuildItem, BuildSummary, BuiltFile, ExpectedRom, FileStat, ItemKind, ItemState,
    Marker, PlanItem, STAGING,
};
use title_select_api::{Job, JobKind, JobState};
use title_select_engine::Progress;
use title_select_engine::build::{Source, copy, repack, verify};

use super::folder_dir::{FolderDir, file_stat};
use super::folders::{
    MARKER_NEW, MarkerState, build_by_id, builds_where, check_marker, folder_by_id, open_folder,
    read_marker, store_id, write_marker,
};
use super::jobs::finish_running;
use super::plans::{digest, json, stat, to_json};
use super::{Error, Fault, Result, Store, now};
use crate::config::Config;

/// What a Managed Folder's current Build cites, in `audit_citations` and `selection_citations`.
fn cited_by(folder: i64) -> String {
    format!("managed-folder:{folder}")
}

/// One file to write, as the approved plan lists it.
struct Work<'a> {
    seq: i64,
    item: &'a PlanItem,
}

/// Why a file was not built: its sources or its output, which fails only the item, or the folder, which stops the
/// Build.
enum Failure {
    Item(String),
    Folder(String),
}

/// A file written and verified in the staging directory, with its SHA-256 and size.
struct Staged {
    sha256: String,
    size: u64,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 and size of `file`, read from its start.
fn sha256_of(mut file: &File) -> io::Result<(String, u64)> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 1 << 16];
    let mut size = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((hex(&hasher.finalize()), size))
}

/// Writes `item` to the new file `name` in `staging` and verifies it.
fn stage(
    item: &PlanItem,
    staging: &FolderDir,
    name: &str,
    set_dir: &Path,
    set_root: &Path,
    built_at: SystemTime,
) -> std::result::Result<Staged, Failure> {
    let sources: Vec<PathBuf> = item
        .sources
        .iter()
        .map(|s| {
            let path = set_dir.join(&s.path);
            let real = path
                .canonicalize()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            if real.starts_with(set_root) {
                Ok(real)
            } else {
                Err(format!("{}: resolves outside its root", path.display()))
            }
        })
        .collect::<std::result::Result<_, String>>()
        .map_err(Failure::Item)?;
    let mut file = staging
        .create_new(name)
        .map_err(|e| Failure::Folder(e.to_string()))?;
    match item.kind {
        ItemKind::Copy => {
            copy(&sources[0], &mut file)
                .map_err(|e| Failure::Item(format!("{}: {e}", sources[0].display())))?;
        }
        ItemKind::Repack => {
            let pairs: Vec<(&ExpectedRom, Source)> = item
                .roms
                .iter()
                .map(|rom| {
                    let i = item
                        .sources
                        .iter()
                        .position(|s| s.rom == rom.name)
                        .expect("a writable item has a source for every ROM");
                    (
                        rom,
                        Source {
                            path: &sources[i],
                            member: item.sources[i].member.as_deref(),
                        },
                    )
                })
                .collect();
            repack(&file, &pairs).map_err(Failure::Item)?;
        }
        _ => unreachable!("only copies and repacks are written"),
    }
    verify(&file, &item.roms).map_err(Failure::Item)?;
    let staged = staging.path().join(name);
    file.set_modified(built_at)
        .and_then(|()| file.sync_all())
        .map_err(|e| Failure::Folder(format!("{}: {e}", staged.display())))?;
    let (sha256, size) =
        sha256_of(&file).map_err(|e| Failure::Folder(format!("{}: {e}", staged.display())))?;
    Ok(Staged { sha256, size })
}

/// Removes what a Build put in the folder's staging directory, then the directory. Anything but regular files in it
/// is left, and the error says so.
fn clear_staging(dir: &FolderDir) -> std::result::Result<(), String> {
    let Some(st) = dir.stat(STAGING).map_err(|e| e.to_string())? else {
        return Ok(());
    };
    if FolderDir::kind(&st) != rustix::fs::FileType::Directory {
        return Err(format!(
            "{}/{STAGING} is not a directory; it was left as it is",
            dir.path().display()
        ));
    }
    let staging = dir.sub(STAGING).map_err(|e| e.to_string())?;
    for (name, _) in staging.entries().map_err(|e| e.to_string())? {
        match staging.stat(&name).map_err(|e| e.to_string())? {
            Some(st) if FolderDir::kind(&st) == rustix::fs::FileType::RegularFile => {
                staging.unlink(&name).map_err(|e| e.to_string())?;
            }
            None => {}
            Some(_) => {
                return Err(format!(
                    "{}/{name} is not a file a build wrote; the staging directory was left as it is",
                    staging.path().display()
                ));
            }
        }
    }
    dir.rmdir(STAGING)
        .and_then(|()| dir.sync())
        .map_err(|e| e.to_string())
}

/// Records the item `seq` of Build `build` applied and its file built, with `file`'s stat.
fn record_applied(
    tx: &Transaction,
    build: i64,
    folder: i64,
    seq: i64,
    item: &PlanItem,
    file: &FileStat,
) -> Result<()> {
    tx.execute(
        "UPDATE build_items SET state = 'applied', error = NULL WHERE build_id = ?1 AND seq = ?2",
        params![build, seq],
    )?;
    tx.execute(
        "INSERT INTO built_files (folder_id, target, game, kind, build_id, selection_id, audit_id, dat_revision_id,
                                  roms, sources, size, mtime_ns)
         SELECT ?1, ?2, ?3, ?4, b.id, b.selection_id, b.audit_id, b.dat_revision_id, ?5, ?6, ?7, ?8
         FROM builds b WHERE b.id = ?9
         ON CONFLICT (folder_id, target) DO UPDATE SET
             game = excluded.game, kind = excluded.kind, build_id = excluded.build_id,
             selection_id = excluded.selection_id, audit_id = excluded.audit_id,
             dat_revision_id = excluded.dat_revision_id, roms = excluded.roms, sources = excluded.sources,
             size = excluded.size, mtime_ns = excluded.mtime_ns",
        params![
            folder,
            item.target,
            item.game,
            item.kind.as_str(),
            to_json(&item.roms),
            to_json(&item.sources),
            file.size as i64,
            file.mtime_ns as i64,
            build
        ],
    )?;
    Ok(())
}

fn set_item(tx: &Transaction, build: i64, seq: i64, state: ItemState, error: &str) -> Result<()> {
    tx.execute(
        "UPDATE build_items SET state = ?3, error = ?4 WHERE build_id = ?1 AND seq = ?2",
        params![build, seq, state.as_str(), error],
    )?;
    Ok(())
}

fn build_items(c: &Connection, build: i64) -> Result<Vec<BuildItem>> {
    let mut stmt = c.prepare(
        "SELECT target, game, kind, state, error FROM build_items WHERE build_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt.query_map([build], |r| {
        let kind: String = r.get(2)?;
        let state: String = r.get(3)?;
        Ok(BuildItem {
            target: r.get(0)?,
            game: r.get(1)?,
            kind: ItemKind::parse(&kind).unwrap_or(ItemKind::Repack),
            state: ItemState::parse(&state).unwrap_or(ItemState::Failed),
            error: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Ends the Running Build `build` of Job `job` as Failed, and the Job as Failed with `error`.
fn end_failed(tx: &Transaction, job: i64, build: Option<i64>, error: &str) -> Result<()> {
    if let Some(build) = build {
        tx.execute(
            "UPDATE builds SET state = 'failed', finished_at = ?2 WHERE id = ?1 AND state = 'running'",
            params![build, now()],
        )?;
    }
    match finish_running(tx, job, JobState::Failed, Some(error)) {
        Err(Error::Io(_)) | Ok(()) => Ok(()),
        Err(err) => Err(err),
    }
}

/// An item that was being renamed into place when its Build stopped.
struct Publishing {
    seq: i64,
    target: String,
    sha256: Option<String>,
    size: Option<i64>,
}

/// What recovery reads about an unfinished Build.
struct Unfinished {
    plan: i64,
    generation: u64,
    finalizing: bool,
    publishing: Vec<Publishing>,
}

impl Store {
    /// Carries out the approved plan of the Running Build Job `id`, calling `progress` as files are written. The plan
    /// is computed again first and must still have the approved digest; when it does not, the new plan is stored and
    /// the Job Failed. Files are added only: nothing in the folder is replaced or removed. A file whose sources fail
    /// their re-hash, or whose output fails validation, is skipped and the rest go on; anything wrong with the folder
    /// itself, its marker included, stops the Build, which then fails without advancing the marker. Returns the Job
    /// as it ends.
    pub fn run_build_job(
        &self,
        config: &Config,
        id: i64,
        progress: Option<&(dyn Fn(Progress) + Sync)>,
    ) -> Result<Job> {
        let (kind, state, plan_id) = self.read(|c| {
            c.query_row(
                "SELECT kind, state, plan_id FROM jobs WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("no job {id}")))
        })?;
        if kind != JobKind::Build.as_str() {
            return Err(Error::Refused(format!("job {id} is not a build")));
        }
        if state != JobState::Running.as_str() {
            return Err(Error::Refused(format!("job {id} is {state}, not running")));
        }
        let plan_id = plan_id.ok_or_else(|| Error::Io(format!("build job {id} has no plan")))?;
        let mut build = None;
        let outcome = self.carry_out(config, id, plan_id, &mut build, progress);
        if let Err(err) = &outcome {
            let message = err.to_string();
            self.write(|tx| end_failed(tx, id, build, &message))?;
            if let Some(build) = build {
                self.recover_build(config, build)?;
            }
        }
        let job = self.job(id)?;
        if job.state == JobState::Succeeded {
            self.retain_blobs()?;
        }
        Ok(job)
    }

    fn carry_out(
        &self,
        config: &Config,
        job: i64,
        plan_id: i64,
        build_id: &mut Option<i64>,
        progress: Option<&(dyn Fn(Progress) + Sync)>,
    ) -> Result<()> {
        let plan = self.plan(plan_id)?;
        let approved = plan
            .approval
            .as_ref()
            .ok_or_else(|| Error::Io(format!("plan {plan_id} is not approved")))?;
        let content = &plan.content;
        let again = self.compute_plan(config, content.folder, content.selection)?;
        if digest(&again.content) != approved.digest {
            let newer = self.write(|tx| super::plans::insert_plan(tx, &again))?;
            return Err(Error::Conflict(format!(
                "the plan changed since you approved it; nothing was written. Review plan {} instead",
                newer.id
            )));
        }
        let audit = content
            .audit
            .ok_or_else(|| Error::Io(format!("plan {plan_id} has no audit")))?;
        let (folder_row, store) =
            self.read(|c| Ok((folder_by_id(c, content.folder)?, store_id(c)?)))?;
        let dir =
            open_folder(config, &folder_row.root, &folder_row.path).map_err(Error::Refused)?;
        let expected = Marker {
            store,
            folder: content.folder,
            generation: content.generation,
        };
        check_marker(&dir, &expected).map_err(Error::Refused)?;
        for leftover in [STAGING, MARKER_NEW] {
            if dir
                .stat(leftover)
                .map_err(|e| Error::Io(e.to_string()))?
                .is_some()
            {
                return Err(Error::Refused(format!(
                    "{} holds {leftover}, which no build of this store left there; nothing was written",
                    dir.path().display()
                )));
            }
        }
        let (set_dir, set_root) = self.set_folder(config, folder_row.source_set_id)?;
        let work: Vec<Work> = content
            .items
            .iter()
            .filter(|i| i.kind.writes())
            .enumerate()
            .map(|(seq, item)| Work {
                seq: seq as i64,
                item,
            })
            .collect();
        let generation = content.generation + 1;
        let id = self.write(|tx| {
            let revision: i64 = tx.query_row(
                "SELECT dat_revision_id FROM selections WHERE id = ?1",
                [content.selection],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT INTO builds (folder_id, plan_id, job_id, selection_id, audit_id, dat_revision_id, state,
                                     generation, started_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running', ?7, ?8)",
                params![
                    content.folder,
                    plan_id,
                    job,
                    content.selection,
                    audit,
                    revision,
                    generation as i64,
                    now()
                ],
            )?;
            let id = tx.last_insert_rowid();
            for w in &work {
                tx.execute(
                    "INSERT INTO build_items (build_id, seq, target, game, kind, state)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
                    params![id, w.seq, w.item.target, w.item.game, w.item.kind.as_str()],
                )?;
            }
            Ok(id)
        })?;
        *build_id = Some(id);

        let folder_io = |e: io::Error| Error::Io(e.to_string());
        dir.mkdir(STAGING).map_err(folder_io)?;
        let staging = dir.sub(STAGING).map_err(folder_io)?;
        let built_at = SystemTime::now();
        let total = work.len();
        if let Some(progress) = progress {
            progress(Progress {
                files_done: 0,
                files_total: total,
            });
        }
        for (done, w) in work.iter().enumerate() {
            let name = format!("{}.zip", w.seq);
            let target = &w.item.target;
            match stage(w.item, &staging, &name, &set_dir, &set_root, built_at) {
                Err(Failure::Item(error)) => {
                    match staging.unlink(&name) {
                        Err(e) if e.kind() != io::ErrorKind::NotFound => {
                            return Err(Error::Io(e.to_string()));
                        }
                        _ => {}
                    }
                    self.write(|tx| set_item(tx, id, w.seq, ItemState::Failed, &error))?;
                }
                Err(Failure::Folder(error)) => {
                    self.write(|tx| set_item(tx, id, w.seq, ItemState::Failed, &error))?;
                    return Err(Error::Io(error));
                }
                Ok(staged) => {
                    self.write(|tx| {
                        tx.execute(
                            "UPDATE build_items SET state = 'publishing', sha256 = ?3, size = ?4
                             WHERE build_id = ?1 AND seq = ?2",
                            params![id, w.seq, staged.sha256, staged.size as i64],
                        )?;
                        Ok(())
                    })?;
                    let renamed = check_marker(&dir, &expected).and_then(|()| {
                        staging
                            .rename_new(&name, &dir, target)
                            .map_err(|e| match e.kind() {
                                io::ErrorKind::AlreadyExists => format!(
                                    "{target} appeared in the folder during the build; it was left as it is"
                                ),
                                _ => e.to_string(),
                            })
                    });
                    if let Err(error) = renamed {
                        self.write(|tx| set_item(tx, id, w.seq, ItemState::Failed, &error))?;
                        return Err(Error::Refused(error));
                    }
                    // From here until the item is recorded applied, recovery settles it from its publish intent.
                    dir.sync().map_err(folder_io)?;
                    self.fault_point(Fault::AfterPublish(w.seq));
                    let st = dir.stat(target).map_err(folder_io)?.ok_or_else(|| {
                        Error::Io(format!("{target} vanished once it was in place"))
                    })?;
                    self.write(|tx| {
                        record_applied(tx, id, content.folder, w.seq, w.item, &file_stat(&st))
                    })?;
                }
            }
            if let Some(progress) = progress {
                progress(Progress {
                    files_done: done + 1,
                    files_total: total,
                });
            }
        }
        drop(staging);
        clear_staging(&dir).map_err(Error::Io)?;

        check_marker(&dir, &expected).map_err(Error::Refused)?;
        self.write(|tx| {
            tx.execute(
                "UPDATE builds SET finalizing_at = ?2 WHERE id = ?1",
                params![id, now()],
            )?;
            Ok(())
        })?;
        write_marker(
            &dir,
            &Marker {
                generation,
                ..expected.clone()
            },
        )?;
        self.fault_point(Fault::AfterMarker);
        let key = cited_by(content.folder);
        self.write(|tx| {
            tx.execute(
                "UPDATE managed_folders SET generation = ?2, current_build_id = ?3 WHERE id = ?1",
                params![content.folder, generation as i64, id],
            )?;
            tx.execute(
                "UPDATE builds SET state = 'succeeded', finished_at = ?2, cleaned_at = ?2 WHERE id = ?1",
                params![id, now()],
            )?;
            tx.execute("DELETE FROM audit_citations WHERE cited_by = ?1", [&key])?;
            tx.execute(
                "INSERT INTO audit_citations (audit_id, cited_by) VALUES (?1, ?2)",
                params![audit, key],
            )?;
            tx.execute(
                "DELETE FROM selection_citations WHERE cited_by = ?1",
                [&key],
            )?;
            tx.execute(
                "INSERT INTO selection_citations (selection_id, cited_by) VALUES (?1, ?2)",
                params![content.selection, key],
            )?;
            finish_running(tx, job, JobState::Succeeded, None)
        })
    }

    /// Cleans up the folder of the unfinished Build `build`: settles each item it was renaming into place, removes
    /// its staging directory and a half-written marker, and puts back a marker it advanced. Nothing is changed unless
    /// the folder's marker shows it is the store's folder, at the store's generation or, with the Build's finalize
    /// intent recorded, at the Build's. Returns why the folder was left as it is, if it was; the Build then still
    /// needs recovery, and plans for its folder are blocked.
    fn recover_build(&self, config: &Config, build: i64) -> Result<Option<String>> {
        let (unfinished, folder, store) = self.read(|c| {
            let (folder, plan, generation, finalizing): (i64, i64, i64, bool) = c.query_row(
                "SELECT folder_id, plan_id, generation, finalizing_at IS NOT NULL FROM builds WHERE id = ?1",
                [build],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            let mut stmt = c.prepare(
                "SELECT seq, target, sha256, size FROM build_items
                 WHERE build_id = ?1 AND state = 'publishing' ORDER BY seq",
            )?;
            let publishing = stmt
                .query_map([build], |r| {
                    Ok(Publishing {
                        seq: r.get(0)?,
                        target: r.get(1)?,
                        sha256: r.get(2)?,
                        size: r.get(3)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            Ok((
                Unfinished {
                    plan,
                    generation: generation as u64,
                    finalizing,
                    publishing,
                },
                folder_by_id(c, folder)?,
                store_id(c)?,
            ))
        })?;
        let outcome = self.clean_folder(config, build, &unfinished, &folder, store)?;
        let error = outcome.as_ref().err().cloned();
        self.write(|tx| {
            tx.execute(
                "UPDATE builds SET cleaned_at = CASE WHEN ?2 IS NULL THEN ?3 END, recovery_error = ?2 WHERE id = ?1",
                params![build, error, now()],
            )?;
            Ok(())
        })?;
        Ok(error)
    }

    fn clean_folder(
        &self,
        config: &Config,
        build: i64,
        unfinished: &Unfinished,
        folder: &super::folders::FolderRow,
        store: String,
    ) -> Result<std::result::Result<(), String>> {
        let dir = match open_folder(config, &folder.root, &folder.path) {
            Ok(dir) => dir,
            Err(reason) => return Ok(Err(reason)),
        };
        let rewind = match read_marker(&dir) {
            MarkerState::Present(m) if m.store == store && m.folder == folder.id => {
                if m.generation == folder.generation {
                    false
                } else if m.generation == folder.generation + 1
                    && unfinished.generation == m.generation
                    && unfinished.finalizing
                {
                    true
                } else {
                    return Ok(Err(format!(
                        "its marker is at build generation {}, and the store is at {} with no record of build {build} \
                         advancing it; the folder was left as it is",
                        m.generation, folder.generation
                    )));
                }
            }
            MarkerState::Present(_) => {
                return Ok(Err(
                    "its marker is for another folder or another store; the folder was left as it is".into(),
                ));
            }
            MarkerState::Absent => {
                return Ok(Err("it has no marker; the folder was left as it is".into()));
            }
            MarkerState::Unreadable(e) => {
                return Ok(Err(format!(
                    "its marker cannot be read ({e}); the folder was left as it is"
                )));
            }
        };
        let expected = Marker {
            store,
            folder: folder.id,
            generation: if rewind {
                folder.generation + 1
            } else {
                folder.generation
            },
        };

        match dir.stat(MARKER_NEW) {
            Ok(None) => {}
            Ok(Some(st)) if FolderDir::kind(&st) == rustix::fs::FileType::RegularFile => {
                if let Err(e) = dir.unlink(MARKER_NEW) {
                    return Ok(Err(e.to_string()));
                }
            }
            Ok(Some(_)) => {
                return Ok(Err(format!(
                    "{MARKER_NEW} is not a file a build wrote; the folder was left as it is"
                )));
            }
            Err(e) => return Ok(Err(e.to_string())),
        }

        if !unfinished.publishing.is_empty() {
            let items = self.plan(unfinished.plan)?.content.items;
            for p in &unfinished.publishing {
                let item = items
                    .iter()
                    .find(|i| i.kind.writes() && i.target == p.target)
                    .ok_or_else(|| {
                        Error::Io(format!("build {build}: no plan item for {}", p.target))
                    })?;
                if let Err(reason) = check_marker(&dir, &expected) {
                    return Ok(Err(reason));
                }
                let found = match dir.open_file(&p.target) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                    Err(e) => Some(Err(e.to_string())),
                    Ok(file) => Some(
                        sha256_of(&file)
                            .and_then(|hash| Ok((hash, stat(&file.metadata()?))))
                            .map_err(|e| e.to_string()),
                    ),
                };
                self.write(|tx| match found {
                    None => set_item(
                        tx,
                        build,
                        p.seq,
                        ItemState::Failed,
                        "the build stopped before the file was put in place",
                    ),
                    Some(Ok(((sha256, size), file)))
                        if Some(&sha256) == p.sha256.as_ref() && Some(size as i64) == p.size =>
                    {
                        record_applied(tx, build, folder.id, p.seq, item, &file)
                    }
                    Some(Ok(_)) => set_item(
                        tx,
                        build,
                        p.seq,
                        ItemState::Uncertain,
                        &format!(
                            "the build stopped while putting {} in place, and the file there now is not the one it \
                             wrote; it was left as it is",
                            p.target
                        ),
                    ),
                    Some(Err(e)) => set_item(
                        tx,
                        build,
                        p.seq,
                        ItemState::Uncertain,
                        &format!(
                            "the build stopped while putting {} in place, and what is there now cannot be checked \
                             ({e}); it was left as it is",
                            p.target
                        ),
                    ),
                })?;
            }
        }

        if let Err(reason) = clear_staging(&dir) {
            return Ok(Err(reason));
        }
        if rewind {
            if let Err(reason) = check_marker(&dir, &expected) {
                return Ok(Err(reason));
            }
            write_marker(
                &dir,
                &Marker {
                    generation: folder.generation,
                    ..expected
                },
            )?;
        }
        Ok(Ok(()))
    }

    /// Run when the service starts, after [`Store::recover_jobs`]: cleans up the folder of each Build that ended
    /// without its folder being cleaned up. A folder that is not the store's, or
    /// cannot be reached, is left as it is; returns, for each, the Build and why.
    pub fn recover_builds(&self, config: &Config) -> Result<Vec<(i64, String)>> {
        let builds: Vec<i64> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT id FROM builds WHERE cleaned_at IS NULL AND state IN ('failed', 'interrupted') ORDER BY id",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })?;
        let mut blocked = Vec::new();
        for id in builds {
            if let Some(reason) = self.recover_build(config, id)? {
                blocked.push((id, reason));
            }
        }
        Ok(blocked)
    }

    pub fn build(&self, id: i64) -> Result<BuildDetail> {
        self.read(|c| {
            let tx = c.unchecked_transaction()?;
            Ok(BuildDetail {
                build: build_by_id(&tx, id)?,
                items: build_items(&tx, id)?,
            })
        })
    }

    /// The Build of Job `job`, if it started.
    pub fn build_of_job(&self, job: i64) -> Result<Option<BuildSummary>> {
        self.read(|c| Ok(builds_where(c, "b.job_id = ?1", [job])?.into_iter().next()))
    }

    /// The files Builds wrote into Managed Folder `folder`, by name.
    pub fn built_files(&self, folder: i64) -> Result<Vec<BuiltFile>> {
        self.read(|c| {
            folder_by_id(c, folder)?;
            let mut stmt = c.prepare(
                "SELECT f.target, f.game, f.kind, f.build_id, f.selection_id, f.audit_id, r.sha256, f.sources,
                        f.roms, f.size, f.mtime_ns
                 FROM built_files f JOIN dat_revisions r ON r.id = f.dat_revision_id
                 WHERE f.folder_id = ?1 ORDER BY f.target",
            )?;
            let rows = stmt.query_map([folder], |r| {
                let kind: String = r.get(2)?;
                Ok(BuiltFile {
                    target: r.get(0)?,
                    game: r.get(1)?,
                    kind: ItemKind::parse(&kind).unwrap_or(ItemKind::Repack),
                    build: r.get(3)?,
                    selection: r.get(4)?,
                    audit: r.get(5)?,
                    dat_revision: r.get(6)?,
                    sources: json(r, 7)?,
                    roms: json(r, 8)?,
                    file: FileStat {
                        size: r.get::<_, i64>(9)? as u64,
                        mtime_ns: r.get::<_, i64>(10)? as i128,
                    },
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }
}
