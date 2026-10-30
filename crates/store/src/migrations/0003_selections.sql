-- Selection Profiles, Selections, and Jobs that make a Selection (ADR 0001). The migration only adds: no existing
-- row changes meaning, and every existing Job is an Audit Job.

-- A named Selection Profile. Its contents live in its versions.
CREATE TABLE profiles (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL
);

-- One immutable version of a Profile: its canonical JSON (compact, maps sorted by key) and that JSON's SHA-256.
-- Versions are numbered 1, 2, … per Profile and never changed or deleted.
CREATE TABLE profile_versions (
    id INTEGER PRIMARY KEY,
    profile_id INTEGER NOT NULL REFERENCES profiles (id),
    version INTEGER NOT NULL,
    json TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    saved_at TEXT NOT NULL,
    saved_by TEXT NOT NULL,
    UNIQUE (profile_id, version)
);
CREATE TRIGGER profile_versions_immutable BEFORE UPDATE ON profile_versions
BEGIN
    SELECT RAISE(ABORT, 'profile versions are immutable');
END;
CREATE TRIGGER profile_versions_kept BEFORE DELETE ON profile_versions
BEGIN
    SELECT RAISE(ABORT, 'profile versions are never deleted');
END;

-- A Selection made for a Source Set from one DAT Revision with one Profile version. It is written whole, in the
-- transaction that finishes its Job. A pruned Selection keeps this row: its summary is what remains, and it no
-- longer holds its DAT Revision's blob.
CREATE TABLE selections (
    id INTEGER PRIMARY KEY,
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    dat_revision_id INTEGER NOT NULL REFERENCES dat_revisions (id),
    profile_version_id INTEGER NOT NULL REFERENCES profile_versions (id),
    job_id INTEGER REFERENCES jobs (id),
    system TEXT NOT NULL,
    system_settings INTEGER NOT NULL,
    -- SelectionPins, SelectionSummary, CoverageDrift and the stale choices of gone groups, as JSON.
    pins TEXT NOT NULL,
    summary TEXT NOT NULL,
    coverage_drift TEXT NOT NULL,
    stale_choices TEXT NOT NULL,
    created_at TEXT NOT NULL,
    pruned_at TEXT
);
CREATE INDEX selections_by_source_set ON selections (source_set_id, dat_revision_id, id);
CREATE INDEX selections_by_dat_revision ON selections (dat_revision_id);

-- A Job is an Audit or a Selection. Both are pinned to a Source Set and a DAT Revision; a Selection Job also runs
-- one Profile version and records the Selection it made.
ALTER TABLE jobs ADD COLUMN kind TEXT NOT NULL DEFAULT 'audit' CHECK (kind IN ('audit', 'selection'));
ALTER TABLE jobs ADD COLUMN profile_version_id INTEGER REFERENCES profile_versions (id);
ALTER TABLE jobs ADD COLUMN selection_id INTEGER REFERENCES selections (id);
CREATE TRIGGER jobs_kind_profile BEFORE INSERT ON jobs
WHEN (NEW.kind = 'selection') <> (NEW.profile_version_id IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT, 'a selection job runs a profile version, and an audit job none');
END;
-- One active Job of each kind per Source Set: an Audit and a Selection may be queued together.
DROP INDEX jobs_one_active_per_source_set;
CREATE UNIQUE INDEX jobs_one_active_per_source_set ON jobs (source_set_id, kind) WHERE state IN ('queued', 'running');

-- Per Game group: the flags and the Operator's choice state (GroupChoice as JSON), in group order.
CREATE TABLE selection_groups (
    selection_id INTEGER NOT NULL REFERENCES selections (id),
    seq INTEGER NOT NULL,
    name TEXT NOT NULL,
    unresolved INTEGER NOT NULL,
    drift INTEGER NOT NULL,
    choice TEXT,
    PRIMARY KEY (selection_id, seq)
);

-- Per release, in group order and then winners, alternatives and exclusions.
CREATE TABLE selection_releases (
    selection_id INTEGER NOT NULL REFERENCES selections (id),
    seq INTEGER NOT NULL,
    group_name TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('winner', 'alternative', 'exclusion')),
    title TEXT NOT NULL,
    clone_of TEXT,
    reason TEXT,
    stage TEXT NOT NULL,
    ref TEXT,
    message TEXT NOT NULL,
    unresolved INTEGER NOT NULL,
    from_choice INTEGER NOT NULL,
    PRIMARY KEY (selection_id, seq)
);
CREATE INDEX selection_releases_by_role ON selection_releases (selection_id, role);

-- Availability joins a Selection's releases to an Audit's Game Statuses by name.
CREATE INDEX audit_games_by_name ON audit_games (audit_id, name);
