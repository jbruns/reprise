//! Builds: carrying out an approved Change Plan in its Managed Folder. Each file is written and verified in the
//! folder's staging directory, then renamed into place without replacing anything, and recorded applied in its own
//! transaction. The marker's generation advances in the transaction that finishes the Build.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use title_select_api::build::{
    BuildDetail, BuildItem, BuildSummary, BuiltFile, ExpectedRom, FileStat, ItemKind, ItemState,
    Marker, PlanItem, STAGING,
};
use title_select_api::{Job, JobKind, JobState};
use title_select_engine::Progress;
use title_select_engine::build::{Source, copy, repack, verify};

use super::blobs::sync_dir;
use super::folders::{
    MARKER_NEW, MarkerState, build_by_id, builds_where, folder_by_id, locate, read_marker,
    store_id, write_marker,
};
use super::jobs::finish_running;
use super::plans::{digest, json, stat, to_json};
use super::{Error, Result, Store, io_error, now};
use crate::config::Config;

/// What a Managed Folder's current Build cites, in `audit_citations` and `selection_citations`.
fn cited_by(folder: i64) -> String {
    format!("managed-folder:{folder}")
}

/// Renames `from` to `to`, failing with `AlreadyExists` rather than replacing anything at `to`.
pub(super) fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    match rename_noreplace(from, to) {
        Some(result) => result,
        None => {
            // The filesystem cannot rename exclusively; nothing else writes to a Managed Folder while a Build runs,
            // so a check first is the best there is.
            if std::fs::symlink_metadata(to).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} already exists", to.display()),
                ));
            }
            std::fs::rename(from, to)
        }
    }
}

#[cfg(unix)]
fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
}

/// An exclusive rename, or `None` when the filesystem or kernel has none.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn rename_noreplace(from: &Path, to: &Path) -> Option<io::Result<()>> {
    let (from, to) = match (c_path(from), c_path(to)) {
        (Ok(from), Ok(to)) => (from, to),
        (Err(e), _) | (_, Err(e)) => return Some(Err(e)),
    };
    // SAFETY: both paths are NUL-terminated strings that outlive the call.
    let done = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    exclusive_outcome(done, &[libc::EINVAL, libc::ENOSYS])
}

#[cfg(target_os = "macos")]
fn rename_noreplace(from: &Path, to: &Path) -> Option<io::Result<()>> {
    let (from, to) = match (c_path(from), c_path(to)) {
        (Ok(from), Ok(to)) => (from, to),
        (Err(e), _) | (_, Err(e)) => return Some(Err(e)),
    };
    // SAFETY: both paths are NUL-terminated strings that outlive the call.
    let done = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    exclusive_outcome(done, &[libc::EINVAL, libc::ENOTSUP])
}

#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos"))]
fn exclusive_outcome(done: libc::c_int, unsupported: &[libc::c_int]) -> Option<io::Result<()>> {
    if done == 0 {
        return Some(Ok(()));
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(code) if unsupported.contains(&code) => None,
        _ => Some(Err(err)),
    }
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
fn rename_noreplace(_from: &Path, _to: &Path) -> Option<io::Result<()>> {
    None
}

/// One file to write, as the approved plan lists it.
struct Work<'a> {
    seq: i64,
    item: &'a PlanItem,
}

/// Writes `item` to `staged`, verifies it, and renames it to its target in `folder`. Returns the placed file.
fn apply(
    item: &PlanItem,
    staged: &Path,
    folder: &Path,
    set_dir: &Path,
    set_root: &Path,
    built_at: SystemTime,
) -> std::result::Result<FileStat, String> {
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
        .collect::<std::result::Result<_, String>>()?;
    match item.kind {
        ItemKind::Copy => {
            copy(&sources[0], staged).map_err(|e| format!("{}: {e}", sources[0].display()))?;
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
            repack(staged, &pairs)?;
        }
        _ => unreachable!("only copies and repacks are written"),
    }
    verify(staged, &item.roms)?;
    let file = File::options()
        .write(true)
        .open(staged)
        .map_err(|e| format!("{}: {e}", staged.display()))?;
    file.set_modified(built_at)
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("{}: {e}", staged.display()))?;
    drop(file);
    let target = folder.join(&item.target);
    rename_new(staged, &target).map_err(|e| match e.kind() {
        io::ErrorKind::AlreadyExists => format!(
            "{} appeared in the folder during the build; it was left as it is",
            item.target
        ),
        _ => format!("{}: {e}", target.display()),
    })?;
    sync_dir(folder).map_err(|e| e.to_string())?;
    let meta = std::fs::metadata(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    Ok(stat(&meta))
}

fn remove_staging(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(io_error(dir, e)),
        _ => Ok(()),
    }
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

/// Ends the Running Build `build` of Job `job` as `state`, and the Job as Failed with `error`.
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

