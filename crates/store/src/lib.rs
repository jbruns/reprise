//! The store (ADR 0003): one SQLite database in a store directory, with DAT blobs under `dats/<sha256>`.

mod audits;
mod blobs;
pub mod config;
mod dats;
mod jobs;
mod migrations;
mod sets;

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};

pub use audits::StoredAudit;
pub use jobs::Interruption;
pub use title_select_api::{
    AuditSummary, DatRevisionRecord, ImportOutcome, Job, JobState, LastVerified, SourceSetRecord,
    Workers,
};

/// The database's file name in the store directory.
pub const DATABASE: &str = "title-select.sqlite";
/// The directory, in the store directory, that holds DAT blobs.
pub const DATS: &str = "dats";
/// The directory, in the store directory, that holds the backups taken before migrations.
pub const BACKUPS: &str = "backups";
/// The directory, in the store directory, that holds per-run scratch folders, such as a Selection's Retool run.
/// Each run deletes its own folder.
pub const TMP: &str = "tmp";
/// The file, in the store directory, that an open store holds an exclusive lock on.
pub const LOCK: &str = "lock";

const BUSY_TIMEOUT: Duration = Duration::from_secs(30);
/// Idle reader connections kept for reuse.
const READER_POOL: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("store: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("store: {0}")]
    Io(String),
    /// The request cannot be carried out as asked.
    #[error("{0}")]
    Refused(String),
    /// What the request names does not exist.
    #[error("{0}")]
    NotFound(String),
    /// A Source Set already has an Audit Job Queued or Running.
    #[error("Source Set {source_set} already has an active Audit: Job {job}")]
    JobActive { source_set: String, job: i64 },
    #[error(transparent)]
    Audit(#[from] title_select_engine::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) fn io_error(path: &Path, err: std::io::Error) -> Error {
    Error::Io(format!("{}: {err}", path.display()))
}

pub(crate) fn now() -> String {
    jiff::Timestamp::now().to_string()
}

/// An open store: one writer connection that serialises writes, and a pool of readers. Only one process at a
/// time may open a store, so the writer is the only one.
pub struct Store {
    dir: PathBuf,
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
    /// Holds the exclusive lock on `lock` until the store is dropped.
    _lock: File,
}

/// Takes the store directory's exclusive lock, failing at once if another process holds it.
fn lock(dir: &Path) -> Result<File> {
    let path = dir.join(LOCK);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| io_error(&path, e))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(Error::Refused(format!(
            "{}: the store is in use by another Title Select process",
            dir.display()
        ))),
        Err(TryLockError::Error(e)) => Err(io_error(&path, e)),
    }
}

/// Settings every connection gets (ADR 0003).
fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

impl Store {
    /// Opens the store in `dir`, creating it if needed, migrating it, and collecting blobs that have no row.
    pub fn open(dir: &Path) -> Result<Store> {
        Store::open_with(dir, migrations::MIGRATIONS)
    }

