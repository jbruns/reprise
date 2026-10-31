//! Managed Folders, the Change Plans that build a Selection into one, and the Builds that carry them out.

use serde::{Deserialize, Serialize};

use crate::report::{EvidenceLevel, Finding};
use crate::selection::Role;

/// The marker file every Managed Folder holds: its [`Marker`] as JSON.
pub const MARKER: &str = ".title-select";
/// The folder, inside a Managed Folder, that a Build stages its files in, so that renames never cross mounts.
pub const STAGING: &str = ".title-select-staging";
/// The folder's retention area, where Kept Files will be set aside (#37). Reserved now, so that a check never reports
/// it as Drift.
pub const RETENTION: &str = ".title-select-kept";

/// What a Managed Folder's marker records. The store stays the source of truth; the marker lets a plan tell its
/// folder from a folder another store manages, a folder restored from a backup, or a wrong mount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    /// The id of the store that manages the folder.
    pub store: String,
    /// The Managed Folder's id in that store.
    pub folder: i64,
    /// The folder's build generation: 0 when registered, and one more after each successful Build.
    pub generation: u64,
}

/// A folder in a writable Root that Title Select builds a Source Set's Selections into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedFolder {
    pub id: i64,
    pub root: String,
    /// Relative to the Root, with `/` separators.
    pub path: String,
    /// The system: the bound Source Set's DAT, as `<provider>/<name>`.
    pub system: String,
    pub source_set: String,
    /// The Selection Profile whose Selections are built here.
    pub profile: String,
    pub generation: u64,
    pub created_at: String,
    /// Whether the Root is writable now.
    pub writable: bool,
    /// The current build: the latest that succeeded.
    pub build: Option<BuildSummary>,
    /// Files the store records as built into the folder.
    pub files: u64,
    /// The Build Job Queued or Running for the folder.
    pub active_job: Option<i64>,
    /// The newest plan made for the folder.
    pub latest_plan: Option<i64>,
    /// What the folder lacks of its current build's selection, by reason; absent when it lacks nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete: Option<IncompleteCounts>,
    /// The latest build, when it stopped before it finished and no build has succeeded since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unfinished: Option<UnfinishedBuild>,
    /// The latest check of the folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<CheckSummary>,
}

/// A build that stopped before it finished, as the store recorded it: shown straight after a restart, without a
/// rescan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnfinishedBuild {
    pub build: i64,
    /// `interrupted` when the service stopped, `failed` when the build stopped itself.
    pub state: BuildState,
    /// Files renamed into place.
    pub applied: u64,
    /// Files whose target holds something other than what the build wrote.
    pub uncertain: u64,
    /// Files the build never reached, or that failed.
    pub not_written: u64,
}

/// The request to register a Managed Folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddManagedFolder {
    /// A writable Root.
    pub root: String,
    /// Relative to the Root. When absent, the path `template` proposes for the Source Set's DAT is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The Path Template to propose the path from when `path` is absent; by default the configured one. Giving
    /// both is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    pub source_set: String,
    pub profile: String,
}

/// The request to rebind a Managed Folder to another Source Set of the same DAT, or to another Profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeManagedFolder {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_set: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

/// The request to plan building a Selection into a Managed Folder, with your choices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanBuild {
    pub selection: i64,
    #[serde(flatten)]
    pub choices: PlanChoices,
}

impl PlanBuild {
    pub fn new(selection: i64) -> PlanBuild {
        PlanBuild {
            selection,
            choices: PlanChoices::default(),
        }
    }
}

/// What you chose for a plan. They are part of its content, and so of the digest you approve.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanChoices {
    /// Build the resolved groups only, leaving unresolved groups untouched, instead of blocking on them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resolved_only: bool,
    /// The targets in conflict whose file you adopt: recorded as built, without writing anything. Any other conflict
    /// is left as it is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adopt: Vec<String>,
}

/// The request to approve a plan: the digest of the plan you reviewed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovePlan {
    pub digest: String,
}

/// What a plan does with one selected release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// Copy a source archive that is already the Game's TorrentZip.
    Copy,
    /// Write a TorrentZip from the verified payloads.
    Repack,
    /// The folder already holds this file, built from the same ROMs.
    Unchanged,
    /// No Entry in the Audit holds every ROM; skipped.
    NoSource,
    /// A file Title Select did not build is at the target, or one it built was changed there; it is never
    /// overwritten, so the item is skipped.
    Conflict,
    /// Cannot be built as planned, such as an invalid name or an unmanaged file at the target; skipped.
    Blocked,
    /// A file in conflict at the target that you chose to adopt: its ROMs are the ones the DAT expects there, so it
    /// is recorded as built and nothing is written.
    Adopt,
    /// Its group is unresolved and you chose to build the resolved groups only; skipped, and whatever is at its
    /// target is left untouched.
    Unresolved,
}

