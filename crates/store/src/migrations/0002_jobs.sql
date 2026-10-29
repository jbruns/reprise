-- A Job: a durable unit of work, so far always an Audit of a Source Set against the DAT Revision chosen when it was
-- queued. Each state change is one transaction. A Job found Running when the Service starts is Interrupted and
-- queued again once, as a new Job with attempt 2; an attempt 2 found Running is Failed instead.
CREATE TABLE jobs (
    id INTEGER PRIMARY KEY,
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'succeeded', 'failed', 'interrupted')),
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    dat_revision_id INTEGER NOT NULL REFERENCES dat_revisions (id),
    actor TEXT NOT NULL,
    attempt INTEGER NOT NULL DEFAULT 1,
    requeued_from INTEGER REFERENCES jobs (id),
    audit_id INTEGER REFERENCES audits (id),
    error TEXT,
    readers INTEGER NOT NULL,
    hashers INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT
);
-- One Audit per Source Set at a time.
CREATE UNIQUE INDEX jobs_one_active_per_source_set ON jobs (source_set_id) WHERE state IN ('queued', 'running');
CREATE INDEX jobs_by_state ON jobs (state, id);
-- The Job running an Audit, so that failing the Job discards its running Audit in the same transaction.
ALTER TABLE audits ADD COLUMN job_id INTEGER REFERENCES jobs (id);
