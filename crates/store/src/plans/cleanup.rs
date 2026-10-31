//! Planning a cleanup: what to do with the drift a check finds in a Managed Folder, each action one you chose for one
//! entry, kept file or built file. A cleanup is planned against the folder's current Build, its Selection and DAT
//! Revision, and lists every entry Title Select did not build as the plan's check found it, chosen or not, so that
//! its digest binds the folder as it was planned against: approving it, and carrying it out, compute it again and
//! refuse it when anything it lists changed. Planning only reads the folder.

use std::collections::{BTreeSet, HashMap, HashSet};

use rusqlite::{OptionalExtension, params};
use title_select_api::build::{
    BlockerKind, CheckSummary, Cleanup, CleanupAction, CleanupEntry, CleanupRepack, CleanupRestore,
    DriftKind, ExpectedRom, FileOutcome, FolderCheck, KeptFile, Plan, PlanChoices, PlanCleanup,
    PlanContent, PlanSource, RETENTION, RestoreKind, invalid_name,
};
use title_select_engine::build::{GameIndex, Probe, holds};
use title_select_engine::dat::Dat;

use super::{Blocker, Folder, Planned, blocker, check_folder, insert_plan, load, short, to_json};
use crate::checks::{Seen, reuse, store_check};
use crate::config::Config;
use crate::folder_dir::{FolderDir, Identity, file_stat};
use crate::folders::{folder_by_id, is_own, open_folder};
use crate::kept::{kept_where, open_retention};
use crate::{Error, Result, Store};

/// The ROMs the DAT lists for `game`, in DAT order.
fn roms_of(dat: &Dat, game: &str) -> Vec<ExpectedRom> {
    let Some(i) = dat.games.iter().position(|g| g == game) else {
        return Vec::new();
    };
    dat.roms
        .iter()
        .filter(|r| r.game == i)
        .map(|rom| ExpectedRom {
            name: rom.name.clone(),
            size: rom.size,
            crc: rom.crc.clone(),
            md5: rom.md5.clone(),
            sha1: rom.sha1.clone(),
            sha256: rom.sha256.clone(),
        })
        .collect()
}

fn refuse(target: &str, message: String) -> Blocker {
    Blocker {
        target: Some(target.to_owned()),
        ..blocker(BlockerKind::Cleanup, message)
    }
}

/// The selected game whose ROMs `probe` holds exactly, under their names: its name, its ROMs and its file name.
fn selected_game(
    probe: &Probe,
    index: &GameIndex,
    dat: &Dat,
    selected: &HashMap<String, String>,
) -> std::result::Result<(String, Vec<ExpectedRom>, String), String> {
    let Some(game) = index.game_of(probe) else {
        return Err("it holds no game the DAT revision lists".into());
    };
    let target = format!("{game}.zip");
    if selected.get(&target.to_lowercase()).map(String::as_str) != Some(game) {
        return Err(format!(
            "it holds {game}, which the selection does not select"
        ));
    }
    let roms = roms_of(dat, game);
    if !holds(probe, &roms) {
        return Err(format!(
            "it holds {game}'s ROMs, but not exactly, under the names the DAT gives them"
        ));
    }
    if let Some(why) = invalid_name(&target) {
        return Err(why);
    }
    Ok((game.to_owned(), roms, target))
}

/// The names in the folder a cleanup may put an entry at, and those it claims as it is planned.
struct Names<'a> {
    folder: &'a Folder,
    /// Lowercased names of built files.
    built: HashSet<String>,
    /// Lowercased names of entries the cleanup keeps aside, which are free once they are.
    freed: HashSet<String>,
    claimed: HashSet<String>,
}

impl Names<'_> {
    /// Claims `name` for an entry the cleanup puts there, or says why it cannot: something else is there, in any
    /// case, or another of the cleanup's actions puts something there.
    fn claim(&mut self, name: &str) -> std::result::Result<(), String> {
        let lower = name.to_lowercase();
        if is_own(name) {
            return Err(format!("{name} is a name Title Select keeps for itself"));
        }
        if self.claimed.contains(&lower) {
            return Err(format!(
                "another of the cleanup's actions puts a file at {name}"
            ));
        }
        if self.built.contains(&lower)
            || (self.folder.files.contains_key(&lower) && !self.freed.contains(&lower))
        {
            return Err(format!("{name} is taken in the folder"));
        }
        self.claimed.insert(lower);
        Ok(())
    }
}

