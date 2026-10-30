//! Durable Jobs: Queued, Running, Succeeded, Failed or Interrupted, each state change one transaction (ADR 0003).

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use super::audits::{check_blob_present, choose_revision, delete_audits};
use super::sets::set_by_name;
use super::{Error, Result, Store, now};
use crate::config::Config;
use title_select_api::{Job, JobState, Workers};
use title_select_engine::Progress;

/// Why a restart Interrupted a Job.
const INTERRUPTED: &str = "the Service stopped while the job was running";
/// Why a restart Failed a Job that was already queued again once.
const INTERRUPTED_AGAIN: &str =
    "the Service stopped while the job was running, after it had already been queued again once";

/// What starting the Service did to a Job it found Running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interruption {
    /// The Job as it now is: Interrupted, or Failed when it was already the Job queued again.
    pub job: Job,
    /// The fresh Job queued in its place, if any.
    pub requeued: Option<Job>,
}

/// A Job's row, with the ids the store needs to run it.
pub(super) struct JobRow {
    pub id: i64,
    pub source_set_id: i64,
    pub dat_revision_id: i64,
    pub workers: Workers,
}

const JOB_QUERY: &str =
    "SELECT j.id, j.state, s.name, r.sha256, j.actor, j.attempt, j.requeued_from, j.audit_id,
            j.error, j.readers, j.hashers, j.created_at, j.started_at, j.finished_at, j.version
     FROM jobs j
     JOIN source_sets s ON s.id = j.source_set_id
     JOIN dat_revisions r ON r.id = j.dat_revision_id";

fn job_from_row(row: &Row) -> rusqlite::Result<Job> {
    let state: String = row.get(1)?;
    let state = JobState::parse(&state).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            format!("unknown job state {state}").into(),
        )
    })?;
    Ok(Job {
        id: row.get(0)?,
        state,
        source_set: row.get(2)?,
        dat_revision: row.get(3)?,
        actor: row.get(4)?,
        attempt: row.get(5)?,
        requeued_from: row.get(6)?,
        audit: row.get(7)?,
        error: row.get(8)?,
        workers: Workers {
            readers: row.get::<_, i64>(9)? as usize,
            hashers: row.get::<_, i64>(10)? as usize,
        },
        created_at: row.get(11)?,
        started_at: row.get(12)?,
        finished_at: row.get(13)?,
        version: row.get::<_, i64>(14)? as u64,
    })
}

fn job_by_id(c: &Connection, id: i64) -> Result<Job> {
    c.query_row(&format!("{JOB_QUERY} WHERE j.id = ?1"), [id], job_from_row)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("no job {id}")))
}

/// Moves the Running Job `id` to `to`, failing if it is not Running.
fn finish_running(tx: &Transaction, id: i64, to: JobState, error: Option<&str>) -> Result<()> {
    let updated = tx.execute(
        "UPDATE jobs SET state = ?2, error = ?3, finished_at = ?4, version = version + 1
         WHERE id = ?1 AND state = 'running'",
        params![id, to.as_str(), error, now()],
    )?;
    if updated != 1 {
        return Err(Error::Io(format!("job {id} is no longer running")));
    }
    Ok(())
}

/// Marks the Running Job `id` Succeeded, having recorded `audit_id`.
pub(super) fn succeed(tx: &Transaction, id: i64, audit_id: i64) -> Result<()> {
    finish_running(tx, id, JobState::Succeeded, None)?;
    tx.execute(
        "UPDATE jobs SET audit_id = ?2 WHERE id = ?1",
        params![id, audit_id],
    )?;
    Ok(())
}

/// Marks the Running Job `id` Failed with `error`, and discards its running Audit, which releases its DAT Revision.
pub(super) fn fail(tx: &Transaction, id: i64, error: &str) -> Result<()> {
    finish_running(tx, id, JobState::Failed, Some(error))?;
    delete_audits(tx, "job_id = ?1 AND status = 'running'", [id])?;
    Ok(())
}