    fn open_with(dir: &Path, migrations: &[&str]) -> Result<Store> {
        let dats = dir.join(DATS);
        std::fs::create_dir_all(&dats).map_err(|e| io_error(&dats, e))?;
        let lock = lock(dir)?;
        let mut writer = Connection::open(dir.join(DATABASE))?;
        configure(&writer)?;
        let mode: String = writer.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(Error::Io(format!(
                "{}: cannot use WAL mode (got {mode})",
                dir.display()
            )));
        }
        migrations::migrate(&mut writer, dir, migrations)?;
        let store = Store {
            dir: dir.to_path_buf(),
            writer: Mutex::new(writer),
            readers: Mutex::new(Vec::new()),
            _lock: lock,
        };
        // Retention is not run here: it runs after an import or an Audit, so a blob restored by one command is
        // still there for the Audit the next command runs.
        store.discard_unfinished_audits()?;
        store.collect_garbage()?;
        Ok(store)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the blob of the DAT Revision with this SHA-256 is kept.
    pub fn blob_path(&self, sha256: &str) -> PathBuf {
        self.dir.join(DATS).join(sha256)
    }

    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let pooled = self.readers.lock().unwrap().pop();
        let conn = match pooled {
            Some(conn) => conn,
            None => {
                let conn = Connection::open_with_flags(
                    self.dir.join(DATABASE),
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )?;
                configure(&conn)?;
                conn
            }
        };
        let out = f(&conn);
        let mut readers = self.readers.lock().unwrap();
        if readers.len() < READER_POOL {
            readers.push(conn);
        }
        out
    }

    /// Runs `f` in one write transaction on the writer connection.
    fn write<T>(&self, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let out = f(&tx)?;
            tx.commit()?;
            Ok(out)
        })
    }

    /// Runs `f` while holding the writer, so that blob files and the rows that hold them change together.
    fn with_writer<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut conn = self.writer.lock().unwrap();
        f(&mut conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pragma(conn: &Connection, name: &str) -> String {
        conn.query_row(&format!("PRAGMA {name}"), [], |r| {
            r.get::<_, rusqlite::types::Value>(0)
        })
        .map(|v| format!("{v:?}"))
        .unwrap()
    }

    #[test]
    fn every_connection_uses_wal_full_sync_foreign_keys_and_a_busy_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        let writer = store.writer.lock().unwrap();
        assert_eq!(pragma(&writer, "journal_mode"), r#"Text("wal")"#);
        let reader = store.read(|c| Ok(pragma(c, "journal_mode"))).unwrap();
        assert_eq!(reader, r#"Text("wal")"#);
        let settings = |c: &Connection| {
            (
                pragma(c, "synchronous"),
                pragma(c, "foreign_keys"),
                pragma(c, "busy_timeout"),
            )
        };
        let expected = (
            "Integer(2)".to_string(),
            "Integer(1)".to_string(),
            "Integer(30000)".to_string(),
        );
        assert_eq!(settings(&writer), expected);
        assert_eq!(store.read(|c| Ok(settings(c))).unwrap(), expected);
    }

    #[test]
    fn a_migration_runs_after_backing_up_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let current = migrations::MIGRATIONS;
        let store = Store::open_with(dir.path(), current).unwrap();
        let blob = store.dir().join("dat.dat");
        std::fs::write(&blob, DAT).unwrap();
        store.import_dat(&blob, "Test", None).unwrap();
        drop(store);
        assert!(!dir.path().join(BACKUPS).exists());

        let second = "CREATE TABLE later (x INTEGER);";
        let store = Store::open_with(dir.path(), &[current, &[second]].concat()).unwrap();

        let backups: Vec<_> = std::fs::read_dir(dir.path().join(BACKUPS))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(backups.len(), 1);
        let backup = Connection::open(&backups[0]).unwrap();
        let before = format!("Integer({})", current.len());
        assert_eq!(pragma(&backup, "user_version"), before);
        let revisions: i64 = backup
            .query_row("SELECT count(*) FROM dat_revisions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(revisions, 1);
        let version = store.read(|c| Ok(pragma(c, "user_version"))).unwrap();
        assert_eq!(version, format!("Integer({})", current.len() + 1));
    }

    #[test]
    fn a_second_open_of_the_same_store_fails_until_the_first_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let first = Store::open(dir.path()).unwrap();

        let second = Store::open(dir.path());

        let Err(Error::Refused(message)) = second else {
            panic!(
                "expected a refusal, got {second:?}",
                second = second.map(|_| ())
            );
        };
        assert!(message.contains("in use by another Title Select process"));
        drop(first);
        Store::open(dir.path()).unwrap();
    }

    const DAT: &str = "<datafile><header><name>Test</name><version>1</version></header></datafile>";

    #[test]
    fn a_store_from_a_newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let first = migrations::MIGRATIONS[0];
        drop(Store::open_with(dir.path(), &[first, "SELECT 1;"]).unwrap());

        assert!(matches!(
            Store::open_with(dir.path(), &[first]),
            Err(Error::Refused(_))
        ));
    }
}