impl ItemKind {
    pub const ALL: [ItemKind; 8] = [
        ItemKind::Copy,
        ItemKind::Repack,
        ItemKind::Adopt,
        ItemKind::Unchanged,
        ItemKind::NoSource,
        ItemKind::Conflict,
        ItemKind::Unresolved,
        ItemKind::Blocked,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ItemKind::Copy => "copy",
            ItemKind::Repack => "repack",
            ItemKind::Unchanged => "unchanged",
            ItemKind::NoSource => "no_source",
            ItemKind::Conflict => "conflict",
            ItemKind::Blocked => "blocked",
            ItemKind::Adopt => "adopt",
            ItemKind::Unresolved => "unresolved",
        }
    }

    pub fn parse(s: &str) -> Option<ItemKind> {
        ItemKind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Whether a Build writes a file for the item.
    pub fn writes(self) -> bool {
        matches!(self, ItemKind::Copy | ItemKind::Repack)
    }
}

/// A ROM a built file must hold, as the DAT Revision lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedRom {
    pub name: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// A file's size and modification time, as a plan saw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStat {
    pub size: u64,
    /// Nanoseconds since the Unix epoch.
    pub mtime_ns: i128,
}

/// The Entry a ROM is taken from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSource {
    /// The ROM's name in the DAT.
    pub rom: String,
    /// Relative to the Source Set's folder.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    pub finding: Finding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<EvidenceLevel>,
    /// The file at `path`.
    pub file: FileStat,
}

/// One selected release in a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub group: String,
    /// The DAT's Game name.
    pub game: String,
    /// The file name in the Managed Folder: `<game>.zip`.
    pub target: String,
    pub kind: ItemKind,
    pub roms: Vec<ExpectedRom>,
    /// One per ROM, in ROM order; empty unless the item is built or unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<PlanSource>,
    /// Why the item has no source, conflicts or is blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What is at the target now, if anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing: Option<FileStat>,
    /// For a conflict: the file at the target holds the ROMs the DAT expects there, so you may adopt it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub adoptable: bool,
}

/// Why a plan cannot be approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerKind {
    /// The Source Set has no Audit to take sources from.
    NoAudit,
    /// The latest Audit is against another DAT Revision than the Selection.
    RevisionMismatch,
    /// A group is unresolved and you have not confirmed a choice.
    Unresolved,
    /// The Root is not writable.
    ReadOnlyRoot,
    /// The folder is missing.
    FolderMissing,
    /// The folder's marker does not match the store.
    Marker,
    /// A built file would change; that needs a later build step.
    Replacement,
    /// A built file would go; that needs a later build step.
    Removal,
    /// An earlier build stopped and its folder has not been cleaned up since, or the folder holds leftovers no
    /// build of this store accounts for.
    Recovery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blocker {
    pub kind: BlockerKind,
    pub message: String,
    /// The group to confirm, for an unresolved group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The file in the folder it is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// What a plan covers, in the order its digest is computed over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanContent {
    pub folder: i64,
    /// The folder's build generation the plan starts from.
    pub generation: u64,
    pub selection: i64,
    /// The Audit the sources come from.
    pub audit: Option<i64>,
    /// The Selection's DAT Revision, by SHA-256.
    pub dat_revision: String,
    /// Your choices, as you made them when planning.
    #[serde(default, skip_serializing_if = "PlanChoices::is_default")]
    pub choices: PlanChoices,
    pub items: Vec<PlanItem>,
    pub blockers: Vec<Blocker>,
}

impl PlanChoices {
    pub fn is_default(&self) -> bool {
        *self == PlanChoices::default()
    }
}

/// What caused a release to change from the folder's current build to the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// Your Profile's settings: another Profile version.
    Settings,
    /// Your choices.
    Choices,
    /// A newer DAT Revision.
    DatRevision,
}

/// One release that changes from the folder's current build to the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanChange {
    pub group: String,
    pub title: String,
    /// Absent when the release is new.
    pub before: Option<Role>,
    /// Absent when the release is gone.
    pub after: Option<Role>,
    pub cause: Cause,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSummary {
    pub copy: u64,
    pub repack: u64,
    pub unchanged: u64,
    pub no_source: u64,
    pub conflict: u64,
    pub blocked: u64,
    #[serde(default)]
    pub adopt: u64,
    #[serde(default)]
    pub unresolved: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub digest: String,
    pub actor: String,
    pub at: String,
}