/// What was read of a kept file, with the status change time it was read at, if it was.
type KeptProbe = Option<(i64, Probe)>;

/// How a kept file is put back, and what was read of it, if it was read now.
type Restore = (CleanupRestore, Option<(Seen, Probe)>);

impl Store {
    /// Computes the cleanup of Managed Folder `folder` that `choices` asks for, against its current Build.
    pub(crate) fn compute_cleanup(
        &self,
        config: &Config,
        folder: i64,
        choices: &PlanCleanup,
    ) -> Result<Planned> {
        type Kept = Vec<(i64, Option<KeptFile>, KeptProbe)>;
        type Sources = HashMap<String, Vec<PlanSource>>;
        let (inputs, build, audit, kept, sources): (_, i64, i64, Kept, Sources) = self.read(|c| {
            let tx = c.unchecked_transaction()?;
            let row = folder_by_id(&tx, folder)?;
            let Some(build) = row.current_build else {
                return Err(Error::Refused(format!(
                    "managed folder {folder} has no build yet; build into it first, then clean up what a check \
                     finds"
                )));
            };
            let (selection, audit): (i64, i64) = tx.query_row(
                "SELECT selection_id, audit_id FROM builds WHERE id = ?1",
                [build],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let inputs = load(&tx, folder, selection, &PlanChoices::default())?;
            let mut kept = Vec::new();
            for &id in &choices.restore {
                let found = kept_where(
                    &tx,
                    "k.id = ?1 AND k.folder_id = ?2 AND k.state = 'kept'",
                    params![id, folder],
                )?;
                let cache: Option<(Option<String>, Option<i64>)> = tx
                    .query_row(
                        "SELECT probe, probe_ctime_ns FROM kept_files WHERE id = ?1",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                let cached = match cache {
                    Some((Some(probe), Some(ctime))) => serde_json::from_str::<Probe>(&probe)
                        .ok()
                        .map(|p| (ctime, p)),
                    _ => None,
                };
                kept.push((id, found.into_iter().next(), cached));
            }
            let mut sources = HashMap::new();
            for target in &choices.repack {
                let found: Option<String> = tx
                    .query_row(
                        "SELECT sources FROM built_files WHERE folder_id = ?1 AND target = ?2",
                        params![folder, target],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(found) = found.and_then(|s| serde_json::from_str(&s).ok()) {
                    sources.insert(target.clone(), found);
                }
            }
            Ok((inputs, build, audit, kept, sources))
        })?;
        let dat = if inputs.revision.blob_present {
            Some(Dat::read(&self.blob_path(&inputs.revision.sha256))?)
        } else {
            None
        };
        let changes = !choices.restore.is_empty()
            || !choices.repack.is_empty()
            || choices
                .entries
                .values()
                .any(|a| matches!(a, CleanupAction::KeepAside | CleanupAction::Adopt));
        let mut blockers = Vec::new();
        let found = check_folder(config, &inputs, dat.as_ref(), &mut blockers);
        // Acknowledging writes nothing to the folder, so it needs no writable root.
        if !changes {
            blockers.retain(|b| b.kind != BlockerKind::ReadOnlyRoot);
        }
        let needs_dat = !choices.restore.is_empty()
            || choices.entries.values().any(|a| *a == CleanupAction::Adopt);
        if dat.is_none() && needs_dat {
            blockers.push(blocker(
                BlockerKind::RevisionMismatch,
                format!(
                    "DAT revision {} is no longer kept, so nothing can be adopted or put back; import its file ({}) \
                     again",
                    short(&inputs.revision.sha256),
                    inputs.revision.file
                ),
            ));
        }
        let index = dat.as_ref().map(GameIndex::new);

        let mut entries = Vec::new();
        let mut restores = Vec::new();
        let mut repacks = Vec::new();
        let mut probed = Vec::new();
        match found.as_ref().and_then(|f| f.scan.as_ref().map(|s| (f, s))) {
            None => {
                for name in choices.entries.keys() {
                    blockers.push(refuse(name, format!("{name} could not be checked")));
                }
            }
            Some((folder_found, scan)) => {
                let mut names = Names {
                    folder: folder_found,
                    built: inputs.built.keys().map(|t| t.to_lowercase()).collect(),
                    freed: scan
                        .drift
                        .iter()
                        .filter(|d| choices.entries.get(&d.name) == Some(&CleanupAction::KeepAside))
                        .map(|d| d.name.to_lowercase())
                        .collect(),
                    claimed: HashSet::new(),
                };
                let drift: HashSet<&str> = scan.drift.iter().map(|d| d.name.as_str()).collect();
                for name in choices.entries.keys() {
                    if !drift.contains(name.as_str()) {
                        blockers.push(refuse(
                            name,
                            format!(
                                "{name} is not something in the folder that Title Select did not build, as it is \
                                 now"
                            ),
                        ));
                    }
                }
                for d in &scan.drift {
                    let Some(f) = folder_found
                        .files
                        .get(&d.name.to_lowercase())
                        .filter(|f| f.name == d.name)
                    else {
                        continue;
                    };
                    let probe = scan
                        .probes
                        .get(&d.name)
                        .filter(|(seen, _)| seen.identity == f.identity && seen.file == f.file)
                        .map(|(_, p)| p);
                    let mut entry = CleanupEntry {
                        name: d.name.clone(),
                        kind: d.kind,
                        game: d.game.clone(),
                        file: f.file.clone(),
                        identity: f.identity.into(),
                        sha256: probe.map(|p| p.sha256.clone()),
                        action: choices.entries.get(&d.name).copied(),
                        target: None,
                        roms: Vec::new(),
                        torrentzip: probe.is_none_or(|p| p.torrentzip),
                    };
                    let why = match entry.action {
                        None => None,
                        Some(CleanupAction::KeepAside) => {
                            (!f.regular && d.kind != DriftKind::Directory).then(|| {
                                format!(
                                    "{} is neither a file nor a folder, so it cannot be kept aside",
                                    d.name
                                )
                            })
                        }
                        Some(CleanupAction::Acknowledge) => d.conflict.then(|| {
                            format!(
                                "{} is at a selected game's file name; adopt it or keep it aside instead",
                                d.name
                            )
                        }),
                        Some(CleanupAction::Adopt) => {
                            let adoptable = match (f.regular, probe, &index, &dat) {
                                (false, ..) => Err("only a file can be adopted".to_owned()),
                                (true, None, ..) => Err("the check could not read it".to_owned()),
                                (true, Some(p), Some(index), Some(dat)) => {
                                    selected_game(p, index, dat, &inputs.selected)
                                }
                                (true, Some(_), ..) => {
                                    Err("the DAT revision is no longer kept".to_owned())
                                }
                            };
                            match adoptable {
                                Err(why) => Some(format!("{} cannot be adopted: {why}", d.name)),
                                Ok((game, roms, target)) => {
                                    let claimed = if target == d.name {
                                        names.claimed.insert(target.to_lowercase());
                                        Ok(())
                                    } else {
                                        names.claim(&target)
                                    };
                                    entry.game = Some(game);
                                    entry.roms = roms;
                                    entry.target = Some(target);
                                    claimed
                                        .err()
                                        .map(|why| format!("{} cannot be adopted: {why}", d.name))
                                }
                            }
                        }
                    };
                    if let Some(why) = why {
                        blockers.push(refuse(&d.name, why));
                    }
                    entries.push(entry);
                }

                let dir = open_folder(config, &inputs.folder.root, &inputs.folder.path).ok();
                for (id, kept, cached) in &kept {
                    let Some(kept) = kept else {
                        blockers.push(refuse(
                            &id.to_string(),
                            format!("managed folder {folder} has no kept file {id}"),
                        ));
                        continue;
                    };
                    let planned = self.plan_restore(
                        dir.as_ref(),
                        kept,
                        cached.as_ref(),
                        dat.as_ref().zip(index.as_ref()),
                        &inputs.selected,
                        &mut names,
                    )?;
                    match planned {
                        Ok((restore, fresh)) => {
                            if let Some(fresh) = fresh {
                                probed.push((kept.id, fresh));
                            }
                            restores.push(restore);
                        }
                        Err(why) => blockers.push(refuse(&kept.name, why)),
                    }
                }

                for target in &choices.repack {
                    let built = inputs.built.get(target);
                    let checked = scan.files.iter().find(|c| c.target == *target);
                    let f = folder_found
                        .files
                        .get(&target.to_lowercase())
                        .filter(|f| f.name == *target && f.regular);
                    let intact = checked.is_some_and(|c| {
                        matches!(
                            c.outcome,
                            FileOutcome::Unchanged | FileOutcome::Refreshed | FileOutcome::Repacked
                        )
                    });
                    let repack = match (built, checked, f) {
                        (None, ..) => Err(format!("{target} is not a file Title Select built")),
                        (_, Some(c), _) if c.torrentzip => {
                            Err(format!("{target} is TorrentZip already"))
                        }
                        (Some(built), Some(_), Some(f)) if intact => {
                            let sha256 = scan
                                .probes
                                .get(target)
                                .filter(|(seen, _)| {
                                    seen.identity == f.identity && seen.file == f.file
                                })
                                .map(|(_, p)| p.sha256.clone())
                                .or_else(|| built.sha256.clone().filter(|_| built.file == f.file));
                            match sha256 {
                                Some(sha256) => Ok(CleanupRepack {
                                    target: target.clone(),
                                    game: built.game.clone(),
                                    roms: built.roms.clone(),
                                    sources: sources.get(target).cloned().unwrap_or_default(),
                                    file: f.file.clone(),
                                    identity: f.identity.into(),
                                    sha256,
                                }),
                                None => Err(format!(
                                    "the store has no record of what {target} holds; check the folder, then plan \
                                     again"
                                )),
                            }
                        }
                        _ => Err(format!(
                            "{target} is not as it was built; check the folder, then plan again"
                        )),
                    };
                    match repack {
                        Ok(repack) => repacks.push(repack),
                        Err(why) => blockers.push(refuse(target, why)),
                    }
                }
            }
        }
        if !probed.is_empty() {
            self.write(|tx| {
                for (id, (seen, probe)) in &probed {
                    let (dev, ino) = seen.identity.to_sql();
                    tx.execute(
                        "UPDATE kept_files SET probe = ?2, probe_ctime_ns = ?3
                         WHERE id = ?1 AND dev = ?4 AND ino = ?5 AND size = ?6 AND mtime_ns = ?7",
                        params![
                            id,
                            to_json(probe),
                            seen.ctime_ns as i64,
                            dev,
                            ino,
                            seen.file.size as i64,
                            seen.file.mtime_ns as i64
                        ],
                    )?;
                }
                Ok(())
            })?;
        }

        let (check, scan) = match found {
            Some(folder_found) => {
                let mut check = FolderCheck {
                    folder: inputs.folder.id,
                    summary: CheckSummary {
                        marker: folder_found.marker,
                        ..CheckSummary::default()
                    },
                    generation: inputs.folder.generation,
                    selection: Some(inputs.selection_id),
                    dat_revision: Some(inputs.revision.sha256.clone()),
                    files: Vec::new(),
                    drift: Vec::new(),
                    acknowledged: Vec::new(),
                };
                if let Some(scan) = &folder_found.scan {
                    check.summary = scan.summary();
                    check.files = scan.files.clone();
                    check.drift = scan.drift.clone();
                    check.acknowledged = scan.acknowledged.clone();
                }
                (Some(check), folder_found.scan)
            }
            None => (None, None),
        };
        Ok(Planned {
            content: PlanContent {
                folder: inputs.folder.id,
                generation: inputs.folder.generation,
                selection: inputs.selection_id,
                audit: Some(audit),
                dat_revision: inputs.revision.sha256.clone(),
                choices: PlanChoices::default(),
                items: Vec::new(),
                removals: Vec::new(),
                blockers,
                cleanup: Some(Cleanup {
                    build,
                    choices: choices.clone(),
                    entries,
                    restores,
                    repacks,
                }),
            },
            previous: None,
            changes: Vec::new(),
            check,
            retained: HashSet::new(),
            scan,
        })
    }

    /// How a cleanup puts kept file `kept` back, reading it through the folder's handle `dir` only when what was read
    /// of it before, `cached`, is not of it as it is now. Returns the restore and what was read, if it was read now;
    /// or why it cannot be put back.
    fn plan_restore(
        &self,
        dir: Option<&FolderDir>,
        kept: &KeptFile,
        cached: Option<&(i64, Probe)>,
        dat: Option<(&Dat, &GameIndex)>,
        selected: &HashMap<String, String>,
        names: &mut Names,
    ) -> Result<std::result::Result<Restore, String>> {
        let name = &kept.name;
        let put_back = |names: &mut Names, why: Option<String>| {
            if selected.contains_key(&name.to_lowercase()) {
                return Err(format!(
                    "{name} is a selected game's file name, and the kept file does not hold that game{}",
                    why.map(|w| format!(" ({w})")).unwrap_or_default()
                ));
            }
            names
                .claim(name)
                .map_err(|why| format!("{name} cannot be put back: {why}"))?;
            Ok(CleanupRestore {
                kept: kept.clone(),
                kind: RestoreKind::PutBack,
                target: name.clone(),
                game: None,
                roms: Vec::new(),
                torrentzip: true,
            })
        };
        let Some(dir) = dir else {
            return Ok(Err("the managed folder cannot be opened".into()));
        };
        let retention = match open_retention(self, kept.folder, dir, kept.generation, false)? {
            Ok(Some(retention)) => retention,
            Ok(None) => {
                return Ok(Err(format!(
                    "{RETENTION}/{} is not in the folder",
                    kept.generation
                )));
            }
            Err(why) => return Ok(Err(why)),
        };
        let st = match retention.generation.stat(&kept.kept_name) {
            Ok(Some(st))
                if Identity::of(&st) == Identity::from(kept.identity)
                    && file_stat(&st) == kept.file
                    && (FolderDir::kind(&st) == rustix::fs::FileType::Directory)
                        == kept.directory =>
            {
                st
            }
            _ => {
                return Ok(Err(format!(
                    "{RETENTION}/{}/{} is not what was kept",
                    kept.generation, kept.kept_name
                )));
            }
        };
        // A kept folder was never read, and is only ever put back as it was.
        if kept.directory {
            return Ok(put_back(names, None).map(|r| (r, None)));
        }
        let cached = cached.map(|(ctime, probe)| {
            (
                Seen {
                    file: kept.file.clone(),
                    identity: kept.identity.into(),
                    ctime_ns: *ctime as i128,
                },
                probe.clone(),
            )
        });
        let (probe, hashed) = reuse(&retention.generation, &kept.kept_name, &st, cached.as_ref());
        let probe = match probe {
            Ok(probe) => probe,
            Err(why) => return Ok(Err(format!("the kept file cannot be read: {why}"))),
        };
        if kept.sha256.as_ref().is_some_and(|s| *s != probe.sha256) {
            return Ok(Err("the kept file is not the file that was kept".into()));
        }
        let fresh = hashed.then(|| (Seen::of(&st), probe.clone()));
        let game = match dat {
            None => Err("the DAT revision is no longer kept".to_owned()),
            Some((dat, index)) => selected_game(&probe, index, dat, selected),
        };
        let restore = match game {
            Ok((game, roms, target)) => names
                .claim(&target)
                .map(|()| CleanupRestore {
                    kept: kept.clone(),
                    kind: if target == *name {
                        RestoreKind::Restore
                    } else {
                        RestoreKind::Adopt
                    },
                    target: target.clone(),
                    game: Some(game),
                    roms,
                    torrentzip: probe.torrentzip,
                })
                .map_err(|why| format!("{name} cannot be put back as {target}: {why}")),
            Err(why) => put_back(names, Some(why)),
        };
        Ok(restore.map(|r| (r, fresh)))
    }

    /// Plans the cleanup of Managed Folder `folder` that `request` asks for, and stores it for review with the check
    /// of the folder it was made with. Nothing is changed until you approve it.
    pub fn plan_cleanup(
        &self,
        config: &Config,
        folder: i64,
        request: &PlanCleanup,
    ) -> Result<Plan> {
        if request.is_empty() {
            return Err(Error::Refused(
                "choose what to do with at least one entry, kept file or built file".into(),
            ));
        }
        let mut choices = request.clone();
        choices.restore = choices
            .restore
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        choices.repack = choices
            .repack
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let planned = self.compute_cleanup(config, folder, &choices)?;
        self.write(|tx| {
            let active = folder_by_id(tx, folder)?.active_job;
            let check = match (&planned.check, active) {
                (Some(check), None) => Some(
                    store_check(tx, folder, None, planned.scan.as_ref(), check.clone())?
                        .summary
                        .id,
                ),
                _ => None,
            };
            insert_plan(tx, &planned, check)
        })
    }
}
