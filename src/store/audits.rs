//! Stored Audits: run against a registered Source Set, kept as rows, pruned to summaries.

use std::collections::BTreeMap;

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params, params_from_iter};

use super::dats::{DatRevisionRecord, REVISION_COLUMNS, revision_from_row};
use super::sets::{SourceSetRow, resolve, set_by_name};
use super::{Error, Result, Store, now};
use crate::config::Config;
use crate::{
    ALGORITHMS, DatRevision, Entry, Evidence, EvidenceLevel, Finding, Game, GameStatus, Report,
    RomRef, Summary,
};

/// An Audit the store has recorded, with the report generated from its rows.
#[derive(Debug)]
pub struct StoredAudit {
    pub id: i64,
    pub report: Report,
}

/// One Audit in a Source Set's history. A pruned Audit keeps only this summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditSummary {
    pub id: i64,
    pub started_at: String,
    pub finished_at: String,
    pub dat_revision_sha256: String,
    pub dat_revision_version: String,
    pub findings: BTreeMap<Finding, usize>,
    pub games: BTreeMap<GameStatus, usize>,
    pub pruned: bool,
}

/// The count columns of `audits`, Findings then Game Statuses, named as the report names them.
fn count_columns(prefix: &str) -> String {
    Finding::ALL
        .iter()
        .map(|f| f.as_str())
        .chain(GameStatus::ALL.iter().map(|g| g.as_str()))
        .map(|c| format!("{prefix}{c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

type Counts = (BTreeMap<Finding, usize>, BTreeMap<GameStatus, usize>);

fn counts_from(row: &Row, first: usize) -> rusqlite::Result<Counts> {
    let mut findings = BTreeMap::new();
    for (i, f) in Finding::ALL.iter().enumerate() {
        findings.insert(*f, row.get::<_, i64>(first + i)? as usize);
    }
    let mut games = BTreeMap::new();
    for (i, g) in GameStatus::ALL.iter().enumerate() {
        games.insert(
            *g,
            row.get::<_, i64>(first + Finding::ALL.len() + i)? as usize,
        );
    }
    Ok((findings, games))
}

fn corrupt(what: impl std::fmt::Display) -> Error {
    Error::Io(format!("unexpected stored value: {what}"))
}

fn timestamp(s: &str) -> Result<jiff::Timestamp> {
    s.parse().map_err(|_| corrupt(s))
}

/// The DAT Revision an Audit of `set` uses: the one whose SHA-256 starts with `prefix`; else the latest
/// Audit's; else the newest imported revision of the bound DAT.
fn choose_revision(
    c: &Connection,
    set: &SourceSetRow,
    prefix: Option<&str>,
) -> Result<(i64, DatRevisionRecord)> {
    let base =
        format!("SELECT {REVISION_COLUMNS} FROM dat_revisions r JOIN dats d ON d.id = r.dat_id");
    let dat = &set.record.dat;
    if let Some(prefix) = prefix {
        let prefix = prefix.to_ascii_lowercase();
        if prefix.is_empty() || !prefix.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Err(Error::Refused(format!(
                "{prefix:?}: a DAT Revision is chosen by a prefix of its SHA-256"
            )));
        }
        let mut stmt = c.prepare(&format!(
            "{base} WHERE r.dat_id = ?1 AND substr(r.sha256, 1, length(?2)) = ?2 ORDER BY r.id"
        ))?;
        let mut found = stmt
            .query_map(params![set.dat_id, prefix], revision_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        return match found.len() {
            1 => Ok(found.remove(0)),
            0 => Err(Error::Refused(format!(
                "no DAT Revision of {dat} has a SHA-256 starting {prefix}"
            ))),
            _ => Err(Error::Refused(format!(
                "{prefix} matches more than one DAT Revision of {dat}"
            ))),
        };
    }
    let latest_audit = c
        .query_row(
            &format!(
                "{base} WHERE r.id = (SELECT dat_revision_id FROM audits WHERE source_set_id = ?1
                                      ORDER BY id DESC LIMIT 1)"
            ),
            [set.id],
            revision_from_row,
        )
        .optional()?;
    if let Some(found) = latest_audit {
        return Ok(found);
    }
    c.query_row(
        &format!("{base} WHERE r.dat_id = ?1 ORDER BY r.id DESC LIMIT 1"),
        [set.dat_id],
        revision_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::Refused(format!("no DAT Revision of {dat} has been imported")))
}

/// Records a finished Audit, its Entries and its Game Statuses.
fn insert_audit(tx: &Transaction, set_id: i64, revision_id: i64, report: &Report) -> Result<i64> {
    let mut values = vec![
        Value::Integer(set_id),
        Value::Integer(revision_id),
        Value::Text(report.source_set.clone()),
        Value::Text(report.started_at.to_string()),
        Value::Text(report.finished_at.to_string()),
    ];
    for f in Finding::ALL {
        values.push(Value::Integer(report.summary.findings[&f] as i64));
    }
    for g in GameStatus::ALL {
        values.push(Value::Integer(report.summary.games[&g] as i64));
    }
    let placeholders = vec!["?"; values.len()].join(", ");
    tx.execute(
        &format!(
            "INSERT INTO audits (source_set_id, dat_revision_id, source_set_path, started_at, finished_at, {})
             VALUES ({placeholders})",
            count_columns("")
        ),
        params_from_iter(values),
    )?;
    let id = tx.last_insert_rowid();

    let mut entry = tx.prepare(
        "INSERT INTO audit_entries (audit_id, seq, path, member, finding, roms, named_game, named_rom,
                                    evidence_level, algorithms, reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?;
    for (seq, e) in report.entries.iter().enumerate() {
        let roms = serde_json::to_string(&e.roms).map_err(corrupt)?;
        entry.execute(params![
            id,
            seq as i64,
            e.path,
            e.member,
            e.finding.as_str(),
            roms,
            e.named_rom.as_ref().map(|r| &r.game),
            e.named_rom.as_ref().map(|r| &r.name),
            e.evidence.as_ref().map(|ev| ev.level.as_str()),
            e.evidence.as_ref().map(|ev| ev.algorithms.join(",")),
            e.reason,
        ])?;
    }
    let mut game = tx.prepare(
        "INSERT INTO audit_games (audit_id, seq, name, status, missing_roms) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (seq, g) in report.games.iter().enumerate() {
        let missing = serde_json::to_string(&g.missing_roms).map_err(corrupt)?;
        game.execute(params![id, seq as i64, g.name, g.status.as_str(), missing])?;
    }
    Ok(id)
}

fn entry_from_row(row: &Row) -> Result<Entry> {
    let finding: String = row.get(2)?;
    let roms: String = row.get(3)?;
    let named_game: Option<String> = row.get(4)?;
    let named_name: Option<String> = row.get(5)?;
    let level: Option<String> = row.get(6)?;
    let algorithms: Option<String> = row.get(7)?;
    let evidence = match (level, algorithms) {
        (Some(level), Some(algorithms)) => Some(Evidence {
            level: EvidenceLevel::parse(&level).ok_or_else(|| corrupt(&level))?,
            algorithms: algorithms
                .split(',')
                .map(|a| {
                    ALGORITHMS
                        .into_iter()
                        .find(|k| *k == a)
                        .ok_or_else(|| corrupt(a))
                })
                .collect::<Result<_>>()?,
        }),
        _ => None,
    };
    Ok(Entry {
        path: row.get(0)?,
        member: row.get(1)?,
        finding: Finding::parse(&finding).ok_or_else(|| corrupt(&finding))?,
        roms: serde_json::from_str(&roms).map_err(corrupt)?,
        named_rom: match (named_game, named_name) {
            (Some(game), Some(name)) => Some(RomRef { game, name }),
            _ => None,
        },
        evidence,
        reason: row.get(8)?,
    })
}

fn game_from_row(row: &Row) -> Result<Game> {
    let status: String = row.get(1)?;
    let missing: String = row.get(2)?;
    Ok(Game {
        name: row.get(0)?,
        status: GameStatus::parse(&status).ok_or_else(|| corrupt(&status))?,
        missing_roms: serde_json::from_str(&missing).map_err(corrupt)?,
    })
}

impl Store {
    /// Audits the registered Source Set `name` against the DAT Revision chosen by `revision` (a SHA-256 prefix),
    /// or by default the latest Audit's, else the newest imported revision of its DAT. The Audit is recorded
    /// in one transaction when it finishes; older Audits are then pruned and unheld DAT blobs dropped.
    pub fn audit_source_set(
        &self,
        config: &Config,
        name: &str,
        revision: Option<&str>,
    ) -> Result<StoredAudit> {
        let (set, (revision_id, revision)) = self.read(|c| {
            let set = set_by_name(c, name)?;
            let revision = choose_revision(c, &set, revision)?;
            Ok((set, revision))
        })?;
        let (_, set_path) = resolve(config, &set.record.root, &set.record.path)?;
        if !revision.blob_present {
            return Err(Error::Refused(format!(
                "DAT Revision {} of {} is no longer kept; import its file ({}) again to restore it",
                revision.sha256,
                revision.dat(),
                revision.file
            )));
        }
        let blob = self.blob_path(&revision.sha256);
        let report = crate::audit_as(&blob, revision.file.clone(), &set_path)?;
        if report.dat_revision.sha256 != revision.sha256 {
            return Err(Error::Io(format!(
                "{}: the blob does not match its SHA-256",
                blob.display()
            )));
        }
        let id = self.write(|tx| insert_audit(tx, set.id, revision_id, &report))?;
        self.prune(set.id, config.audits.keep)?;
        self.retain_blobs()?;
        Ok(StoredAudit {
            id,
            report: self.report(id)?,
        })
    }

    /// The `schema: 2` report of a stored Audit, generated from its rows.
    pub fn report(&self, audit_id: i64) -> Result<Report> {
        self.read(|c| {
            let head = c
                .query_row(
                    &format!(
                        "SELECT a.source_set_path, a.started_at, a.finished_at, a.pruned_at IS NOT NULL,
                                r.sha256, d.name, r.version, r.file_name, {}
                         FROM audits a
                         JOIN dat_revisions r ON r.id = a.dat_revision_id
                         JOIN dats d ON d.id = r.dat_id
                         WHERE a.id = ?1",
                        count_columns("a.")
                    ),
                    [audit_id],
                    |r| {
                        Ok((
                            (
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, String>(2)?,
                                r.get::<_, bool>(3)?,
                            ),
                            DatRevision {
                                sha256: r.get(4)?,
                                name: r.get(5)?,
                                version: r.get(6)?,
                                file: r.get(7)?,
                            },
                            counts_from(r, 8)?,
                        ))
                    },
                )
                .optional()?
                .ok_or_else(|| Error::Refused(format!("no Audit {audit_id}")))?;
            let ((source_set, started_at, finished_at, pruned), dat_revision, (findings, games)) =
                head;
            if pruned {
                return Err(Error::Refused(format!(
                    "Audit {audit_id} was pruned; only its summary is kept"
                )));
            }
            let mut stmt = c.prepare(
                "SELECT path, member, finding, roms, named_game, named_rom, evidence_level, algorithms, reason
                 FROM audit_entries WHERE audit_id = ?1 ORDER BY seq",
            )?;
            let mut rows = stmt.query([audit_id])?;
            let mut entries = Vec::new();
            while let Some(row) = rows.next()? {
                entries.push(entry_from_row(row)?);
            }
            let mut stmt = c.prepare(
                "SELECT name, status, missing_roms FROM audit_games WHERE audit_id = ?1 ORDER BY seq",
            )?;
            let mut rows = stmt.query([audit_id])?;
            let mut game_rows = Vec::new();
            while let Some(row) = rows.next()? {
                game_rows.push(game_from_row(row)?);
            }
            Ok(Report {
                schema: 2,
                dat_revision,
                source_set,
                started_at: timestamp(&started_at)?,
                finished_at: timestamp(&finished_at)?,
                summary: Summary { findings, games },
                entries,
                games: game_rows,
            })
        })
    }

    /// The Audits of the Source Set `name`, newest first.
    pub fn history(&self, name: &str) -> Result<Vec<AuditSummary>> {
        self.read(|c| {
            let set = set_by_name(c, name)?;
            let mut stmt = c.prepare(&format!(
                "SELECT a.id, a.started_at, a.finished_at, r.sha256, r.version, a.pruned_at IS NOT NULL, {}
                 FROM audits a JOIN dat_revisions r ON r.id = a.dat_revision_id
                 WHERE a.source_set_id = ?1 ORDER BY a.id DESC",
                count_columns("a.")
            ))?;
            let rows = stmt.query_map([set.id], |r| {
                let (findings, games) = counts_from(r, 6)?;
                Ok(AuditSummary {
                    id: r.get(0)?,
                    started_at: r.get(1)?,
                    finished_at: r.get(2)?,
                    dat_revision_sha256: r.get(3)?,
                    dat_revision_version: r.get(4)?,
                    pruned: r.get(5)?,
                    findings,
                    games,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Prunes the Source Set's full Audits beyond the latest `keep` to their summary rows, except any cited as
    /// provenance. The latest Audit is always kept.
    fn prune(&self, set_id: i64, keep: usize) -> Result<Vec<i64>> {
        self.write(|tx| {
            let mut stmt = tx.prepare(
                "SELECT id, EXISTS (SELECT 1 FROM audit_citations c WHERE c.audit_id = a.id)
                 FROM audits a WHERE source_set_id = ?1 AND pruned_at IS NULL ORDER BY id DESC",
            )?;
            let full = stmt
                .query_map([set_id], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let pruned: Vec<i64> = full
                .into_iter()
                .skip(keep.max(1))
                .filter(|(_, cited)| !cited)
                .map(|(id, _)| id)
                .collect();
            let at = now();
            for id in &pruned {
                tx.execute("DELETE FROM audit_entries WHERE audit_id = ?1", [id])?;
                tx.execute("DELETE FROM audit_games WHERE audit_id = ?1", [id])?;
                tx.execute(
                    "UPDATE audits SET pruned_at = ?1 WHERE id = ?2",
                    params![at, id],
                )?;
            }
            Ok(pruned)
        })
    }
}
