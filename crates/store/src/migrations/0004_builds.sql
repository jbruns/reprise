-- Managed Folders, Change Plans and Builds.

-- The store's identity, which every Managed Folder's marker records, so that a plan can tell a folder this store
-- manages from one another store manages.
CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT INTO meta (key, value) VALUES ('store_id', lower(hex(randomblob(16))));

-- A Managed Folder: a folder in a writable Root that a Source Set's Selections with one Profile are built into. Its
-- system is the Source Set's DAT; a folder may be rebound only to a Source Set of the same DAT. generation counts
-- the Builds that succeeded and is mirrored in the folder's marker.
CREATE TABLE managed_folders (
    id INTEGER PRIMARY KEY,
    root TEXT NOT NULL,
    path TEXT NOT NULL,
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    profile_id INTEGER NOT NULL REFERENCES profiles (id),
    generation INTEGER NOT NULL DEFAULT 0,
    current_build_id INTEGER REFERENCES builds (id),
    created_at TEXT NOT NULL,
    UNIQUE (root, path)
);

-- A Change Plan: what building one Selection into a Managed Folder would do, as PlanContent JSON, and the SHA-256
-- of that JSON, which an approval binds to. changes is the PlanChange JSON against previous_selection_id.
CREATE TABLE plans (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES managed_folders (id),
    selection_id INTEGER NOT NULL REFERENCES selections (id),
    audit_id INTEGER REFERENCES audits (id),
    generation INTEGER NOT NULL,
    content TEXT NOT NULL,
    digest TEXT NOT NULL,
    previous_selection_id INTEGER REFERENCES selections (id),
    changes TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX plans_by_folder ON plans (folder_id, id);

CREATE TABLE plan_approvals (
    plan_id INTEGER PRIMARY KEY REFERENCES plans (id),
    digest TEXT NOT NULL,
    actor TEXT NOT NULL,
    approved_at TEXT NOT NULL
);

-- A Build: an approved plan carried out by a Build Job, from the generation in its plan to generation + 1.
CREATE TABLE builds (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES managed_folders (id),
    plan_id INTEGER NOT NULL REFERENCES plans (id),
    job_id INTEGER NOT NULL UNIQUE REFERENCES jobs (id),
    selection_id INTEGER NOT NULL REFERENCES selections (id),
    audit_id INTEGER NOT NULL REFERENCES audits (id),
    dat_revision_id INTEGER NOT NULL REFERENCES dat_revisions (id),
    state TEXT NOT NULL CHECK (state IN ('running', 'succeeded', 'failed', 'interrupted')),
    generation INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    -- Set in the transaction that advances the folder to `generation` and makes this its current Build, which
    -- commits before the marker is advanced: the only evidence on which recovery moves a marker one generation
    -- behind the store forward. A marker is never moved back.
    finalizing_at TEXT,
    -- When the folder was last left clean after the Build: no staging directory, no half-written marker, every
    -- publishing item settled. A Build that ended without it still needs recovery, and its folder is not planned.
    cleaned_at TEXT,
    -- Why recovery left the folder as it was.
    recovery_error TEXT
);
CREATE INDEX builds_by_folder ON builds (folder_id, id);

-- Each file a Build writes, recorded pending before the first is written, so that an interrupted Build shows what
-- it never reached. An item is 'uncertain' when recovery found something other than its file at its target.
CREATE TABLE build_items (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    target TEXT NOT NULL,
    game TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('copy', 'repack')),
    state TEXT NOT NULL CHECK (state IN ('pending', 'publishing', 'applied', 'failed', 'uncertain')),
    error TEXT,
    -- The staged file's SHA-256, size, device and inode, recorded with state 'publishing' before it is renamed into
    -- place, so that the Build and recovery can tell the file it put there from any other.
    sha256 TEXT,
    size INTEGER,
    dev INTEGER,
    ino INTEGER,
    PRIMARY KEY (build_id, seq)
);

-- The files in a Managed Folder that a Build wrote, with their provenance: the ROMs as JSON ExpectedRoms, the
-- sources as JSON PlanSources. Written in the transaction that marks the item applied.
CREATE TABLE built_files (
    folder_id INTEGER NOT NULL REFERENCES managed_folders (id),
    target TEXT NOT NULL,
    game TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('copy', 'repack')),
    build_id INTEGER NOT NULL REFERENCES builds (id),
    selection_id INTEGER NOT NULL REFERENCES selections (id),
    audit_id INTEGER NOT NULL REFERENCES audits (id),
    dat_revision_id INTEGER NOT NULL REFERENCES dat_revisions (id),
    roms TEXT NOT NULL,
    sources TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    PRIMARY KEY (folder_id, target)
);

-- What a Build or recovery created in a Managed Folder for itself: the staging directory, each file in it (named
-- '.title-select-staging/<file>') and a marker's temp file, by device and inode, committed before it is relied on.
-- Only an entry found with a recorded identity is ever removed; anything else there is left and blocks the folder.
CREATE TABLE folder_temps (
    folder_id INTEGER NOT NULL REFERENCES managed_folders (id),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    PRIMARY KEY (folder_id, name)
);

-- Something that cites a Selection as provenance, such as a Managed Folder's current Build ('managed-folder:<id>'),
-- or holds it for a Build Job from its approval until it ends ('build-job:<id>'); audit_citations does the same for
-- Audits. A cited Selection is never pruned.
CREATE TABLE selection_citations (
    selection_id INTEGER NOT NULL REFERENCES selections (id),
    cited_by TEXT NOT NULL,
    PRIMARY KEY (selection_id, cited_by)
);

-- A Job may now be a Build of an approved plan into a Managed Folder. SQLite cannot change a CHECK, so jobs is
-- rebuilt; the migration runs with foreign keys off and checks them before it commits.
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
    kind TEXT NOT NULL DEFAULT 'audit' CHECK (kind IN ('audit', 'selection', 'build')),
    profile_version_id INTEGER REFERENCES profile_versions (id),
    selection_id INTEGER REFERENCES selections (id),
    managed_folder_id INTEGER REFERENCES managed_folders (id),
    plan_id INTEGER REFERENCES plans (id)
);
INSERT INTO jobs_new (id, state, source_set_id, dat_revision_id, actor, attempt, requeued_from, audit_id, error,
                      readers, hashers, created_at, started_at, finished_at, version, kind, profile_version_id,
                      selection_id)
SELECT id, state, source_set_id, dat_revision_id, actor, attempt, requeued_from, audit_id, error, readers, hashers,
       created_at, started_at, finished_at, version, kind, profile_version_id, selection_id
FROM jobs;
DROP TABLE jobs;
ALTER TABLE jobs_new RENAME TO jobs;
CREATE INDEX jobs_by_state ON jobs (state, id);
-- One active Audit and one active Selection per Source Set, and one active Build per Managed Folder.
CREATE UNIQUE INDEX jobs_one_active_per_source_set ON jobs (source_set_id, kind)
WHERE state IN ('queued', 'running') AND kind <> 'build';
CREATE UNIQUE INDEX jobs_one_active_build_per_folder ON jobs (managed_folder_id)
WHERE state IN ('queued', 'running') AND kind = 'build';
CREATE TRIGGER jobs_kind_profile BEFORE INSERT ON jobs
WHEN (NEW.kind = 'selection') <> (NEW.profile_version_id IS NOT NULL)
  OR (NEW.kind = 'build') <> (NEW.managed_folder_id IS NOT NULL AND NEW.plan_id IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT, 'a selection job runs a profile version, a build job an approved plan, an audit job neither');
END;
