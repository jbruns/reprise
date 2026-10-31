-- Cleanups: a plan that acts on the drift a check found in a Managed Folder, carried out as a Build.

-- A Build's items may now keep aside an entry Title Select did not build, acknowledge one, or put a kept file back.
-- An item that moves an entry within the folder, rather than publishing a staged file, records where it moves it
-- from ('origin', relative to the folder) with the entry's identity as its intent, before it is moved; a restore
-- also names the kept file it moves ('kept_id').
CREATE TABLE build_items_new (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    target TEXT NOT NULL,
    game TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('copy', 'repack', 'adopt', 'remove', 'keep_aside', 'acknowledge', 'restore')),
    state TEXT NOT NULL CHECK (state IN ('pending', 'publishing', 'applied', 'failed', 'uncertain')),
    error TEXT,
    sha256 TEXT,
    size INTEGER,
    dev INTEGER,
    ino INTEGER,
    origin TEXT,
    kept_id INTEGER REFERENCES kept_files (id),
    PRIMARY KEY (build_id, seq)
);
INSERT INTO build_items_new (build_id, seq, target, game, kind, state, error, sha256, size, dev, ino)
SELECT build_id, seq, target, game, kind, state, error, sha256, size, dev, ino FROM build_items;
DROP TABLE build_items;
ALTER TABLE build_items_new RENAME TO build_items;

-- A kept file may now be a directory, moved whole and never read, with no SHA-256; an entry Title Select did not
-- build ('drift'), or a built file a cleanup repacked ('repack'). 'restoring' is the intent to move it back into the
-- folder, recorded with its Build's item before it is moved; 'restored' once it is found there. 'probe' is what
-- reading it found, as a JSON Probe, reused while its identity, size, modification and status change times are
-- 'probe_ctime_ns' and those recorded.
CREATE TABLE kept_files_new (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES managed_folders (id),
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    generation INTEGER NOT NULL,
    name TEXT NOT NULL,
    kept_name TEXT NOT NULL,
    reason TEXT NOT NULL CHECK (reason IN ('removal', 'replacement', 'conflict', 'drift', 'repack')),
    built_by INTEGER REFERENCES builds (id),
    game TEXT,
    roms TEXT,
    selection_id INTEGER REFERENCES selections (id),
    audit_id INTEGER REFERENCES audits (id),
    dat_revision_id INTEGER REFERENCES dat_revisions (id),
    sha256 TEXT,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('keeping', 'kept', 'lost', 'deleting', 'quarantined', 'deleted', 'gone',
                                         'restoring', 'restored')),
    error TEXT,
    created_at TEXT NOT NULL,
    kept_at TEXT,
    deletion_id INTEGER REFERENCES kept_deletions (id),
    deleted_at TEXT,
    quarantine_dev INTEGER,
    quarantine_ino INTEGER,
    directory INTEGER NOT NULL DEFAULT 0,
    probe TEXT,
    probe_ctime_ns INTEGER,
    restored_at TEXT,
    CHECK (directory = 1 OR sha256 IS NOT NULL)
);
INSERT INTO kept_files_new (id, folder_id, build_id, seq, generation, name, kept_name, reason, built_by, game, roms,
                            selection_id, audit_id, dat_revision_id, sha256, size, mtime_ns, dev, ino, state, error,
                            created_at, kept_at, deletion_id, deleted_at, quarantine_dev, quarantine_ino)
SELECT id, folder_id, build_id, seq, generation, name, kept_name, reason, built_by, game, roms, selection_id, audit_id,
       dat_revision_id, sha256, size, mtime_ns, dev, ino, state, error, created_at, kept_at, deletion_id, deleted_at,
       quarantine_dev, quarantine_ino
FROM kept_files;
DROP TABLE kept_files;
ALTER TABLE kept_files_new RENAME TO kept_files;
CREATE INDEX kept_files_by_folder ON kept_files (folder_id, generation, id);
CREATE UNIQUE INDEX kept_files_by_name ON kept_files (folder_id, generation, kept_name)
WHERE state IN ('keeping', 'kept', 'deleting', 'quarantined', 'restoring');

-- An entry in a Managed Folder Title Select did not build that you acknowledged: a check reports it as acknowledged
-- rather than as drift while it is the same entry, by name, identity, size and modification time, and drops the
-- acknowledgement once it is not. Acknowledging writes nothing to the folder.
CREATE TABLE drift_acks (
    folder_id INTEGER NOT NULL REFERENCES managed_folders (id),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    build_id INTEGER NOT NULL REFERENCES builds (id),
    acknowledged_at TEXT NOT NULL,
    PRIMARY KEY (folder_id, name)
);