impl Store {
    /// Carries out the approved plan of the Running Build Job `id`, calling `progress` as files are written. The plan
    /// is computed again first and must still have the approved digest; when it does not, the new plan is stored and
    /// the Job Failed. Files are added only: nothing in the folder is replaced or removed. A file whose sources fail
    /// their re-hash, or whose output fails validation, is skipped and the rest go on. Returns the Job as it ends.
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
        let mut staging = None;
        let outcome = self.carry_out(config, id, plan_id, &mut build, &mut staging, progress);
        if let Err(err) = &outcome {
            let message = err.to_string();
            self.write(|tx| end_failed(tx, id, build, &message))?;
        }
        if let Some(staging) = staging {
            remove_staging(&staging)?;
        }
        if let Some(build) = build {
            self.restore_marker(config, build)?;
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
        staging_dir: &mut Option<PathBuf>,
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
        let folder_row = self.read(|c| folder_by_id(c, content.folder))?;
        let folder = locate(config, &folder_row.root, &folder_row.path).map_err(Error::Refused)?;
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

        let staging = folder.canonical.join(STAGING);
        remove_staging(&staging)?;
        std::fs::create_dir(&staging).map_err(|e| io_error(&staging, e))?;
        *staging_dir = Some(staging.clone());
        let built_at = SystemTime::now();
        let total = work.len();
        if let Some(progress) = progress {
            progress(Progress {
                files_done: 0,
                files_total: total,
            });
        }
        for (done, w) in work.iter().enumerate() {
            let staged = staging.join(format!("{}.zip", w.seq));
            let outcome = apply(
                w.item,
                &staged,
                &folder.canonical,
                &set_dir,
                &set_root,
                built_at,
            );
            if outcome.is_err() {
                let _ = std::fs::remove_file(&staged);
            }
            self.write(|tx| {
                match &outcome {
                    Ok(file) => {
                        tx.execute(
                            "UPDATE build_items SET state = 'applied' WHERE build_id = ?1 AND seq = ?2",
                            params![id, w.seq],
                        )?;
                        tx.execute(
                            "INSERT INTO built_files (folder_id, target, game, kind, build_id, selection_id,
                                                      audit_id, dat_revision_id, roms, sources, size, mtime_ns)
                             SELECT ?1, ?2, ?3, ?4, b.id, b.selection_id, b.audit_id, b.dat_revision_id, ?5, ?6,
                                    ?7, ?8
                             FROM builds b WHERE b.id = ?9
                             ON CONFLICT (folder_id, target) DO UPDATE SET
                                 game = excluded.game, kind = excluded.kind, build_id = excluded.build_id,
                                 selection_id = excluded.selection_id, audit_id = excluded.audit_id,
                                 dat_revision_id = excluded.dat_revision_id, roms = excluded.roms,
                                 sources = excluded.sources, size = excluded.size, mtime_ns = excluded.mtime_ns",
                            params![
                                content.folder,
                                w.item.target,
                                w.item.game,
                                w.item.kind.as_str(),
                                to_json(&w.item.roms),
                                to_json(&w.item.sources),
                                file.size as i64,
                                file.mtime_ns as i64,
                                id
                            ],
                        )?;
                    }
                    Err(error) => {
                        tx.execute(
                            "UPDATE build_items SET state = 'failed', error = ?3 WHERE build_id = ?1 AND seq = ?2",
                            params![id, w.seq, error],
                        )?;
                    }
                }
                Ok(())
            })?;
            if let Some(progress) = progress {
                progress(Progress {
                    files_done: done + 1,
                    files_total: total,
                });
            }
        }

        let key = cited_by(content.folder);
        self.write(|tx| {
            tx.execute(
                "UPDATE managed_folders SET generation = ?2, current_build_id = ?3 WHERE id = ?1",
                params![content.folder, generation as i64, id],
            )?;
            tx.execute(
                "UPDATE builds SET state = 'succeeded', finished_at = ?2 WHERE id = ?1",
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
            finish_running(tx, job, JobState::Succeeded, None)?;
            // Last, so that the marker advances only if everything else will commit; a crash between the two
            // leaves the marker one ahead, which recovery puts back.
            write_marker(
                &folder.canonical,
                &Marker {
                    store: store_id(tx)?,
                    folder: content.folder,
                    generation,
                },
            )
        })
    }

    /// Puts the marker of Build `build`'s folder back to the store's generation when it is one ahead because the
    /// Build did not finish, and removes a half-written marker.
    fn restore_marker(&self, config: &Config, build: i64) -> Result<()> {
        let (folder, state) = self.read(|c| {
            let b = build_by_id(c, build)?;
            Ok((folder_by_id(c, b.folder)?, b.state))
        })?;
        if state == title_select_api::build::BuildState::Succeeded {
            return Ok(());
        }
        self.restore_folder(config, folder.id)
    }

    fn restore_folder(&self, config: &Config, id: i64) -> Result<()> {
        let (folder, store) = self.read(|c| Ok((folder_by_id(c, id)?, store_id(c)?)))?;
        let Ok(located) = locate(config, &folder.root, &folder.path) else {
            return Ok(());
        };
        let _ = std::fs::remove_file(located.path.join(MARKER_NEW));
        if let MarkerState::Present(marker) = read_marker(&located.path)
            && marker.store == store
            && marker.folder == folder.id
            && marker.generation == folder.generation + 1
        {
            write_marker(
                &located.path,
                &Marker {
                    generation: folder.generation,
                    ..marker
                },
            )?;
        }
        Ok(())
    }

    /// Run when the service starts, after [`Store::recover_jobs`]: removes each Managed Folder's staging directory,
    /// and puts back a marker an unfinished Build advanced. A folder that cannot be reached is left for its next plan
    /// to report.
    pub fn recover_builds(&self, config: &Config) -> Result<()> {
        let folders: Vec<i64> = self.read(|c| {
            let mut stmt = c.prepare("SELECT id FROM managed_folders ORDER BY id")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })?;
        for id in folders {
            let folder = self.read(|c| folder_by_id(c, id))?;
            if let Ok(located) = locate(config, &folder.root, &folder.path) {
                remove_staging(&located.path.join(STAGING))?;
            }
            self.restore_folder(config, id)?;
        }
        Ok(())
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
