-- Removals and kept files.

-- A Build's items may now remove a built file no longer selected: kept aside in the folder's retention area, or,
-- when it is gone already, only its record dropped.
CREATE TABLE build_items_new (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    target TEXT NOT NULL,
    game TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('copy', 'repack', 'adopt', 'remove')),
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

-- The directories Title Select created in a Managed Folder that outlive a Build: the retention area
-- ('.title-select-retained'),
-- each generation's folder in it ('.title-select-retained/<generation>') and the private folder kept files are
-- deleted from ('.title-select-retained/.deleting'), by device and inode, committed before anything is moved into
-- them. Each is created under a temporary name recorded in folder_temps, then renamed into place without replacing
-- anything. One found with another identity, or not recorded at all, is never used or removed.
CREATE TABLE folder_dirs (
    folder_id INTEGER NOT NULL REFERENCES managed_folders (id),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    PRIMARY KEY (folder_id, name)
);

-- A plan to delete kept files: KeptDeletionContent JSON and the SHA-256 of that JSON, which an approval binds to.
-- An approved one not finished is being carried out; while it is, no Build or Check of its folder starts.
CREATE TABLE kept_deletions (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES managed_folders (id),
    content TEXT NOT NULL,
    digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    approval_digest TEXT,
    actor TEXT,
    approved_at TEXT,
    finished_at TEXT,
    -- KeptDeletionOutcome JSON, once carried out.
    outcomes TEXT,
    -- Why recovery left it unfinished when the service last started, if it did.
    recovery_error TEXT
);
CREATE INDEX kept_deletions_by_folder ON kept_deletions (folder_id, id);

-- A file a Build moved from its Managed Folder into '.title-select-retained/<generation>/<kept_name>', with what it
-- was: its name, its SHA-256, size and identity, and, for a file Title Select built, its game, ROMs and provenance.
-- Recorded 'keeping', the intent, before it is moved, and 'kept' once it is found moved; recovery settles a 'keeping'
-- one from what is in the folder. 'deleting' is the intent to delete it, recorded before it is moved into the
-- private '.deleting' folder and removed there; 'quarantined' when what was moved there is not the file, until it
-- is moved back. 'gone' is one found already gone, with its folder's marker and
-- retention area checked, when its deletion was carried out.
CREATE TABLE kept_files (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES managed_folders (id),
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    generation INTEGER NOT NULL,
    name TEXT NOT NULL,
    kept_name TEXT NOT NULL,
    reason TEXT NOT NULL CHECK (reason IN ('removal', 'replacement', 'conflict')),
    built_by INTEGER REFERENCES builds (id),
    game TEXT,
    roms TEXT,
    selection_id INTEGER REFERENCES selections (id),
    audit_id INTEGER REFERENCES audits (id),
    dat_revision_id INTEGER REFERENCES dat_revisions (id),
    sha256 TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('keeping', 'kept', 'lost', 'deleting', 'quarantined', 'deleted', 'gone')),
    error TEXT,
    created_at TEXT NOT NULL,
    kept_at TEXT,
    deletion_id INTEGER REFERENCES kept_deletions (id),
    deleted_at TEXT,
    -- The identity of what was found at '.title-select-retained/.deleting/<id>' when it was not the kept file, while
    -- the state is 'quarantined': it is to be moved back to the kept entry, and the deletion is not finished until it is.
    quarantine_dev INTEGER,
    quarantine_ino INTEGER
);
CREATE INDEX kept_files_by_folder ON kept_files (folder_id, generation, id);
CREATE UNIQUE INDEX kept_files_by_name ON kept_files (folder_id, generation, kept_name)
WHERE state IN ('keeping', 'kept', 'deleting', 'quarantined');
