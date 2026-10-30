//! Audits a Source Set against a DAT Revision (see GLOSSARY.md and ADR 0002).

pub mod dat;
mod payload;
mod read;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use title_select_api::Workers;
pub use title_select_api::report::*;

use dat::Dat;
use payload::Checksums;
use read::FileRead;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot read DAT: {0}")]
    Dat(String),
    #[error("cannot read source set: {0}")]
    SourceSet(String),
}

/// One payload reader and one hash worker: what the stateless Audit uses.
pub const ONE_WORKER_EACH: Workers = Workers {
    readers: 1,
    hashers: 1,
};

/// How far an Audit has got: files whose Entries are read, of the files found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub files_done: usize,
    pub files_total: usize,
}

/// How an Audit runs.
pub struct Options<'a> {
    /// The DAT Revision's original filename, as the report names it.
    pub file: String,
    /// A canonical path every file and directory must resolve inside. One that resolves outside is an Unreadable
    /// Entry and is not read, and the file read is the one its resolved path names.
    pub confine: Option<&'a Path>,
    /// At least one of each: `readers` files are read at once, and `hashers` threads checksum their payloads.
    pub workers: Workers,
    /// Called as each file's Entries are read, from the reader threads.
    pub progress: Option<&'a (dyn Fn(Progress) + Sync)>,
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Audits the Source Set at `root` against the DAT Revision at `dat_path`, reading every payload.
pub fn audit(dat_path: &Path, root: &Path) -> Result<Report, Error> {
    let file = dat_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    audit_with(
        dat_path,
        root,
        &Options {
            file,
            confine: None,
            workers: ONE_WORKER_EACH,
            progress: None,
        },
    )
}

/// What the walk found, in report order.
enum Item {
    Unreadable { rel: String, reason: String },
    File { rel: String },
}

/// Like [`audit`], with the DAT Revision's filename, confinement, worker counts and progress given by `options`.
pub fn audit_with(dat_path: &Path, root: &Path, options: &Options) -> Result<Report, Error> {
    let started_at = jiff::Timestamp::now();
    if !root.is_dir() {
        return Err(Error::SourceSet(format!(
            "{}: not a directory",
            root.display()
        )));
    }
    let dat = Dat::read(dat_path)?;
    let rel = |p: &Path| {
        p.strip_prefix(root)
            .unwrap_or(p)
            .to_string_lossy()
            .into_owned()
    };
    let mut items = Vec::new();
    let mut files: Vec<(PathBuf, bool)> = Vec::new();
    let mut walk = walkdir::WalkDir::new(root)
        .follow_links(true)
        .sort_by_file_name()
        .into_iter();
    while let Some(item) = walk.next() {
        let item = match item {
            Ok(item) => item,
            Err(err) if err.depth() == 0 => return Err(Error::SourceSet(err.to_string())),
            Err(err) => {
                let path = err.path().map(rel).unwrap_or_default();
                items.push(Item::Unreadable {
                    rel: path,
                    reason: err.to_string(),
                });
                continue;
            }
        };
        let mut path = item.path().to_path_buf();
        if let Some(confine) = options.confine {
            let refusal = match item.path().canonicalize() {
                Ok(real) if real.starts_with(confine) => {
                    path = real;
                    None
                }
                Ok(_) => Some("resolves outside the Root".to_owned()),
                Err(err) => Some(err.to_string()),
            };
            if let Some(reason) = refusal {
                if item.depth() == 0 {
                    return Err(Error::SourceSet(format!("{}: {reason}", root.display())));
                }
                if item.file_type().is_dir() {
                    walk.skip_current_dir();
                }
                items.push(Item::Unreadable {
                    rel: rel(item.path()),
                    reason,
                });
                continue;
            }
        }
        if !item.file_type().is_file() {
            continue;
        }
        let is_zip = item
            .path()
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"));
        items.push(Item::File {
            rel: rel(item.path()),
        });
        files.push((path, is_zip));
    }

    let mut reads = read::read_files(&files, options.workers, options.progress).into_iter();
    let mut scan = Scan {
        covered: vec![false; dat.roms.len()],
        entries: Vec::new(),
        dat: &dat,
    };
    for item in items {
        match item {
            Item::Unreadable { rel, reason } => scan.unreadable(rel, None, reason),
            Item::File { rel } => {
                let FileRead { members, error } = reads.next().expect("one read per file");
                for (member, sums) in members {
                    match sums {
                        Ok(sums) => scan.record(rel.clone(), member, &sums),
                        Err(reason) => scan.unreadable(rel.clone(), member, reason),
                    }
                }
                if let Some(reason) = error {
                    scan.unreadable(rel, None, reason);
                }
            }
        }
    }
    let games = scan.games();
    let mut findings: BTreeMap<_, _> = Finding::ALL.iter().map(|f| (*f, 0)).collect();
    for e in &scan.entries {
        *findings.entry(e.finding).or_default() += 1;
    }
    let mut statuses: BTreeMap<_, _> = GameStatus::ALL.iter().map(|g| (*g, 0)).collect();
    for g in &games {
        *statuses.entry(g.status).or_default() += 1;
    }
    Ok(Report {
        schema: SCHEMA,
        dat_revision: DatRevision {
            sha256: dat.sha256.clone(),
            name: dat.name.clone(),
            version: dat.version.clone(),
            file: options.file.clone(),
        },
        source_set: root.display().to_string(),
        started_at,
        finished_at: jiff::Timestamp::now(),
        summary: Summary {
            findings,
            games: statuses,
        },
        entries: scan.entries,
        games,
    })
}

