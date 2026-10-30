//! Change Plans: what building a Selection into a Managed Folder would do, computed from the store and a stat of the
//! folder and of the sources, and stored with the SHA-256 of its canonical JSON, which an approval binds to.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::Metadata;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use sha2::{Digest, Sha256};
use title_select_api::build::{
    Approval, Blocker, BlockerKind, Cause, ExpectedRom, FileStat, ItemKind, Plan, PlanChange,
    PlanContent, PlanItem, PlanSource, STAGING, invalid_name,
};
use title_select_api::report::{Entry, EvidenceLevel, Finding};
use title_select_api::selection::{ReleaseRow, Role, Selection, diff_rows};
use title_select_api::{DatRevisionRecord, Job, Workers};
use title_select_engine::build::is_torrentzip;
use title_select_engine::dat::Dat;

use super::audits::{ENTRY_COLUMNS, entry_from_row};
use super::dats::revision_by_id;
use super::folder_dir::{FolderDir, file_stat};
use super::folders::{
    FolderRow, MARKER_NEW, MarkerState, folder_by_id, is_own, open_folder, read_marker, store_id,
};
use super::jobs::{build_hold, job_by_id};
use super::selections::{read_rows, read_selection, record_by_id};
use super::sets::{resolve, set_by_id};
use super::{Error, Result, Store, now};
use crate::config::Config;

/// A file a Build wrote into a Managed Folder, as the store recorded it.
#[derive(Debug, Clone)]
pub(super) struct Built {
    pub game: String,
    pub roms: Vec<ExpectedRom>,
    pub file: FileStat,
}

pub(super) fn read_built(c: &Connection, folder: i64) -> Result<BTreeMap<String, Built>> {
    let mut stmt = c.prepare(
        "SELECT target, game, roms, size, mtime_ns FROM built_files WHERE folder_id = ?1",
    )?;
    let rows = stmt.query_map([folder], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Built {
                game: r.get(1)?,
                roms: json(r, 2)?,
                file: FileStat {
                    size: r.get::<_, i64>(3)? as u64,
                    mtime_ns: r.get::<_, i64>(4)? as i128,
                },
            },
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub(super) fn json<T: serde::de::DeserializeOwned>(row: &Row, i: usize) -> rusqlite::Result<T> {
    let text: String = row.get(i)?;
    serde_json::from_str(&text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
    })
}

pub(super) fn to_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("store values always serialize")
}