/// A Change Plan that builds a Selection into a Managed Folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub id: i64,
    /// The SHA-256 of the canonical JSON of `content`, which an approval binds to.
    pub digest: String,
    pub created_at: String,
    pub content: PlanContent,
    pub summary: PlanSummary,
    /// The Selection of the folder's current build, which `changes` compares against.
    pub previous_selection: Option<i64>,
    pub changes: Vec<PlanChange>,
    pub approval: Option<Approval>,
    /// The Build Job that carries the plan out, once approved.
    pub job: Option<i64>,
    /// The check of the folder made with the plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<i64>,
}

impl Plan {
    pub fn approvable(&self) -> bool {
        self.content.blockers.is_empty()
    }
}

impl PlanContent {
    pub fn summary(&self) -> PlanSummary {
        let mut s = PlanSummary::default();
        for item in &self.items {
            *match item.kind {
                ItemKind::Copy => &mut s.copy,
                ItemKind::Repack => &mut s.repack,
                ItemKind::Unchanged => &mut s.unchanged,
                ItemKind::NoSource => &mut s.no_source,
                ItemKind::Conflict => &mut s.conflict,
                ItemKind::Blocked => &mut s.blocked,
                ItemKind::Adopt => &mut s.adopt,
                ItemKind::Unresolved => &mut s.unresolved,
            } += 1;
        }
        s
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildState {
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

impl BuildState {
    pub fn as_str(self) -> &'static str {
        match self {
            BuildState::Running => "running",
            BuildState::Succeeded => "succeeded",
            BuildState::Failed => "failed",
            BuildState::Interrupted => "interrupted",
        }
    }

    pub fn parse(s: &str) -> Option<BuildState> {
        [
            BuildState::Running,
            BuildState::Succeeded,
            BuildState::Failed,
            BuildState::Interrupted,
        ]
        .into_iter()
        .find(|k| k.as_str() == s)
    }
}

/// A Build: one approved plan carried out by a Build Job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildSummary {
    pub id: i64,
    pub folder: i64,
    pub plan: i64,
    pub job: i64,
    pub state: BuildState,
    /// The folder's generation once the Build succeeded.
    pub generation: u64,
    pub selection: i64,
    pub audit: i64,
    pub dat_revision: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// Files renamed into place, files that failed, and files not reached.
    pub applied: u64,
    pub failed: u64,
    pub pending: u64,
    /// Files whose target, once the build stopped, held something other than what it wrote.
    #[serde(default)]
    pub uncertain: u64,
    /// What the plan held, including the items it skipped.
    pub planned: PlanSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemState {
    /// Not written yet; after an interruption, never written.
    Pending,
    /// Written and verified, and being renamed into place.
    Publishing,
    /// Renamed into place.
    Applied,
    /// Its re-hash or validation failed, or it could not be written; nothing was put in place.
    Failed,
    /// The build stopped while renaming it into place, and what is at its target now is not the file it wrote; that
    /// file is left as it is.
    Uncertain,
}

impl ItemState {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemState::Pending => "pending",
            ItemState::Publishing => "publishing",
            ItemState::Applied => "applied",
            ItemState::Failed => "failed",
            ItemState::Uncertain => "uncertain",
        }
    }

    pub fn parse(s: &str) -> Option<ItemState> {
        [
            ItemState::Pending,
            ItemState::Publishing,
            ItemState::Applied,
            ItemState::Failed,
            ItemState::Uncertain,
        ]
        .into_iter()
        .find(|k| k.as_str() == s)
    }
}

/// One file a Build writes, and how it went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildItem {
    pub target: String,
    pub game: String,
    pub kind: ItemKind,
    pub state: ItemState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildDetail {
    pub build: BuildSummary,
    pub items: Vec<BuildItem>,
}

/// A file in a Managed Folder and its provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltFile {
    pub target: String,
    pub game: String,
    /// `copy` or `repack`.
    pub kind: ItemKind,
    pub build: i64,
    pub selection: i64,
    pub audit: i64,
    pub dat_revision: String,
    pub sources: Vec<PlanSource>,
    pub roms: Vec<ExpectedRom>,
    pub file: FileStat,
    /// The SHA-256 of the whole file, when the store recorded it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// False once a check found the file holds the right ROMs in a ZIP that is not TorrentZip, such as one re-zipped
    /// by another tool.
    #[serde(default = "yes")]
    pub torrentzip: bool,
}