impl Store {
    /// Queues an Audit of the Source Set `name`, started by `actor`, pinned now to the DAT Revision `revision` (a
    /// SHA-256 prefix) or by default the latest Audit's, else the newest imported revision of its DAT. Refused, naming
    /// the Job, while the Source Set already has an Audit Job Queued or Running.
    pub fn enqueue_audit(
        &self,
        name: &str,
        revision: Option<&str>,
        actor: &str,
        workers: Workers,
    ) -> Result<Job> {
        if workers.readers == 0 || workers.hashers == 0 {
            return Err(Error::Refused(
                "a job needs at least one payload reader and one hash worker".into(),
            ));
        }
        self.write(|tx| {
            let set = set_by_name(tx, name)?;
            let active: Option<i64> = tx
                .query_row(
                    "SELECT id FROM jobs WHERE source_set_id = ?1 AND state IN ('queued', 'running')",
                    [set.id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(job) = active {
                return Err(Error::JobActive {
                    source_set: set.record.name,
                    job,
                });
            }
            let (revision_id, chosen) = choose_revision(tx, &set, revision)?;
            check_blob_present(&chosen)?;
            tx.execute(
                "INSERT INTO jobs (state, source_set_id, dat_revision_id, actor, readers, hashers, created_at)
                 VALUES ('queued', ?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    set.id,
                    revision_id,
                    actor,
                    workers.readers as i64,
                    workers.hashers as i64,
                    now()
                ],
            )?;
            job_by_id(tx, tx.last_insert_rowid())
        })
    }

    /// Moves the oldest Queued Job to Running and returns it, or returns `None` when none is Queued.
    pub fn claim_job(&self) -> Result<Option<Job>> {
        self.write(|tx| {
            let id: Option<i64> = tx
                .query_row(
                    "SELECT id FROM jobs WHERE state = 'queued' ORDER BY id LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(id) = id else { return Ok(None) };
            tx.execute(
                "UPDATE jobs SET state = 'running', started_at = ?2, version = version + 1 WHERE id = ?1",
                params![id, now()],
            )?;
            job_by_id(tx, id).map(Some)
        })
    }

    /// Runs the Running Job `id`'s Audit to completion, calling `progress` as files are read. The Audit and the
    /// Job's Succeeded state are recorded in one transaction; an Audit that fails is discarded and the Job Failed in
    /// one transaction. Returns the Job as it ends. An error means the Job's outcome could not be recorded, or that
    /// pruning or retention failed after the Job Succeeded.
    pub fn run_audit_job(
        &self,
        config: &Config,
        id: i64,
        progress: Option<&(dyn Fn(Progress) + Sync)>,
    ) -> Result<Job> {
        let row = self.read(|c| {
            c.query_row(
                "SELECT id, source_set_id, dat_revision_id, readers, hashers, state FROM jobs WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        JobRow {
                            id: r.get(0)?,
                            source_set_id: r.get(1)?,
                            dat_revision_id: r.get(2)?,
                            workers: Workers {
                                readers: r.get::<_, i64>(3)? as usize,
                                hashers: r.get::<_, i64>(4)? as usize,
                            },
                        },
                        r.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("no job {id}")))
        })?;
        let (row, state) = row;
        if state != JobState::Running.as_str() {
            return Err(Error::Refused(format!("job {id} is {state}, not running")));
        }
        let outcome = self
            .begin_job_audit(config, &row)
            .and_then(|running| self.complete_audit(config, running, progress));
        if let Err(err) = &outcome {
            // A failure before the Audit's row existed leaves the Job Running; one after has Failed it already.
            let message = err.to_string();
            let _ = self.write(|tx| match fail(tx, id, &message) {
                Err(Error::Io(_)) => Ok(()),
                other => other,
            });
        }
        let job = self.job(id)?;
        match outcome {
            Ok(_) => Ok(job),
            Err(err) if job.state == JobState::Succeeded => Err(err),
            Err(_) if job.state.is_final() => Ok(job),
            Err(err) => Err(err),
        }
    }

    /// Marks the Running Job `id` Failed with `error` and discards its running Audit, if any, in one transaction.
    /// Does nothing to a Job that is not Running.
    pub fn fail_job(&self, id: i64, error: &str) -> Result<Job> {
        self.write(|tx| match fail(tx, id, error) {
            Err(Error::Io(_)) | Ok(()) => job_by_id(tx, id),
            Err(err) => Err(err),
        })
    }

    /// Run when the Service starts: every Job still Running was cut off by a stop. Its Audit was already discarded
    /// when the store opened. A first attempt becomes Interrupted and a fresh Job is queued for it with `workers`; a
    /// Job that was itself queued again becomes Failed. Each Job's change is one transaction.
    pub fn recover_jobs(&self, workers: Workers) -> Result<Vec<Interruption>> {
        let running: Vec<(i64, u32)> = self.read(|c| {
            let mut stmt =
                c.prepare("SELECT id, attempt FROM jobs WHERE state = 'running' ORDER BY id")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })?;
        running
            .into_iter()
            .map(|(id, attempt)| {
                self.write(|tx| {
                    if attempt > 1 {
                        finish_running(tx, id, JobState::Failed, Some(INTERRUPTED_AGAIN))?;
                        return Ok(Interruption {
                            job: job_by_id(tx, id)?,
                            requeued: None,
                        });
                    }
                    finish_running(tx, id, JobState::Interrupted, Some(INTERRUPTED))?;
                    tx.execute(
                        "INSERT INTO jobs (state, source_set_id, dat_revision_id, actor, attempt, requeued_from,
                                           readers, hashers, created_at)
                         SELECT 'queued', source_set_id, dat_revision_id, actor, attempt + 1, id, ?2, ?3, ?4
                         FROM jobs WHERE id = ?1",
                        params![id, workers.readers as i64, workers.hashers as i64, now()],
                    )?;
                    let requeued = job_by_id(tx, tx.last_insert_rowid())?;
                    Ok(Interruption {
                        job: job_by_id(tx, id)?,
                        requeued: Some(requeued),
                    })
                })
            })
            .collect()
    }

    pub fn job(&self, id: i64) -> Result<Job> {
        self.read(|c| job_by_id(c, id))
    }

    /// Every Queued or Running Job and the `recent` latest others, newest first, read in one query.
    pub fn job_snapshot(&self, recent: usize) -> Result<Vec<Job>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "{JOB_QUERY} WHERE j.state IN ('queued', 'running')
                    OR j.id IN (SELECT id FROM jobs WHERE state NOT IN ('queued', 'running') ORDER BY id DESC LIMIT ?1)
                 ORDER BY j.id DESC"
            ))?;
            let rows = stmt.query_map([recent as i64], job_from_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// The latest `limit` Jobs, newest first.
    pub fn jobs(&self, limit: usize) -> Result<Vec<Job>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!("{JOB_QUERY} ORDER BY j.id DESC LIMIT ?1"))?;
            let rows = stmt.query_map([limit as i64], job_from_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }
}