/// The SHA-256 of the plan's canonical JSON: compact, with fields in declaration order and items in plan order.
pub(super) fn digest(content: &PlanContent) -> String {
    let bytes = serde_json::to_vec(content).expect("a plan always serializes");
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Nanoseconds since the Unix epoch.
pub(super) fn mtime_ns(meta: &Metadata) -> i128 {
    match meta.modified() {
        Ok(at) => match at.duration_since(UNIX_EPOCH) {
            Ok(after) => after.as_nanos() as i128,
            Err(e) => -(e.duration().as_nanos() as i128),
        },
        Err(_) => 0,
    }
}

pub(super) fn stat(meta: &Metadata) -> FileStat {
    FileStat {
        size: meta.len(),
        mtime_ns: mtime_ns(meta),
    }
}

/// What the folder's current Build was made from.
struct Previous {
    selection: i64,
    rows: Vec<ReleaseRow>,
    profile_version: i64,
    revision: i64,
}

/// Everything a plan reads from the store, read in one transaction.
struct Inputs {
    store: String,
    folder: FolderRow,
    set_root: String,
    set_path: String,
    selection_id: i64,
    selection: Selection,
    rows: Vec<ReleaseRow>,
    profile_version: i64,
    revision_id: i64,
    revision: DatRevisionRecord,
    /// The Source Set's latest Audit and its DAT Revision.
    audit: Option<(i64, i64, String)>,
    /// That Audit's Entries, when it is against the Selection's DAT Revision.
    entries: Vec<Entry>,
    built: BTreeMap<String, Built>,
    previous: Option<Previous>,
    /// The folder's earliest Build that ended without its folder being cleaned up, and why recovery left it.
    unrecovered: Option<(i64, Option<String>)>,
}

fn load(c: &Connection, folder_id: i64, selection_id: i64) -> Result<Inputs> {
    let folder = folder_by_id(c, folder_id)?;
    let (set_id, revision_id, profile_version, profile_id, pruned): (i64, i64, i64, i64, bool) = c
        .query_row(
            "SELECT sel.source_set_id, sel.dat_revision_id, sel.profile_version_id, v.profile_id,
                    sel.pruned_at IS NOT NULL
             FROM selections sel JOIN profile_versions v ON v.id = sel.profile_version_id
             WHERE sel.id = ?1",
            [selection_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("no selection {selection_id}")))?;
    if set_id != folder.source_set_id || profile_id != folder.profile_id {
        let record = record_by_id(c, selection_id)?;
        return Err(Error::Refused(format!(
            "selection {selection_id} was made for source set {} with profile {}, but managed folder {} builds \
             source set {} with profile {}",
            record.source_set, record.profile.name, folder.id, folder.source_set, folder.profile
        )));
    }
    if pruned {
        return Err(Error::Refused(format!(
            "selection {selection_id} was pruned and only its summary is kept; make a new selection to build"
        )));
    }
    let selection = read_selection(c, selection_id)?;
    let rows = read_rows(c, selection_id)?;
    let (_, revision) = revision_by_id(c, revision_id)?;
    let set = set_by_id(c, set_id)?;
    let audit: Option<(i64, i64, String)> = c
        .query_row(
            "SELECT a.id, a.dat_revision_id, r.sha256 FROM audits a JOIN dat_revisions r ON r.id = a.dat_revision_id
             WHERE a.source_set_id = ?1 AND a.status = 'finished' AND a.pruned_at IS NULL
             ORDER BY a.finish_seq DESC LIMIT 1",
            [set_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let entries = match &audit {
        Some((id, audit_revision, _)) if *audit_revision == revision_id => {
            let mut stmt = c.prepare(&format!(
                "SELECT {ENTRY_COLUMNS} FROM audit_entries WHERE audit_id = ?1 ORDER BY seq"
            ))?;
            let mut rows = stmt.query([id])?;
            let mut entries = Vec::new();
            while let Some(row) = rows.next()? {
                entries.push(entry_from_row(row)?);
            }
            entries
        }
        _ => Vec::new(),
    };
    let previous = match folder.current_build {
        Some(build) => {
            let (selection, profile_version, revision): (i64, i64, i64) = c.query_row(
                "SELECT b.selection_id, sel.profile_version_id, sel.dat_revision_id
                 FROM builds b JOIN selections sel ON sel.id = b.selection_id WHERE b.id = ?1",
                [build],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            Some(Previous {
                selection,
                rows: read_rows(c, selection)?,
                profile_version,
                revision,
            })
        }
        None => None,
    };
    let unrecovered = c
        .query_row(
            "SELECT id, recovery_error FROM builds
             WHERE folder_id = ?1 AND cleaned_at IS NULL AND state IN ('failed', 'interrupted')
             ORDER BY id LIMIT 1",
            [folder.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(Inputs {
        unrecovered,
        store: store_id(c)?,
        built: read_built(c, folder.id)?,
        folder,
        set_root: set.record.root,
        set_path: set.record.path,
        selection_id,
        selection,
        rows,
        profile_version,
        revision_id,
        revision,
        audit,
        entries,
        previous,
    })
}

/// A plan before it is stored.
pub(super) struct Planned {
    pub content: PlanContent,
    pub previous: Option<i64>,
    pub changes: Vec<PlanChange>,
}

fn blocker(kind: BlockerKind, message: impl Into<String>) -> Blocker {
    Blocker {
        kind,
        message: message.into(),
        group: None,
        target: None,
    }
}

/// The files in a Managed Folder, other than its own, by lowercased name: the name as it is, and its stat.
type Files = HashMap<String, (String, FileStat)>;

/// The folder's state, from a stat of it and its marker: its files by lowercased name, or why it cannot be built
/// into.
fn check_folder(config: &Config, inputs: &Inputs, blockers: &mut Vec<Blocker>) -> Option<Files> {
    let folder = &inputs.folder;
    if !config.root(&folder.root).is_some_and(|r| r.writable) {
        blockers.push(blocker(
            BlockerKind::ReadOnlyRoot,
            format!(
                "root {} is not writable; declare it with writable = true to build into it",
                folder.root
            ),
        ));
    }
    let dir = match open_folder(config, &folder.root, &folder.path) {
        Ok(dir) => dir,
        Err(reason) => {
            blockers.push(blocker(
                BlockerKind::FolderMissing,
                format!("the managed folder cannot be opened: {reason}. Is its share mounted?"),
            ));
            return None;
        }
    };
    let listing = match dir.entries() {
        Ok(listing) => listing,
        Err(e) => {
            blockers.push(blocker(BlockerKind::FolderMissing, e.to_string()));
            return None;
        }
    };
    let mut files = HashMap::new();
    let mut leftovers = Vec::new();
    for (name, _) in listing {
        if is_own(&name) {
            if name == STAGING || name == MARKER_NEW {
                leftovers.push(name);
            }
            continue;
        }
        let Ok(Some(st)) = dir.stat(&name) else {
            continue;
        };
        files.insert(name.to_lowercase(), (name, file_stat(&st)));
    }
    check_recovery(inputs, &dir, &leftovers, blockers);
    let generation = folder.generation;
    let problem = match read_marker(&dir) {
        MarkerState::Absent if files.is_empty() && generation == 0 => None,
        MarkerState::Absent if files.is_empty() => Some(format!(
            "the folder is empty and has no marker, though {generation} builds went into it; is its share mounted?"
        )),
        MarkerState::Absent => Some(
            "the folder has files but no .title-select marker; is this the right folder, or the right mount?"
                .to_owned(),
        ),
        MarkerState::Unreadable(e) => Some(format!("its .title-select marker cannot be read: {e}")),
        MarkerState::Present(marker) if marker.store != inputs.store => Some(
            "its .title-select marker is from another store; is this a restored store, or the wrong mount?"
                .to_owned(),
        ),
        MarkerState::Present(marker) if marker.folder != folder.id => Some(format!(
            "its .title-select marker is for managed folder {}, not {}; is this the right mount?",
            marker.folder, folder.id
        )),
        MarkerState::Present(marker) => match marker.generation.cmp(&generation) {
            Ordering::Equal => None,
            Ordering::Greater => Some(format!(
                "its marker is at build generation {}, newer than the store's {generation}; was the store restored \
                 from a backup?",
                marker.generation
            )),
            Ordering::Less => Some(format!(
                "its marker is at build generation {}, older than the store's {generation}; was the folder restored \
                 from a backup, or is this the wrong mount?",
                marker.generation
            )),
        },
    };
    if let Some(problem) = problem {
        blockers.push(blocker(BlockerKind::Marker, problem));
    }
    Some(files)
}

/// Blocks on an earlier Build whose folder recovery has not cleaned up, or on a staging directory or half-written
/// marker that no Build of this store accounts for.
fn check_recovery(
    inputs: &Inputs,
    dir: &FolderDir,
    leftovers: &[String],
    blockers: &mut Vec<Blocker>,
) {
    if let Some((build, reason)) = &inputs.unrecovered {
        blockers.push(blocker(
            BlockerKind::Recovery,
            format!(
                "build {build} stopped, and its folder has not been cleaned up since{}. The service cleans it up \
                 when it starts; restart it once the folder is as build {build} left it",
                reason
                    .as_deref()
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
            ),
        ));
    } else if !leftovers.is_empty() && inputs.folder.active_job.is_none() {
        blockers.push(blocker(
            BlockerKind::Recovery,
            format!(
                "{} holds {}, which no build of this store left there; remove {} to build here",
                dir.path().display(),
                leftovers.join(" and "),
                if leftovers.len() == 1 { "it" } else { "them" }
            ),
        ));
    }
}

/// One Entry that holds a ROM's payload.
struct Candidate<'a> {
    entry: &'a Entry,
    file: FileStat,
    copyable: bool,
}

/// Finds and ranks the Entries that hold each ROM, reading each source's metadata once and each archive's central
/// directory at most once per Game.
struct Sources<'a> {
    set_dir: Option<(PathBuf, PathBuf)>,
    by_rom: HashMap<(String, String), Vec<&'a Entry>>,
    by_path: HashMap<&'a str, Vec<&'a Entry>>,
    stats: HashMap<&'a str, Option<FileStat>>,
    copyable: HashMap<(&'a str, String), bool>,
}

impl<'a> Sources<'a> {
    fn new(config: &Config, inputs: &'a Inputs) -> Sources<'a> {
        let set_dir = resolve(config, &inputs.set_root, &inputs.set_path)
            .ok()
            .map(|r| (r.path, r.canonical_root));
        let mut by_rom: HashMap<(String, String), Vec<&Entry>> = HashMap::new();
        let mut by_path: HashMap<&str, Vec<&Entry>> = HashMap::new();
        for entry in &inputs.entries {
            by_path.entry(&entry.path).or_default().push(entry);
            if matches!(
                entry.finding,
                Finding::Matched | Finding::Misnamed | Finding::Conflicting
            ) {
                for rom in &entry.roms {
                    by_rom
                        .entry((rom.game.clone(), rom.name.clone()))
                        .or_default()
                        .push(entry);
                }
            }
        }
        Sources {
            set_dir,
            by_rom,
            by_path,
            stats: HashMap::new(),
            copyable: HashMap::new(),
        }
    }

    /// The source file at `path`, confined to the Source Set's Root.
    fn file(&mut self, path: &'a str) -> Option<FileStat> {
        let set_dir = &self.set_dir;
        self.stats
            .entry(path)
            .or_insert_with(|| {
                let (dir, root) = set_dir.as_ref()?;
                let full = dir.join(path);
                let real = full.canonicalize().ok()?;
                if !real.starts_with(root) {
                    return None;
                }
                std::fs::metadata(&real)
                    .ok()
                    .filter(Metadata::is_file)
                    .map(|m| stat(&m))
            })
            .clone()
    }

    /// Whether the archive at `path` can be copied as `game`'s file as it is: its members are exactly the Game's
    /// ROMs under their names, each Matched on its payload, and it is already TorrentZip.
    fn copyable(&mut self, path: &'a str, game: &str, roms: &[ExpectedRom]) -> bool {
        if let Some(&known) = self.copyable.get(&(path, game.to_owned())) {
            return known;
        }
        let members = self.by_path.get(path).map(Vec::as_slice).unwrap_or(&[]);
        let names: BTreeSet<&str> = roms.iter().map(|r| r.name.as_str()).collect();
        let exact = members.len() == roms.len()
            && names.len() == roms.len()
            && members.iter().all(|e| {
                e.finding == Finding::Matched
                    && e.evidence
                        .as_ref()
                        .is_some_and(|ev| ev.level == EvidenceLevel::Payload)
                    && e.member.as_deref().is_some_and(|m| {
                        names.contains(m) && e.roms.iter().any(|r| r.game == game && r.name == m)
                    })
            })
            && members
                .iter()
                .filter_map(|e| e.member.as_deref())
                .collect::<BTreeSet<_>>()
                == names;
        let copyable = exact
            && self.file(path).is_some()
            && self
                .set_dir
                .as_ref()
                .is_some_and(|(dir, _)| is_torrentzip(&dir.join(path)));
        self.copyable.insert((path, game.to_owned()), copyable);
        copyable
    }

    /// The best Entry for each ROM, in ROM order, or the ROMs no Entry holds.
    fn choose(
        &mut self,
        game: &str,
        roms: &[ExpectedRom],
    ) -> std::result::Result<Vec<Candidate<'a>>, Vec<String>> {
        let mut chosen = Vec::new();
        let mut missing = Vec::new();
        for rom in roms {
            let entries = self
                .by_rom
                .get(&(game.to_owned(), rom.name.clone()))
                .cloned()
                .unwrap_or_default();
            let mut candidates: Vec<Candidate> = entries
                .into_iter()
                .filter_map(|entry| {
                    let file = self.file(&entry.path)?;
                    let copyable = entry.member.is_some() && self.copyable(&entry.path, game, roms);
                    Some(Candidate {
                        entry,
                        file,
                        copyable,
                    })
                })
                .collect();
            candidates.sort_by(|a, b| rank(a).cmp(&rank(b)));
            match candidates.into_iter().next() {
                Some(best) => chosen.push(best),
                None => missing.push(rom.name.clone()),
            }
        }
        if missing.is_empty() {
            Ok(chosen)
        } else {
            Err(missing)
        }
    }
}

/// Matched over Misnamed or Conflicting; Payload Evidence over a claim; a source that can be copied as it is; then
/// path order.
fn rank<'a>(c: &Candidate<'a>) -> (bool, bool, bool, &'a str, Option<&'a str>) {
    (
        c.entry.finding != Finding::Matched,
        c.entry.evidence.as_ref().map(|e| e.level) != Some(EvidenceLevel::Payload),
        !c.copyable,
        &c.entry.path,
        c.entry.member.as_deref(),
    )
}

fn compute(config: &Config, inputs: &Inputs, dat: Option<&Dat>) -> Planned {
    let mut blockers = Vec::new();
    let usable_audit = match &inputs.audit {
        None => {
            blockers.push(blocker(
                BlockerKind::NoAudit,
                format!(
                    "source set {} has no audit to take sources from; audit it first",
                    inputs.folder.source_set
                ),
            ));
            None
        }
        Some((_, revision, sha256)) if *revision != inputs.revision_id => {
            blockers.push(blocker(
                BlockerKind::RevisionMismatch,
                format!(
                    "the latest audit of source set {} is against DAT revision {}, but the selection is against {}; \
                     audit it against {} first",
                    inputs.folder.source_set,
                    short(sha256),
                    short(&inputs.revision.sha256),
                    short(&inputs.revision.sha256)
                ),
            ));
            None
        }
        Some((id, _, _)) => Some(*id),
    };
    for group in &inputs.selection.groups {
        if group.unresolved {
            blockers.push(Blocker {
                kind: BlockerKind::Unresolved,
                message: format!(
                    "the group {} is unresolved; confirm a choice for it in the selection",
                    group.group
                ),
                group: Some(group.group.clone()),
                target: None,
            });
        }
    }
    let folder = check_folder(config, inputs, &mut blockers);
    let files = folder.as_ref();

    let mut items = Vec::new();
    if let (Some(dat), Some(_)) = (dat, usable_audit) {
        items = plan_items(config, inputs, dat, files, &mut blockers);
    }
    let targets: BTreeSet<&str> = items.iter().map(|i: &PlanItem| i.target.as_str()).collect();
    if usable_audit.is_some() {
        for target in inputs.built.keys() {
            if !targets.contains(target.as_str()) {
                blockers.push(Blocker {
                    kind: BlockerKind::Removal,
                    message: format!(
                        "{target} was built here but is no longer selected; removing it needs a later build step"
                    ),
                    group: None,
                    target: Some(target.clone()),
                });
            }
        }
    }
    let (previous, changes) = match &inputs.previous {
        Some(previous) => (Some(previous.selection), changes(inputs, previous)),
        None => (None, Vec::new()),
    };
    Planned {
        content: PlanContent {
            folder: inputs.folder.id,
            generation: inputs.folder.generation,
            selection: inputs.selection_id,
            audit: usable_audit,
            dat_revision: inputs.revision.sha256.clone(),
            items,
            blockers,
        },
        previous,
        changes,
    }
}

fn short(sha256: &str) -> &str {
    &sha256[..sha256.len().min(12)]
}

fn plan_items(
    config: &Config,
    inputs: &Inputs,
    dat: &Dat,
    files: Option<&Files>,
    blockers: &mut Vec<Blocker>,
) -> Vec<PlanItem> {
    let games: HashMap<&str, usize> = dat
        .games
        .iter()
        .enumerate()
        .map(|(i, g)| (g.as_str(), i))
        .collect();
    let mut roms_of: Vec<Vec<ExpectedRom>> = vec![Vec::new(); dat.games.len()];
    for rom in &dat.roms {
        roms_of[rom.game].push(ExpectedRom {
            name: rom.name.clone(),
            size: rom.size,
            crc: rom.crc.clone(),
            md5: rom.md5.clone(),
            sha1: rom.sha1.clone(),
            sha256: rom.sha256.clone(),
        });
    }
    let mut winners: Vec<(&str, &str)> = Vec::new();
    let mut seen = BTreeSet::new();
    for group in &inputs.selection.groups {
        for winner in &group.winners {
            if seen.insert(winner.title.as_str()) {
                winners.push((&group.group, &winner.title));
            }
        }
    }
    let mut lowered: HashMap<String, usize> = HashMap::new();
    for (_, title) in &winners {
        *lowered
            .entry(format!("{title}.zip").to_lowercase())
            .or_default() += 1;
    }
    let mut sources = Sources::new(config, inputs);
    let mut items = Vec::new();
    for (group, game) in winners {
        let target = format!("{game}.zip");
        let roms = games
            .get(game)
            .map(|&i| roms_of[i].clone())
            .unwrap_or_default();
        let existing = files.and_then(|f| f.get(&target.to_lowercase()));
        let mut item = PlanItem {
            group: group.to_owned(),
            game: game.to_owned(),
            target: target.clone(),
            kind: ItemKind::Blocked,
            roms,
            sources: Vec::new(),
            reason: None,
            existing: existing.map(|(_, file)| file.clone()),
        };
        let duplicate = {
            let mut names = BTreeSet::new();
            item.roms
                .iter()
                .find(|r| !names.insert(r.name.as_str()))
                .map(|r| r.name.clone())
        };
        let built = inputs.built.get(&target);
        let blocked = if !games.contains_key(game) {
            Some(format!("the DAT revision lists no game named {game}"))
        } else if let Some(reason) = invalid_name(&target) {
            Some(reason)
        } else if let Some(reason) = item
            .roms
            .iter()
            .find_map(|r| invalid_name(&r.name).map(|why| format!("a ROM's name: {why}")))
        {
            Some(reason)
        } else if lowered[&target.to_lowercase()] > 1 {
            Some(format!(
                "another selected game's file differs from {target} only in case, which Windows and SMB cannot tell \
                 apart"
            ))
        } else {
            duplicate.map(|name| format!("the DAT lists two ROMs named {name} in it"))
        };
        if let Some(reason) = blocked {
            item.reason = Some(reason);
            items.push(item);
            continue;
        }
        if let Some(built) = built {
            if built.game != item.game || built.roms != item.roms {
                let reason = format!(
                    "{target} is built here from other ROMs; replacing it needs a later build step"
                );
                blockers.push(Blocker {
                    kind: BlockerKind::Replacement,
                    message: reason.clone(),
                    group: Some(item.group.clone()),
                    target: Some(target.clone()),
                });
                item.reason = Some(reason);
                items.push(item);
                continue;
            }
            match existing {
                Some((name, file)) if *name == target && *file == built.file => {
                    item.kind = ItemKind::Unchanged;
                    items.push(item);
                    continue;
                }
                Some(_) => {
                    item.kind = ItemKind::Conflict;
                    item.reason = Some(format!(
                        "{target} changed since it was built; it is never overwritten"
                    ));
                    items.push(item);
                    continue;
                }
                // A built file that is gone is written again.
                None => {}
            }
        } else if let Some((name, _)) = existing {
            item.kind = ItemKind::Conflict;
            item.reason = Some(format!(
                "{name} is in the folder and was not built by Title Select; it is never overwritten"
            ));
            items.push(item);
            continue;
        }
        if item.roms.is_empty() {
            item.kind = ItemKind::NoSource;
            item.reason = Some("the DAT lists no ROMs for it".into());
            items.push(item);
            continue;
        }
        match sources.choose(game, &item.roms) {
            Err(missing) => {
                item.kind = ItemKind::NoSource;
                item.reason = Some(format!(
                    "the latest audit found no source for {}",
                    missing.join(", ")
                ));
            }
            Ok(chosen) => {
                let first = &chosen[0];
                let copy = first.copyable
                    && chosen
                        .iter()
                        .all(|c| c.copyable && c.entry.path == first.entry.path);
                item.kind = if copy {
                    ItemKind::Copy
                } else {
                    ItemKind::Repack
                };
                item.sources = chosen
                    .iter()
                    .zip(&item.roms)
                    .map(|(c, rom)| PlanSource {
                        rom: rom.name.clone(),
                        path: c.entry.path.clone(),
                        member: c.entry.member.clone(),
                        finding: c.entry.finding,
                        evidence: c.entry.evidence.as_ref().map(|e| e.level),
                        file: c.file.clone(),
                    })
                    .collect();
            }
        }
        items.push(item);
    }
    items
}

/// What changes from the folder's current Build, and why: releases that become or stop being winners.
fn changes(inputs: &Inputs, previous: &Previous) -> Vec<PlanChange> {
    diff_rows(&previous.rows, &inputs.rows)
        .into_iter()
        .filter(|c| c.before == Some(Role::Winner) || c.after == Some(Role::Winner))
        .map(|c| {
            let rule_of = |rows: &[ReleaseRow]| {
                rows.iter()
                    .find(|r| r.group == c.group && r.title == c.title)
                    .map(|r| r.rule.is_choice())
                    .unwrap_or(false)
            };
            let cause = if c.before.is_none() || c.after.is_none() {
                Cause::DatRevision
            } else if rule_of(&previous.rows) || rule_of(&inputs.rows) {
                Cause::Choices
            } else if previous.profile_version != inputs.profile_version {
                Cause::Settings
            } else if previous.revision != inputs.revision_id {
                Cause::DatRevision
            } else {
                Cause::Settings
            };
            PlanChange {
                group: c.group,
                title: c.title,
                before: c.before,
                after: c.after,
                cause,
            }
        })
        .collect()
}

const PLAN_QUERY: &str =
    "SELECT p.id, p.digest, p.created_at, p.content, p.previous_selection_id, p.changes,
            a.digest, a.actor, a.approved_at, (SELECT max(j.id) FROM jobs j WHERE j.plan_id = p.id)
     FROM plans p LEFT JOIN plan_approvals a ON a.plan_id = p.id";

fn plan_from_row(r: &Row) -> rusqlite::Result<Plan> {
    let content: PlanContent = json(r, 3)?;
    let approval = match r.get::<_, Option<String>>(6)? {
        Some(digest) => Some(Approval {
            digest,
            actor: r.get(7)?,
            at: r.get(8)?,
        }),
        None => None,
    };
    Ok(Plan {
        id: r.get(0)?,
        digest: r.get(1)?,
        created_at: r.get(2)?,
        summary: content.summary(),
        content,
        previous_selection: r.get(4)?,
        changes: json(r, 5)?,
        approval,
        job: r.get(9)?,
    })
}

pub(super) fn plan_by_id(c: &Connection, id: i64) -> Result<Plan> {
    c.query_row(
        &format!("{PLAN_QUERY} WHERE p.id = ?1"),
        [id],
        plan_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("no plan {id}")))
}

pub(super) fn insert_plan(tx: &Transaction, planned: &Planned) -> Result<Plan> {
    let content = &planned.content;
    tx.execute(
        "INSERT INTO plans (folder_id, selection_id, audit_id, generation, content, digest, previous_selection_id,
                            changes, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            content.folder,
            content.selection,
            content.audit,
            content.generation as i64,
            to_json(content),
            digest(content),
            planned.previous,
            to_json(&planned.changes),
            now()
        ],
    )?;
    plan_by_id(tx, tx.last_insert_rowid())
}

impl Store {
    /// Computes what building `selection` into the Managed Folder would do.
    pub(super) fn compute_plan(
        &self,
        config: &Config,
        folder: i64,
        selection: i64,
    ) -> Result<Planned> {
        let inputs = self.read(|c| {
            let tx = c.unchecked_transaction()?;
            load(&tx, folder, selection)
        })?;
        let dat = if inputs.revision.blob_present {
            let blob = self.blob_path(&inputs.revision.sha256);
            Some(Dat::read(&blob)?)
        } else {
            None
        };
        let mut planned = compute(config, &inputs, dat.as_ref());
        if dat.is_none() {
            planned.content.blockers.push(blocker(
                BlockerKind::RevisionMismatch,
                format!(
                    "DAT revision {} is no longer kept; import its file ({}) again to build from it",
                    short(&inputs.revision.sha256),
                    inputs.revision.file
                ),
            ));
        }
        Ok(planned)
    }

    /// Plans building `selection` into Managed Folder `folder`, and stores the plan for review.
    pub fn plan_build(&self, config: &Config, folder: i64, selection: i64) -> Result<Plan> {
        let planned = self.compute_plan(config, folder, selection)?;
        self.write(|tx| insert_plan(tx, &planned))
    }

    pub fn plan(&self, id: i64) -> Result<Plan> {
        self.read(|c| plan_by_id(c, id))
    }

    /// Approves plan `id` as reviewed with `digest`, and queues the Build Job that carries it out. The plan is
    /// computed again first: when the store or the folder changed since, the new plan is stored and the approval
    /// refused, naming it.
    pub fn approve_plan(
        &self,
        config: &Config,
        id: i64,
        digest_reviewed: &str,
        actor: &str,
        workers: Workers,
    ) -> Result<Job> {
        let plan = self.plan(id)?;
        if plan.approval.is_some() {
            return Err(Error::Refused(format!("plan {id} is already approved")));
        }
        if digest_reviewed != plan.digest {
            return Err(Error::Conflict(format!(
                "you reviewed a different version of plan {id}; review it again"
            )));
        }
        if !plan.approvable() {
            return Err(Error::Refused(format!(
                "plan {id} has blockers; resolve them and plan again"
            )));
        }
        let content = &plan.content;
        let again = self.compute_plan(config, content.folder, content.selection)?;
        if digest(&again.content) != plan.digest {
            let newer = self.write(|tx| insert_plan(tx, &again))?;
            return Err(Error::Conflict(format!(
                "the plan changed since you reviewed it; review plan {}",
                newer.id
            )));
        }
        self.write(|tx| {
            let active: Option<i64> = tx
                .query_row(
                    "SELECT id FROM jobs WHERE managed_folder_id = ?1 AND kind = 'build'
                     AND state IN ('queued', 'running')",
                    [content.folder],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(job) = active {
                return Err(Error::Conflict(format!(
                    "managed folder {} already has a build running: job {job}",
                    content.folder
                )));
            }
            let folder = folder_by_id(tx, content.folder)?;
            let revision_id: i64 = tx.query_row(
                "SELECT dat_revision_id FROM selections WHERE id = ?1",
                [content.selection],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT INTO plan_approvals (plan_id, digest, actor, approved_at) VALUES (?1, ?2, ?3, ?4)",
                params![id, plan.digest, actor, now()],
            )?;
            tx.execute(
                "INSERT INTO jobs (state, kind, source_set_id, dat_revision_id, actor, readers, hashers, created_at,
                                   managed_folder_id, plan_id)
                 VALUES ('queued', 'build', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    folder.source_set_id,
                    revision_id,
                    actor,
                    workers.readers.max(1) as i64,
                    workers.hashers.max(1) as i64,
                    now(),
                    folder.id,
                    id
                ],
            )?;
            let job = tx.last_insert_rowid();
            // The Selection and Audit are held from pruning until the Job ends, in the transaction that queues it.
            let pruned: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM selections WHERE id = ?1 AND pruned_at IS NOT NULL)
                     OR EXISTS (SELECT 1 FROM audits WHERE id = ?2 AND pruned_at IS NOT NULL)",
                params![content.selection, content.audit],
                |r| r.get(0),
            )?;
            if pruned {
                return Err(Error::Refused(format!(
                    "plan {id}'s selection or audit was pruned; plan again"
                )));
            }
            let hold = build_hold(job);
            tx.execute(
                "INSERT INTO selection_citations (selection_id, cited_by) VALUES (?1, ?2)",
                params![content.selection, hold],
            )?;
            if let Some(audit) = content.audit {
                tx.execute(
                    "INSERT INTO audit_citations (audit_id, cited_by) VALUES (?1, ?2)",
                    params![audit, hold],
                )?;
            }
            job_by_id(tx, job)
        })
    }
}