fn yes() -> bool {
    true
}

/// Why a Managed Folder is Incomplete: why its current build lacks a selected game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncompleteReason {
    /// Its group was unresolved and you built the resolved groups only.
    Unresolved,
    /// No audited file holds every ROM.
    NoSource,
    /// Its file failed to build.
    Failed,
    /// Something Title Select did not build, or changed since, is at its target.
    Conflict,
    /// It cannot be built as planned, such as for an invalid name.
    Blocked,
    /// A check found its built file gone.
    Missing,
}

impl IncompleteReason {
    pub const ALL: [IncompleteReason; 6] = [
        IncompleteReason::Unresolved,
        IncompleteReason::NoSource,
        IncompleteReason::Failed,
        IncompleteReason::Conflict,
        IncompleteReason::Blocked,
        IncompleteReason::Missing,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            IncompleteReason::Unresolved => "unresolved",
            IncompleteReason::NoSource => "no_source",
            IncompleteReason::Failed => "failed",
            IncompleteReason::Conflict => "conflict",
            IncompleteReason::Blocked => "blocked",
            IncompleteReason::Missing => "missing",
        }
    }

    pub fn parse(s: &str) -> Option<IncompleteReason> {
        IncompleteReason::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// How many selected games a Managed Folder lacks, by reason.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncompleteCounts {
    pub unresolved: u64,
    pub no_source: u64,
    pub failed: u64,
    pub conflict: u64,
    pub blocked: u64,
    pub missing: u64,
}

impl IncompleteCounts {
    pub fn get(&self, reason: IncompleteReason) -> u64 {
        match reason {
            IncompleteReason::Unresolved => self.unresolved,
            IncompleteReason::NoSource => self.no_source,
            IncompleteReason::Failed => self.failed,
            IncompleteReason::Conflict => self.conflict,
            IncompleteReason::Blocked => self.blocked,
            IncompleteReason::Missing => self.missing,
        }
    }

    pub fn add(&mut self, reason: IncompleteReason) {
        *match reason {
            IncompleteReason::Unresolved => &mut self.unresolved,
            IncompleteReason::NoSource => &mut self.no_source,
            IncompleteReason::Failed => &mut self.failed,
            IncompleteReason::Conflict => &mut self.conflict,
            IncompleteReason::Blocked => &mut self.blocked,
            IncompleteReason::Missing => &mut self.missing,
        } += 1;
    }

    pub fn total(&self) -> u64 {
        IncompleteReason::ALL.into_iter().map(|r| self.get(r)).sum()
    }
}

/// One selected game a Managed Folder lacks, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncompleteItem {
    pub reason: IncompleteReason,
    pub group: String,
    pub game: String,
    pub target: String,
    /// The error of a failed file, or why the item was skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What a Managed Folder lacks: what its current build recorded, and the conflicts and missing files its latest
/// check found since. Never written to the marker.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Incomplete {
    /// The current build.
    pub build: Option<i64>,
    pub counts: IncompleteCounts,
    pub items: Vec<IncompleteItem>,
}

/// What a check found for one built file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOutcome {
    /// Its size and modification time are as recorded; it was not read.
    Unchanged,
    /// Its size or modification time changed, but it is still the file the build wrote; the record was refreshed.
    Refreshed,
    /// It holds the ROMs the DAT expects there in another ZIP, such as one re-zipped by another tool; recorded as
    /// built, and flagged when it is not TorrentZip.
    Repacked,
    /// It changed and no longer holds the ROMs the DAT expects there, or is no longer a regular file.
    Conflict,
    /// It is gone.
    Missing,
    /// It could not be read, or changed while it was read.
    Unreadable,
}

impl FileOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            FileOutcome::Unchanged => "unchanged",
            FileOutcome::Refreshed => "refreshed",
            FileOutcome::Repacked => "repacked",
            FileOutcome::Conflict => "conflict",
            FileOutcome::Missing => "missing",
            FileOutcome::Unreadable => "unreadable",
        }
    }
}

