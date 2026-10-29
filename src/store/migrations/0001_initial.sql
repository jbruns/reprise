-- A DAT, identified by its provider and header name. Each import of new bytes adds a DAT Revision of it.
CREATE TABLE dats (
    id INTEGER PRIMARY KEY,
    provider TEXT NOT NULL,
    name TEXT NOT NULL,
    UNIQUE (provider, name)
);

-- The blob of a DAT Revision is dats/<sha256> beside the database while blob_dropped_at is NULL.
CREATE TABLE dat_revisions (
    id INTEGER PRIMARY KEY,
    dat_id INTEGER NOT NULL REFERENCES dats (id),
    sha256 TEXT NOT NULL UNIQUE,
    version TEXT NOT NULL,
    file_name TEXT NOT NULL,
    imported_at TEXT NOT NULL,
    source_url TEXT,
    blob_dropped_at TEXT
);
CREATE INDEX dat_revisions_by_dat ON dat_revisions (dat_id, id);

-- A Source Set is a Root plus a path relative to it, displayed by its folder name and bound to a DAT.
CREATE TABLE source_sets (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    root TEXT NOT NULL,
    path TEXT NOT NULL,
    dat_id INTEGER NOT NULL REFERENCES dats (id),
    added_at TEXT NOT NULL,
    UNIQUE (root, path)
);

-- One row per Audit, inserted as running before its scan so that it holds its DAT Revision, and finished in the
-- transaction that writes its Entries and Game Statuses. A running row left by a crash is deleted on open. A
-- pruned Audit keeps this row: its counts are the summary that remains.
CREATE TABLE audits (
    id INTEGER PRIMARY KEY,
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    dat_revision_id INTEGER NOT NULL REFERENCES dat_revisions (id),
    status TEXT NOT NULL CHECK (status IN ('running', 'finished')),
    source_set_path TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    matched INTEGER NOT NULL DEFAULT 0,
    misnamed INTEGER NOT NULL DEFAULT 0,
    conflicting INTEGER NOT NULL DEFAULT 0,
    mismatched INTEGER NOT NULL DEFAULT 0,
    unmatched INTEGER NOT NULL DEFAULT 0,
    unreadable INTEGER NOT NULL DEFAULT 0,
    complete INTEGER NOT NULL DEFAULT 0,
    incomplete INTEGER NOT NULL DEFAULT 0,
    missing INTEGER NOT NULL DEFAULT 0,
    pruned_at TEXT
);
CREATE INDEX audits_by_source_set ON audits (source_set_id, id);
CREATE INDEX audits_by_dat_revision ON audits (dat_revision_id);

-- Something that cites an Audit as provenance, such as a later Change Plan. A cited Audit is never pruned.
CREATE TABLE audit_citations (
    audit_id INTEGER NOT NULL REFERENCES audits (id),
    cited_by TEXT NOT NULL,
    PRIMARY KEY (audit_id, cited_by)
);

-- roms is a JSON array of {game, name}; algorithms is a comma-separated list in report order.
CREATE TABLE audit_entries (
    audit_id INTEGER NOT NULL REFERENCES audits (id),
    seq INTEGER NOT NULL,
    path TEXT NOT NULL,
    member TEXT,
    finding TEXT NOT NULL,
    roms TEXT NOT NULL,
    named_game TEXT,
    named_rom TEXT,
    evidence_level TEXT,
    algorithms TEXT,
    reason TEXT,
    PRIMARY KEY (audit_id, seq)
);
CREATE INDEX audit_entries_by_finding ON audit_entries (audit_id, finding);

-- missing_roms is a JSON array of ROM names.
CREATE TABLE audit_games (
    audit_id INTEGER NOT NULL REFERENCES audits (id),
    seq INTEGER NOT NULL,
    name TEXT NOT NULL,
    status TEXT NOT NULL,
    missing_roms TEXT NOT NULL,
    PRIMARY KEY (audit_id, seq)
);
CREATE INDEX audit_games_by_status ON audit_games (audit_id, status);
