//! Audits a Verified Set against a DAT Revision (see GLOSSARY.md and ADR 0002).

mod dat;
mod payload;

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use serde::Serialize;

use dat::Dat;
use payload::Checksums;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot read DAT: {0}")]
    Dat(String),
    #[error("cannot read Verified Set: {0}")]
    VerifiedSet(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Finding {
    Matched,
    Misnamed,
    Conflicting,
    Bad,
    Extra,
    Unreadable,
}

impl Finding {
    const ALL: [Finding; 6] = [
        Finding::Matched,
        Finding::Misnamed,
        Finding::Conflicting,
        Finding::Bad,
        Finding::Extra,
        Finding::Unreadable,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GameStatus {
    Complete,
    Incomplete,
    Missing,
}

impl GameStatus {
    const ALL: [GameStatus; 3] = [
        GameStatus::Complete,
        GameStatus::Incomplete,
        GameStatus::Missing,
    ];
}

/// Only Payload exists so far: every Entry's bytes are read and checksummed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceLevel {
    Payload,
}

/// What the Audit actually checked for an Entry.
#[derive(Debug, Serialize)]
pub struct Evidence {
    pub level: EvidenceLevel,
    /// The size and checksum algorithms the Finding rests on, in the order size, crc32, md5, sha1, sha256.
    pub algorithms: Vec<&'static str>,
}

/// The DAT Revision an Audit is pinned to.
#[derive(Debug, Serialize)]
pub struct DatRevision {
    /// SHA-256 of the unmodified DAT file; the DAT Revision's identity.
    pub sha256: String,
    pub name: String,
    pub version: String,
    pub file: String,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub findings: BTreeMap<Finding, usize>,
    pub games: BTreeMap<GameStatus, usize>,
}

/// A ROM of a DAT Revision, by its Game and name.
#[derive(Debug, Clone, Serialize)]
pub struct RomRef {
    pub game: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct Entry {
    /// Path relative to the Verified Set's root.
    pub path: String,
    /// The member's name, when the Entry is a member of a ZIP archive at `path`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    pub finding: Finding,
    /// The ROMs this Entry's payload matches.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub roms: Vec<RomRef>,
    /// For a Conflicting or Bad Entry, the ROM its name gives.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub named_rom: Option<RomRef>,
    /// Absent for an Unreadable Entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    /// Why an Unreadable Entry could not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Game {
    pub name: String,
    pub status: GameStatus,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing_roms: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    /// Version of this report's shape.
    pub schema: u32,
    pub dat_revision: DatRevision,
    pub verified_set: String,
    pub started_at: jiff::Timestamp,
    pub finished_at: jiff::Timestamp,
    pub summary: Summary,
    pub entries: Vec<Entry>,
    pub games: Vec<Game>,
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Audits the Verified Set at `root` against the DAT Revision at `dat_path`, reading every payload.
pub fn audit(dat_path: &Path, root: &Path) -> Result<Report, Error> {
    let started_at = jiff::Timestamp::now();
    if !root.is_dir() {
        return Err(Error::VerifiedSet(format!(
            "{}: not a directory",
            root.display()
        )));
    }
    let dat = Dat::read(dat_path)?;
    let mut audit = Audit {
        covered: vec![false; dat.roms.len()],
        entries: Vec::new(),
        dat: &dat,
    };
    for item in walkdir::WalkDir::new(root).sort_by_file_name() {
        let item = item.map_err(|err| Error::VerifiedSet(err.to_string()))?;
        if !item.file_type().is_file() {
            continue;
        }
        let rel = item
            .path()
            .strip_prefix(root)
            .unwrap_or(item.path())
            .to_string_lossy()
            .into_owned();
        let is_zip = item
            .path()
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"));
        if let Err(reason) = audit.read_file(item.path(), &rel, is_zip) {
            audit.unreadable(rel, None, reason);
        }
    }
    let games = audit.games();
    let mut findings: BTreeMap<_, _> = Finding::ALL.iter().map(|f| (*f, 0)).collect();
    for e in &audit.entries {
        *findings.entry(e.finding).or_default() += 1;
    }
    let mut statuses: BTreeMap<_, _> = GameStatus::ALL.iter().map(|g| (*g, 0)).collect();
    for g in &games {
        *statuses.entry(g.status).or_default() += 1;
    }
    Ok(Report {
        schema: 1,
        dat_revision: DatRevision {
            sha256: dat.sha256.clone(),
            name: dat.name.clone(),
            version: dat.version.clone(),
            file: dat_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        },
        verified_set: root.display().to_string(),
        started_at,
        finished_at: jiff::Timestamp::now(),
        summary: Summary {
            findings,
            games: statuses,
        },
        entries: audit.entries,
        games,
    })
}

struct Audit<'d> {
    dat: &'d Dat,
    covered: Vec<bool>,
    entries: Vec<Entry>,
}

impl Audit<'_> {
    /// Records the Entries of one file. An error is about the file as a whole; a member that fails to read
    /// is recorded as Unreadable here and the rest of the archive is still read.
    fn read_file(&mut self, path: &Path, rel: &str, is_zip: bool) -> Result<(), String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        if !is_zip {
            let sums = Checksums::of_reader(file).map_err(|e| e.to_string())?;
            self.record(rel.to_owned(), None, &sums);
            return Ok(());
        }
        let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        for i in 0..zip.len() {
            let member = zip.by_index(i).map_err(|e| e.to_string())?;
            if member.is_dir() {
                continue;
            }
            let name = member.name().map_err(|e| e.to_string())?.into_owned();
            match Checksums::of_reader(member) {
                Ok(sums) => self.record(rel.to_owned(), Some(name), &sums),
                Err(e) => self.unreadable(rel.to_owned(), Some(name), e.to_string()),
            }
        }
        Ok(())
    }

    fn unreadable(&mut self, path: String, member: Option<String>, reason: String) {
        self.entries.push(Entry {
            path,
            member,
            finding: Finding::Unreadable,
            roms: Vec::new(),
            named_rom: None,
            evidence: None,
            reason: Some(reason),
        });
    }

    fn record(&mut self, path: String, member: Option<String>, sums: &Checksums) {
        let full = member.as_deref().unwrap_or(&path);
        let name = full.rsplit(['/', '\\']).next().unwrap_or(full);
        let (finding, payload, named) = classify(self.dat, name, sums);
        for r in &payload {
            self.covered[*r] = true;
        }
        self.entries.push(Entry {
            path,
            member,
            finding,
            evidence: Some(evidence(
                self.dat,
                if payload.is_empty() {
                    named.as_slice()
                } else {
                    &payload
                },
            )),
            roms: payload.iter().map(|r| rom_ref(self.dat, *r)).collect(),
            named_rom: named.map(|r| rom_ref(self.dat, r)),
            reason: None,
        });
    }

    fn games(&self) -> Vec<Game> {
        let mut roms_of: Vec<Vec<usize>> = vec![Vec::new(); self.dat.games.len()];
        for (r, rom) in self.dat.roms.iter().enumerate() {
            roms_of[rom.game].push(r);
        }
        self.dat
            .games
            .iter()
            .zip(roms_of)
            .map(|(name, roms)| {
                let missing_roms: Vec<String> = roms
                    .iter()
                    .filter(|r| !self.covered[**r])
                    .map(|r| self.dat.roms[*r].name.clone())
                    .collect();
                let status = match missing_roms.len() {
                    0 => GameStatus::Complete,
                    n if n == roms.len() => GameStatus::Missing,
                    _ => GameStatus::Incomplete,
                };
                Game {
                    name: name.clone(),
                    status,
                    missing_roms,
                }
            })
            .collect()
    }
}

/// Size and crc32 are always compared, since candidates are found by them; each other algorithm counts
/// only when every ROM the Finding rests on supplies it.
fn evidence(dat: &Dat, roms: &[usize]) -> Evidence {
    let all_supply =
        |has: fn(&dat::Rom) -> bool| !roms.is_empty() && roms.iter().all(|r| has(&dat.roms[*r]));
    let mut algorithms = vec!["size", "crc32"];
    if all_supply(|r| r.md5.is_some()) {
        algorithms.push("md5");
    }
    if all_supply(|r| r.sha1.is_some()) {
        algorithms.push("sha1");
    }
    if all_supply(|r| r.sha256.is_some()) {
        algorithms.push("sha256");
    }
    Evidence {
        level: EvidenceLevel::Payload,
        algorithms,
    }
}

fn rom_ref(dat: &Dat, r: usize) -> RomRef {
    RomRef {
        game: dat.games[dat.roms[r].game].clone(),
        name: dat.roms[r].name.clone(),
    }
}

/// Decides an Entry's Finding by payload before name (ADR 0002). Returns the Finding, the ROMs the payload
/// matches, and the ROM the name gives when that differs from the payload's.
fn classify(dat: &Dat, name: &str, sums: &Checksums) -> (Finding, Vec<usize>, Option<usize>) {
    let payload = matching_roms(dat, sums);
    let named = dat.roms_named(name);
    if let Some(r) = named.iter().find(|r| payload.contains(r)) {
        return (Finding::Matched, vec![*r], None);
    }
    match (payload.is_empty(), named.first()) {
        (false, None) => (Finding::Misnamed, payload, None),
        (false, Some(n)) => (Finding::Conflicting, payload, Some(*n)),
        (true, Some(n)) => (Finding::Bad, payload, Some(*n)),
        (true, None) => (Finding::Extra, payload, None),
    }
}

/// The ROMs whose every supplied checksum equals the payload's.
fn matching_roms(dat: &Dat, sums: &Checksums) -> Vec<usize> {
    dat.roms_with_size_crc(sums.size, &sums.crc)
        .iter()
        .copied()
        .filter(|r| sums.matches(&dat.roms[*r]))
        .collect()
}