/// One built file, as a check found it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckedFile {
    pub target: String,
    pub game: String,
    pub outcome: FileOutcome,
    /// Whether the check read the file. A file whose size and modification time are as recorded is never read.
    pub hashed: bool,
    /// False when the file holds the right ROMs in a ZIP that is not TorrentZip.
    pub torrentzip: bool,
    /// Whether the selection the check was made against selects it.
    pub selected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What an entry Title Select did not build is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftKind {
    /// A game the DAT lists, under its own name, that the selection does not select.
    Unselected,
    /// A selected game, under its own name, that Title Select did not build. A plan can adopt it.
    Selected,
    /// A game the DAT lists, under another name.
    Renamed,
    /// Nothing the DAT lists: a hack, homebrew or junk, or a file that cannot be read as one.
    Unknown,
    /// A directory, which is never opened.
    Directory,
}

impl DriftKind {
    pub const ALL: [DriftKind; 5] = [
        DriftKind::Unselected,
        DriftKind::Selected,
        DriftKind::Renamed,
        DriftKind::Unknown,
        DriftKind::Directory,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            DriftKind::Unselected => "unselected",
            DriftKind::Selected => "selected",
            DriftKind::Renamed => "renamed",
            DriftKind::Unknown => "unknown",
            DriftKind::Directory => "directory",
        }
    }
}

/// One entry in a Managed Folder that Title Select did not build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftEntry {
    pub name: String,
    pub kind: DriftKind,
    /// The DAT's game it holds, for a known title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<String>,
    /// Whether it sits at a selected game's target, which makes the folder Incomplete.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub conflict: bool,
    /// Whether the check read it; one unchanged since an earlier check is not read again.
    pub hashed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The counts of a check.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckSummary {
    pub id: i64,
    pub at: String,
    /// The Check Job that made it, or none when a plan made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<i64>,
    /// Why the folder's marker is not this store's and this folder's; absent when it is. The rest of the check is
    /// skipped when it is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    pub unchanged: u64,
    pub refreshed: u64,
    pub repacked: u64,
    /// Built files in conflict, and entries in conflict at a selected game's target.
    pub conflict: u64,
    pub missing: u64,
    pub unreadable: u64,
    /// Files the check read.
    pub hashed: u64,
    /// Entries Title Select did not build.
    pub drift: u64,
}

/// A read-only check of a Managed Folder against the store's record of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderCheck {
    pub folder: i64,
    pub summary: CheckSummary,
    /// The folder's generation when checked.
    pub generation: u64,
    /// The Selection checked against: the plan's, or the current build's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<i64>,
    /// The DAT Revision entries were hashed against, by SHA-256.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dat_revision: Option<String>,
    pub files: Vec<CheckedFile>,
    pub drift: Vec<DriftEntry>,
}

/// Why a name cannot be a file name on every filesystem a Play Library may be shared from, or `None` when it can.
pub fn invalid_name(name: &str) -> Option<String> {
    if name.is_empty() || name == "." || name == ".." {
        return Some(format!("{name:?} is not a file name"));
    }
    if let Some(ch) = name.chars().find(|c| "<>:\"/\\|?*".contains(*c)) {
        return Some(format!(
            "{name} contains {ch}, which Windows and SMB refuse"
        ));
    }
    if name.chars().any(char::is_control) {
        return Some(format!("{name:?} contains a control character"));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some(format!(
            "{name:?} ends with a dot or a space, which Windows and SMB drop"
        ));
    }
    if reserved_device(name) {
        return Some(format!(
            "{name} is named for a device, which Windows and SMB reserve"
        ));
    }
    None
}

/// Whether `name`, up to its first dot, is a Windows device name, whatever its case and extension.
fn reserved_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    let upper = stem.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => match upper.as_bytes() {
            [b'C', b'O', b'M', digit] | [b'L', b'P', b'T', digit] => (b'1'..=b'9').contains(digit),
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_invalid_on_windows_or_smb_are_refused_and_dat_names_are_not() {
        for ok in [
            "Pong (USA).zip",
            "Combat (USA, Europe) (Rev 1).zip",
            "Café ★.zip",
            "Console (USA).zip",
            "COM10.zip",
            "LPT0.bin",
            "Auxiliary.a26",
        ] {
            assert_eq!(invalid_name(ok), None, "{ok}");
        }
        for bad in [
            "a<b.zip",
            "a>b.zip",
            "a:b.zip",
            "a\"b.zip",
            "a/b.zip",
            "a\\b.zip",
            "a|b.zip",
            "a?b.zip",
            "a*b.zip",
            "a\u{1}b.zip",
            "name.",
            "name ",
            "",
            "..",
            "CON.zip",
            "con",
            "Prn.a26",
            "aux.tar.gz",
            "NUL .zip",
            "com1.zip",
            "LPT9",
        ] {
            assert!(invalid_name(bad).is_some(), "{bad:?}");
        }
    }
}
