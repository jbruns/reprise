-- Partial builds, Incomplete folders, folder checks and conflict choices.

-- A Job may now be a read-only Check of a Managed Folder. SQLite cannot change a CHECK, so jobs is rebuilt again;
-- the migration runs with foreign keys off and checks them before it commits.
CREATE TABLE jobs_new (
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
    finished_at TEXT,
    version INTEGER NOT NULL DEFAULT 1,
    kind TEXT NOT NULL DEFAULT 'audit' CHECK (kind IN ('audit', 'selection', 'build', 'check')),
    profile_version_id INTEGER REFERENCES profile_versions (id),
    selection_id INTEGER REFERENCES selections (id),
    managed_folder_id INTEGER REFERENCES managed_folders (id),
    plan_id INTEGER REFERENCES plans (id)
);
INSERT INTO jobs_new (id, state, source_set_id, dat_revision_id, actor, attempt, requeued_from, audit_id, error,
                      readers, hashers, created_at, started_at, finished_at, version, kind, profile_version_id,
                      selection_id, managed_folder_id, plan_id)
SELECT id, state, source_set_id, dat_revision_id, actor, attempt, requeued_from, audit_id, error, readers, hashers,
       created_at, started_at, finished_at, version, kind, profile_version_id, selection_id, managed_folder_id, plan_id
FROM jobs;
DROP TABLE jobs;
ALTER TABLE jobs_new RENAME TO jobs;
CREATE INDEX jobs_by_state ON jobs (state, id);
-- One active Audit and one active Selection per Source Set, and one active Build or Check per Managed Folder: a
-- Check never runs while a Build changes the folder.
CREATE UNIQUE INDEX jobs_one_active_per_source_set ON jobs (source_set_id, kind)
WHERE state IN ('queued', 'running') AND kind IN ('audit', 'selection');
CREATE UNIQUE INDEX jobs_one_active_per_folder ON jobs (managed_folder_id)
WHERE state IN ('queued', 'running') AND kind IN ('build', 'check');
CREATE TRIGGER jobs_kind_profile BEFORE INSERT ON jobs
WHEN (NEW.kind = 'selection') <> (NEW.profile_version_id IS NOT NULL)
  OR (NEW.kind = 'build') <> (NEW.plan_id IS NOT NULL)
  OR (NEW.kind IN ('build', 'check')) <> (NEW.managed_folder_id IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT, 'a selection job runs a profile version, a build job an approved plan, a check job a managed folder, an audit job none');
END;

-- A Build's items may now adopt a file in conflict: recorded as built, nothing written.
CREATE TABLE build_items_new (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    target TEXT NOT NULL,
    game TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('copy', 'repack', 'adopt')),
    state TEXT NOT NULL CHECK (state IN ('pending', 'publishing', 'applied', 'failed', 'uncertain')),
    error TEXT,
    sha256 TEXT,
    size INTEGER,
    dev INTEGER,
    ino INTEGER,
    PRIMARY KEY (build_id, seq)
);
INSERT INTO build_items_new SELECT build_id, seq, target, game, kind, state, error, sha256, size, dev, ino
FROM build_items;
DROP TABLE build_items;
ALTER TABLE build_items_new RENAME TO build_items;

-- A built file now records the SHA-256 of the whole file, which a check compares a changed file against, and whether
-- it is TorrentZip: false once a check accepted the right ROMs in another ZIP. Files built before keep the SHA-256
-- their publish intent recorded.
CREATE TABLE built_files_new (
    folder_id INTEGER NOT NULL REFERENCES managed_folders (id),
    target TEXT NOT NULL,
    game TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('copy', 'repack', 'adopt')),
    build_id INTEGER NOT NULL REFERENCES builds (id),
    selection_id INTEGER NOT NULL REFERENCES selections (id),
    audit_id INTEGER NOT NULL REFERENCES audits (id),
    dat_revision_id INTEGER NOT NULL REFERENCES dat_revisions (id),
    roms TEXT NOT NULL,
    sources TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    sha256 TEXT,
    torrentzip INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (folder_id, target)
);
INSERT INTO built_files_new (folder_id, target, game, kind, build_id, selection_id, audit_id, dat_revision_id, roms,
                             sources, size, mtime_ns, sha256)
SELECT f.folder_id, f.target, f.game, f.kind, f.build_id, f.selection_id, f.audit_id, f.dat_revision_id, f.roms,
       f.sources, f.size, f.mtime_ns,
       (SELECT i.sha256 FROM build_items i
        WHERE i.build_id = f.build_id AND i.target = f.target AND i.state = 'applied')
FROM built_files f;
DROP TABLE built_files;
ALTER TABLE built_files_new RENAME TO built_files;

-- What a Build left its folder lacking of its Selection, recorded when it succeeds: each selected Game skipped as
-- unresolved, with no source, blocked or in conflict, and each whose file failed. Never written to the marker.
CREATE TABLE build_incomplete (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    reason TEXT NOT NULL CHECK (reason IN ('unresolved', 'no_source', 'failed', 'conflict', 'blocked')),
    grp TEXT NOT NULL,
    game TEXT NOT NULL,
    target TEXT NOT NULL,
    detail TEXT,
    PRIMARY KEY (build_id, seq)
);

-- A read-only check of a Managed Folder, by a Check Job or a plan, at the folder's generation then: its CheckSummary
-- and FolderCheck JSON. Older checks no plan refers to are dropped.
CREATE TABLE folder_checks (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES managed_folders (id),
    job_id INTEGER REFERENCES jobs (id),
    generation INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    summary TEXT NOT NULL,
    report TEXT NOT NULL
);
CREATE INDEX folder_checks_by_folder ON folder_checks (folder_id, id);

-- What a check read of an entry Title Select did not build, or of a built file in conflict, by its size and
-- modification time: so that an entry unchanged since is not read again. Probe JSON.
CREATE TABLE folder_probes (
    folder_id INTEGER NOT NULL REFERENCES managed_folders (id),
    name TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    ctime_ns INTEGER NOT NULL,
    probe TEXT NOT NULL,
    PRIMARY KEY (folder_id, name)
);

ALTER TABLE plans ADD COLUMN check_id INTEGER REFERENCES folder_checks (id);
