//! The types of Title Select's versioned JSON API (`/api/v1`), shared by the Service, the headless client and the UI
//! (ADR 0005). Every response body is one of these, or the `schema: 2` [`Report`].

pub mod report;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use report::*;

/// The path every API route is under.
pub const PREFIX: &str = "/api/v1";

/// How often the Service sends a comment on an idle event stream, so that a client can tell it from a stalled one.
pub const SSE_KEEP_ALIVE: std::time::Duration = std::time::Duration::from_secs(15);

/// The body of the unauthenticated health path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    pub status: String,
}

/// The body of every error response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
    /// The Job a refusal names, such as the active Job of a Source Set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<i64>,
}

/// A Root the Operator configured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Root {
    pub name: String,
    pub path: String,
    pub writable: bool,
}

/// A DAT, identified by its provider and header name, with its DAT Revisions, oldest import first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dat {
    /// `<provider>/<name>`.
    pub dat: String,
    pub provider: String,
    pub name: String,
    pub revisions: Vec<DatRevisionRecord>,
}

/// A DAT Revision as the store records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatRevisionRecord {
    /// SHA-256 of the unmodified DAT file; the DAT Revision's identity.
    pub sha256: String,
    pub provider: String,
    /// The DAT's header name.
    pub name: String,
    pub version: String,
    /// The original filename.
    pub file: String,
    pub imported_at: String,
    pub source_url: Option<String>,
    /// Whether the unmodified file is kept in the store.
    pub blob_present: bool,
}

impl DatRevisionRecord {
    /// The DAT this is a revision of, as `<provider>/<name>`.
    pub fn dat(&self) -> String {
        format!("{}/{}", self.provider, self.name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportOutcome {
    /// A new DAT Revision.
    Added,
    /// The same bytes were already imported; nothing changed.
    AlreadyImported,
    /// The same bytes were already imported and their dropped blob is restored.
    Restored,
}

/// The response to a DAT upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedDatRevision {
    pub outcome: ImportOutcome,
    pub dat_revision: DatRevisionRecord,
}

/// The DAT Revision a Source Set was last verified against: its latest Audit's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastVerified {
    pub sha256: String,
    pub version: String,
    pub finished_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSetRecord {
    /// The Source Set's folder name, which identifies it.
    pub name: String,
    pub root: String,
    /// Relative to the Root, with `/` separators.
    pub path: String,
    /// The bound DAT, as `<provider>/<name>`.
    pub dat: String,
    pub added_at: String,
    pub last_verified_against: Option<LastVerified>,
}

/// The request to register a folder in a Root as a Source Set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddSourceSet {
    pub root: String,
    /// Relative to the Root.
    pub path: String,
    /// The DAT to audit it against, as `<provider>/<name>`.
    pub dat: String,
}

/// One Audit in a Source Set's history. A pruned Audit keeps only this summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditSummary {
    pub id: i64,
    pub source_set: String,
    pub started_at: String,
    pub finished_at: String,
    pub dat_revision_sha256: String,
    pub dat_revision_version: String,
    pub findings: BTreeMap<Finding, usize>,
    pub games: BTreeMap<GameStatus, usize>,
    pub pruned: bool,
}

/// The request to start an Audit of a registered Source Set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartAudit {
    pub source_set: String,
    /// A prefix of the DAT Revision's SHA-256. Defaults to the latest Audit's DAT Revision, else the newest imported
    /// revision of the Source Set's DAT.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

impl JobState {
    pub const ALL: [JobState; 5] = [
        JobState::Queued,
        JobState::Running,
        JobState::Succeeded,
        JobState::Failed,
        JobState::Interrupted,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Succeeded => "succeeded",
            JobState::Failed => "failed",
            JobState::Interrupted => "interrupted",
        }
    }

    pub fn parse(s: &str) -> Option<JobState> {
        JobState::ALL.into_iter().find(|j| j.as_str() == s)
    }

    /// Whether the Job will not change state again.
    pub fn is_final(self) -> bool {
        matches!(
            self,
            JobState::Succeeded | JobState::Failed | JobState::Interrupted
        )
    }
}

/// The payload-reader and hash worker counts a Job runs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workers {
    pub readers: usize,
    pub hashers: usize,
}

/// A durable unit of work: so far, always an Audit of a Source Set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: i64,
    pub state: JobState,
    pub source_set: String,
    /// The DAT Revision the Audit is pinned to, by SHA-256.
    pub dat_revision: String,
    /// Who started it: the identity the trusted proxy handed over, or the Operator token's.
    pub actor: String,
    /// 1, or 2 for the Job queued again after a restart interrupted the first.
    pub attempt: u32,
    /// The interrupted Job this one was queued again for.
    pub requeued_from: Option<i64>,
    /// The Audit a Succeeded Job recorded.
    pub audit: Option<i64>,
    /// Why a Job Failed or was Interrupted.
    pub error: Option<String>,
    pub workers: Workers,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    /// Starts at 1 and goes up with each change of state, so that a client can tell a stale `state` from a newer one.
    pub version: u64,
}

/// How far a Running Audit has got through its Source Set's files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobProgress {
    pub job: i64,
    /// The `version` of the Running Job.
    pub version: u64,
    pub files_done: usize,
    pub files_total: usize,
}

/// The Jobs a client of `/events` should know about: every Queued or Running Job and the most recently finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSnapshot {
    pub jobs: Vec<Job>,
}

/// A server-sent event: the SSE event name is `state`, `progress` or `resync`, and its data is this, as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JobEvent {
    State(Box<Job>),
    Progress(JobProgress),
    /// Sent by `/events` when a client connects and whenever it fell behind and missed events: the Jobs now, to
    /// replace what the client knew.
    Resync(JobSnapshot),
}

impl JobEvent {
    /// The SSE event name.
    pub fn name(&self) -> &'static str {
        match self {
            JobEvent::State(_) => "state",
            JobEvent::Progress(_) => "progress",
            JobEvent::Resync(_) => "resync",
        }
    }

    /// The Job the event is about; none for a `resync`.
    pub fn job_id(&self) -> Option<i64> {
        match self {
            JobEvent::State(job) => Some(job.id),
            JobEvent::Progress(p) => Some(p.job),
            JobEvent::Resync(_) => None,
        }
    }
}

/// One page of a list with cursor paging. Pass `next_cursor` as `cursor` for the next page; it is absent on the last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

/// An Audit's Entry with its position in the report's `entries`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryRow {
    pub seq: u64,
    #[serde(flatten)]
    pub entry: Entry,
}

/// An Audit's Game Status with its position in the report's `games`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameRow {
    pub seq: u64,
    #[serde(flatten)]
    pub game: Game,
}