/// What an Audit has found so far.
struct Scan<'d> {
    dat: &'d Dat,
    covered: Vec<bool>,
    entries: Vec<Entry>,
}

impl Scan<'_> {
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
            evidence: Some(evidence(self.dat, &payload)),
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

/// For a payload match, size plus each checksum that every matched ROM supplies, since every supplied
/// checksum was compared and agreed. Without one, only the size-and-CRC32 lookup that found no candidate.
fn evidence(dat: &Dat, roms: &[usize]) -> Evidence {
    if roms.is_empty() {
        return Evidence {
            level: EvidenceLevel::Payload,
            algorithms: vec!["size".into(), "crc32".into()],
        };
    }
    let all_supply = |has: fn(&dat::Rom) -> bool| roms.iter().all(|r| has(&dat.roms[*r]));
    let mut algorithms = vec!["size".into()];
    for (name, has) in [
        ("crc32", (|r| r.crc.is_some()) as fn(&dat::Rom) -> bool),
        ("md5", |r| r.md5.is_some()),
        ("sha1", |r| r.sha1.is_some()),
        ("sha256", |r| r.sha256.is_some()),
    ] {
        if all_supply(has) {
            algorithms.push(name.into());
        }
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
    // Every ROM with this name and payload, so an identical ROM shared by several Games counts for each.
    let both: Vec<usize> = named
        .iter()
        .copied()
        .filter(|r| payload.contains(r))
        .collect();
    if !both.is_empty() {
        return (Finding::Matched, both, None);
    }
    match (payload.is_empty(), named.first()) {
        (false, None) => (Finding::Misnamed, payload, None),
        (false, Some(n)) => (Finding::Conflicting, payload, Some(*n)),
        (true, Some(n)) => (Finding::Mismatched, payload, Some(*n)),
        (true, None) => (Finding::Unmatched, payload, None),
    }
}

/// The ROMs whose every supplied checksum equals the payload's.
fn matching_roms(dat: &Dat, sums: &Checksums) -> Vec<usize> {
    dat.candidates(sums.size, &sums.crc)
        .filter(|r| sums.matches(&dat.roms[*r]))
        .collect()
}
