//! The `schema: 2` Audit report (see GLOSSARY.md and ADR 0002).

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The report shape this version writes.
pub const SCHEMA: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Finding {
    Matched,
    Misnamed,
    Conflicting,
    Mismatched,
    Unmatched,
    Unreadable,
}

impl Finding {
    pub const ALL: [Finding; 6] = [
        Finding::Matched,
        Finding::Misnamed,
        Finding::Conflicting,
        Finding::Mismatched,
        Finding::Unmatched,
        Finding::Unreadable,
    ];

    /// The name the report uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Finding::Matched => "matched",
            Finding::Misnamed => "misnamed",
            Finding::Conflicting => "conflicting",
            Finding::Mismatched => "mismatched",
            Finding::Unmatched => "unmatched",
            Finding::Unreadable => "unreadable",
        }
    }

    pub fn parse(s: &str) -> Option<Finding> {
        Finding::ALL.into_iter().find(|f| f.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameStatus {
    Complete,
    Incomplete,
    Missing,
}

impl GameStatus {
    pub const ALL: [GameStatus; 3] = [
        GameStatus::Complete,
        GameStatus::Incomplete,
        GameStatus::Missing,
    ];

    /// The name the report uses.
    pub fn as_str(self) -> &'static str {
        match self {
            GameStatus::Complete => "complete",
            GameStatus::Incomplete => "incomplete",
            GameStatus::Missing => "missing",
        }
    }

    pub fn parse(s: &str) -> Option<GameStatus> {
        GameStatus::ALL.into_iter().find(|g| g.as_str() == s)
    }
}

/// Only Payload exists so far: every Entry's bytes are read and checksummed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceLevel {
    Payload,
}

impl EvidenceLevel {
    /// The name the report uses.
    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceLevel::Payload => "payload",
        }
    }

    pub fn parse(s: &str) -> Option<EvidenceLevel> {
        (s == "payload").then_some(EvidenceLevel::Payload)
    }
}

/// Every algorithm an Evidence can list, in report order.
pub const ALGORITHMS: [&str; 5] = ["size", "crc32", "md5", "sha1", "sha256"];

/// What the Audit actually checked for an Entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub level: EvidenceLevel,
    /// The size and checksum algorithms the Finding rests on, in the order size, crc32, md5, sha1, sha256.
    pub algorithms: Vec<Cow<'static, str>>,
}

/// The DAT Revision an Audit is pinned to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatRevision {
    /// SHA-256 of the unmodified DAT file; the DAT Revision's identity.
    pub sha256: String,
    pub name: String,
    pub version: String,
    pub file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub findings: BTreeMap<Finding, usize>,
    pub games: BTreeMap<GameStatus, usize>,
}

/// A ROM of a DAT Revision, by its Game and name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RomRef {
    pub game: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Path relative to the Source Set's root.
    pub path: String,
    /// The member's name, when the Entry is a member of a ZIP archive at `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    pub finding: Finding,
    /// The ROMs this Entry's payload matches.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roms: Vec<RomRef>,
    /// For a Conflicting or Mismatched Entry, the ROM its name gives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub named_rom: Option<RomRef>,
    /// Absent for an Unreadable Entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    /// Why an Unreadable Entry could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Game {
    pub name: String,
    pub status: GameStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_roms: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// Version of this report's shape.
    pub schema: u32,
    pub dat_revision: DatRevision,
    pub source_set: String,
    pub started_at: jiff::Timestamp,
    pub finished_at: jiff::Timestamp,
    pub summary: Summary,
    pub entries: Vec<Entry>,
    pub games: Vec<Game>,
}
