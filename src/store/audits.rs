//! Stored Audits: run against a registered Source Set, kept as rows, pruned to summaries.

use std::collections::BTreeMap;

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params, params_from_iter};

use super::dats::{DatRevisionRecord, REVISION_COLUMNS, revision_from_row};
use super::sets::{Resolved, SourceSetRow, resolve, set_by_name};
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

/// An Audit whose row is inserted as running, before its scan.
struct RunningAudit {
    id: i64,
    set_id: i64,
    resolved: Resolved,
    revision: DatRevisionRecord,
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
                "{base} WHERE r.id = (SELECT dat_revision_id FROM audits
                                      WHERE source_set_id = ?1 AND status = 'finished'
                                      ORDER BY finish_seq DESC LIMIT 1)"
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

/// Finishes the running Audit `id`: its counts and times, its Entries and its Game Statuses.
fn finish_audit(tx: &Transaction, id: i64, report: &Report) -> Result<()> {
    let mut values = vec![
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
    let counts: Vec<String> = Finding::ALL
        .iter()
        .map(|f| f.as_str())
        .chain(GameStatus::ALL.iter().map(|g| g.as_str()))
        .map(|c| format!("{c} = ?"))
        .collect();
    values.push(Value::Integer(id));
    let updated = tx.execute(
        &format!(
            "UPDATE audits SET status = 'finished',
                               finish_seq = (SELECT coalesce(max(finish_seq), 0) + 1 FROM audits),
                               source_set_path = ?, started_at = ?, finished_at = ?, {}
             WHERE id = ? AND status = 'running'",
            counts.join(", ")
        ),
        params_from_iter(values),
    )?;
    if updated != 1 {
        return Err(Error::Io(format!("Audit {id} is no longer running")));
    }
    // The Audit now holds its revision, so a restored blob needs no other hold.
    tx.execute(
        "UPDATE dat_revisions SET restored_at = NULL
         WHERE id = (SELECT dat_revision_id FROM audits WHERE id = ?1)",
        [id],
    )?;

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
    Ok(())
}

/// Deletes the Audits matching `condition`, with their Entries and Game Statuses.
fn delete_audits(
    tx: &Transaction,
    condition: &str,
    params: impl rusqlite::Params + Copy,
) -> Result<usize> {
    for table in ["audit_entries", "audit_games", "audit_citations"] {
        tx.execute(
            &format!(
                "DELETE FROM {table} WHERE audit_id IN (SELECT id FROM audits WHERE {condition})"
            ),
            params,
        )?;
    }
    Ok(tx.execute(&format!("DELETE FROM audits WHERE {condition}"), params)?)
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

/// The report of the finished Audit `audit_id`, generated from its rows on `c`.
fn read_report(c: &Connection, audit_id: i64) -> Result<Report> {
    let head = c
        .query_row(
            &format!(
                "SELECT a.source_set_path, a.started_at, a.finished_at, a.pruned_at IS NOT NULL,
                        r.sha256, d.name, r.version, r.file_name, {}
                 FROM audits a
                 JOIN dat_revisions r ON r.id = a.dat_revision_id
                 JOIN dats d ON d.id = r.dat_id
                 WHERE a.id = ?1 AND a.status = 'finished'",
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
        .ok_or_else(|| Error::Refused(format!("no finished Audit {audit_id}")))?;
    let ((source_set, started_at, finished_at, pruned), dat_revision, (findings, games)) = head;
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
        let running = self.begin_audit(config, name, revision)?;
        self.complete_audit(config, running)
    }

    /// Chooses the revision and inserts the running row in one transaction, which pins the blob: retention, which
    /// also runs under the writer, treats a running Audit as a holder.
    fn begin_audit(
        &self,
        config: &Config,
        name: &str,
        revision: Option<&str>,
    ) -> Result<RunningAudit> {
        let set = self.read(|c| set_by_name(c, name))?;
        let resolved = resolve(config, &set.record.root, &set.record.path)?;
        let source_set_path = resolved.path.display().to_string();
        let (id, revision) = self.write(|tx| {
            let (revision_id, revision) = choose_revision(tx, &set, revision)?;
            if !revision.blob_present {
                return Err(Error::Refused(format!(
                    "DAT Revision {} of {} is no longer kept; import its file ({}) again to restore it",
                    revision.sha256,
                    revision.dat(),
                    revision.file
                )));
            }
            tx.execute(
                "INSERT INTO audits (source_set_id, dat_revision_id, status, source_set_path, started_at)
                 VALUES (?1, ?2, 'running', ?3, ?4)",
                params![set.id, revision_id, source_set_path, now()],
            )?;
            Ok((tx.last_insert_rowid(), revision))
        })?;
        Ok(RunningAudit {
            id,
            set_id: set.id,
            resolved,
            revision,
        })
    }

    /// Finishes a running Audit, then prunes the Source Set's Audits and drops unheld DAT blobs.
    fn complete_audit(&self, config: &Config, running: RunningAudit) -> Result<StoredAudit> {
        let set_id = running.set_id;
        let stored = self.finish(running)?;
        self.prune(set_id, config.audits.keep)?;
        self.retain_blobs()?;
        Ok(stored)
    }

    /// Scans a running Audit and finishes it. Its report is read back in the finishing transaction, so no other
    /// Audit's pruning can come between.
    fn finish(&self, running: RunningAudit) -> Result<StoredAudit> {
        let id = running.id;
        let finished = self
            .scan(&running.revision, &running.resolved)
            .and_then(|report| {
                self.write(|tx| {
                    finish_audit(tx, id, &report)?;
                    read_report(tx, id)
                })
            });
        match finished {
            Ok(report) => Ok(StoredAudit { id, report }),
            Err(err) => {
                // Best effort: a running row that survives this is deleted when the store next opens.
                let _ = self.write(|tx| delete_audits(tx, "id = ?1", [id]));
                Err(err)
            }
        }
    }

    /// Reads the Source Set's files against the revision's blob, confined to the Root.
    fn scan(&self, revision: &DatRevisionRecord, resolved: &Resolved) -> Result<Report> {
        let blob = self.blob_path(&revision.sha256);
        let report = crate::audit_as(
            &blob,
            revision.file.clone(),
            &resolved.path,
            Some(&resolved.canonical_root),
        )?;
        if report.dat_revision.sha256 != revision.sha256 {
            return Err(Error::Io(format!(
                "{}: the blob does not match its SHA-256",
                blob.display()
            )));
        }
        Ok(report)
    }

    /// Deletes the rows of Audits left running by a process that stopped mid-Audit.
    pub(super) fn discard_unfinished_audits(&self) -> Result<usize> {
        self.write(|tx| delete_audits(tx, "status = 'running'", []))
    }

    /// The `schema: 2` report of a stored Audit, generated from its rows.
    pub fn report(&self, audit_id: i64) -> Result<Report> {
        self.read(|conn| {
            // One read transaction, so a concurrent prune cannot leave the counts without their rows. It only
            // reads, so ending it by rollback when it drops is enough.
            let tx = conn.unchecked_transaction()?;
            read_report(&tx, audit_id)
        })
    }

    /// The Audits of the Source Set `name`, the latest to finish first.
    pub fn history(&self, name: &str) -> Result<Vec<AuditSummary>> {
        self.read(|c| {
            let set = set_by_name(c, name)?;
            let mut stmt = c.prepare(&format!(
                "SELECT a.id, a.started_at, a.finished_at, r.sha256, r.version, a.pruned_at IS NOT NULL, {}
                 FROM audits a JOIN dat_revisions r ON r.id = a.dat_revision_id
                 WHERE a.source_set_id = ?1 AND a.status = 'finished' ORDER BY a.finish_seq DESC",
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

    /// Prunes the Source Set's full Audits beyond the latest `keep` to finish to their summary rows, except any
    /// cited as provenance. The latest Audit is always kept.
    fn prune(&self, set_id: i64, keep: usize) -> Result<Vec<i64>> {
        self.write(|tx| {
            let mut stmt = tx.prepare(
                "SELECT id, EXISTS (SELECT 1 FROM audit_citations c WHERE c.audit_id = a.id)
                 FROM audits a
                 WHERE source_set_id = ?1 AND status = 'finished' AND pruned_at IS NULL
                 ORDER BY finish_seq DESC",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A store with keep = 1 and one Source Set `Test` in a Root, bound to an imported DAT.
    fn store_with_a_set() -> (tempfile::TempDir, Store, Config) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("sets");
        std::fs::create_dir_all(root.join("Test")).unwrap();
        let dat = tmp.path().join("Test.dat");
        std::fs::write(
            &dat,
            "<datafile><header><name>Test</name><version>1</version></header></datafile>",
        )
        .unwrap();
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\n\n[audits]\nkeep = 1\n",
            root.display().to_string()
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        store.import_dat(&dat, "Test", None).unwrap();
        store
            .add_source_set(&config, "sets", "Test", "Test/Test")
            .unwrap();

        (tmp, store, config)
    }

    #[test]
    fn the_latest_audit_is_the_last_to_finish_and_is_never_pruned_by_its_own_completion() {
        let (_tmp, store, config) = store_with_a_set();
        let first = store.begin_audit(&config, "Test", None).unwrap();
        let second = store.begin_audit(&config, "Test", None).unwrap();
        let (first_id, second_id) = (first.id, second.id);
        store.complete_audit(&config, second).unwrap();
        let completed = store.complete_audit(&config, first).unwrap();

        assert_eq!(completed.id, first_id);
        let history = store.history("Test").unwrap();
        assert_eq!(
            history.iter().map(|a| (a.id, a.pruned)).collect::<Vec<_>>(),
            vec![(first_id, false), (second_id, true)],
            "the Audit that finished last is the latest"
        );
        assert!(store.report(first_id).is_ok());
        let set = &store.source_sets().unwrap()[0];
        let last = set.last_verified_against.as_ref().unwrap();
        assert_eq!(last.finished_at, history[0].finished_at);
    }

    #[test]
    fn a_finished_audits_report_survives_another_audit_pruning_it_before_its_own_pruning() {
        let (_tmp, store, config) = store_with_a_set();
        let first = store.begin_audit(&config, "Test", None).unwrap();
        let second = store.begin_audit(&config, "Test", None).unwrap();
        let (first_id, set_id) = (first.id, first.set_id);

        let finished = store.finish(first).unwrap();
        store.complete_audit(&config, second).unwrap();
        store.prune(set_id, config.audits.keep).unwrap();

        assert_eq!(finished.id, first_id);
        assert_eq!(finished.report.dat_revision.name, "Test");
        assert!(
            store.history("Test").unwrap()[1].pruned,
            "the other Audit finished later and pruned it"
        );
        assert!(matches!(store.report(first_id), Err(Error::Refused(_))));
    }
}
