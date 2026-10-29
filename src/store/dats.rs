//! DAT import and DAT blob retention.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::blobs::{remove_blob, write_blob};
use super::{DATS, Error, Result, Store, io_error, now};
use crate::dat::Dat;

/// A DAT Revision as the store records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatRevisionRecord {
    /// SHA-256 of the unmodified DAT file; the DAT Revision's identity.
    pub sha256: String,
    pub provider: String,
    /// The DAT's header name.
    pub name: String,
    pub version: String,
    /// The original filename.
    pub file: String,
    pub imported_at: String,
    pub source_url: Option<String>,
    /// Whether the unmodified file is kept in the store.
    pub blob_present: bool,
}

impl DatRevisionRecord {
    /// The DAT this is a revision of, as `<provider>/<name>`.
    pub fn dat(&self) -> String {
        format!("{}/{}", self.provider, self.name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportOutcome {
    /// A new DAT Revision.
    Added,
    /// The same bytes were already imported; nothing changed.
    AlreadyImported,
    /// The same bytes were already imported and their dropped blob is restored.
    Restored,
}

pub(super) const REVISION_COLUMNS: &str = "r.id, r.sha256, d.provider, d.name, r.version, r.file_name, \
     r.imported_at, r.source_url, r.blob_dropped_at IS NULL";

pub(super) fn revision_from_row(row: &Row) -> rusqlite::Result<(i64, DatRevisionRecord)> {
    Ok((
        row.get(0)?,
        DatRevisionRecord {
            sha256: row.get(1)?,
            provider: row.get(2)?,
            name: row.get(3)?,
            version: row.get(4)?,
            file: row.get(5)?,
            imported_at: row.get(6)?,
            source_url: row.get(7)?,
            blob_present: row.get(8)?,
        },
    ))
}

fn revision_by_sha(c: &Connection, sha256: &str) -> Result<Option<(i64, DatRevisionRecord)>> {
    let sql = format!(
        "SELECT {REVISION_COLUMNS} FROM dat_revisions r JOIN dats d ON d.id = r.dat_id WHERE r.sha256 = ?1"
    );
    Ok(c.query_row(&sql, [sha256], revision_from_row).optional()?)
}

/// Splits `<provider>/<name>`. A provider never contains `/`; a DAT's name may.
pub(super) fn split_dat(dat: &str) -> Result<(&str, &str)> {
    dat.split_once('/')
        .filter(|(p, n)| !p.is_empty() && !n.is_empty())
        .ok_or_else(|| Error::Refused(format!("{dat}: a DAT is named as <provider>/<name>")))
}

impl Store {
    /// Imports the DAT file at `path` as a DAT Revision of the DAT named by `provider` and its header name.
    /// Importing bytes already imported returns the existing DAT Revision, restoring its blob if it was dropped.
    pub fn import_dat(
        &self,
        path: &Path,
        provider: &str,
        source_url: Option<&str>,
    ) -> Result<(DatRevisionRecord, ImportOutcome)> {
        if provider.is_empty() || provider.contains('/') {
            return Err(Error::Refused(format!(
                "{provider:?}: a provider is a non-empty name without `/`"
            )));
        }
        let bytes = std::fs::read(path).map_err(|e| io_error(path, e))?;
        let dat = Dat::parse(&bytes, path)?;
        if dat.name.is_empty() {
            return Err(Error::Refused(format!(
                "{}: the DAT header has no name",
                path.display()
            )));
        }
        let dats = self.dir.join(DATS);
        if let Some((id, existing)) = self.read(|c| revision_by_sha(c, &dat.sha256))? {
            let blob = self.blob_path(&existing.sha256);
            if existing.blob_present && blob.is_file() {
                return Ok((existing, ImportOutcome::AlreadyImported));
            }
            write_blob(&dats, &dat.sha256, &bytes)?;
            self.write(|tx| {
                tx.execute(
                    "UPDATE dat_revisions SET blob_dropped_at = NULL WHERE id = ?1",
                    [id],
                )?;
                Ok(())
            })?;
            let restored = DatRevisionRecord {
                blob_present: true,
                ..existing
            };
            return Ok((restored, ImportOutcome::Restored));
        }

        write_blob(&dats, &dat.sha256, &bytes)?;
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let record = self.write(|tx| {
            tx.execute(
                "INSERT INTO dats (provider, name) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
                params![provider, dat.name],
            )?;
            let dat_id: i64 = tx.query_row(
                "SELECT id FROM dats WHERE provider = ?1 AND name = ?2",
                params![provider, dat.name],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT INTO dat_revisions (dat_id, sha256, version, file_name, imported_at, source_url)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![dat_id, dat.sha256, dat.version, file, now(), source_url],
            )?;
            Ok(revision_by_sha(tx, &dat.sha256)?.expect("just inserted").1)
        })?;
        self.retain_blobs()?;
        Ok((record, ImportOutcome::Added))
    }

    /// Every DAT Revision, oldest import first.
    pub fn dat_revisions(&self) -> Result<Vec<DatRevisionRecord>> {
        self.read(|c| {
            let sql = format!(
                "SELECT {REVISION_COLUMNS} FROM dat_revisions r JOIN dats d ON d.id = r.dat_id ORDER BY r.id"
            );
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map([], |r| revision_from_row(r).map(|(_, rev)| rev))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Drops the blob of every DAT Revision that no retained Audit refers to and that is not the newest
    /// imported revision of its DAT (ADR 0003). The row stays. Returns the SHA-256s whose blobs were dropped.
    pub fn retain_blobs(&self) -> Result<Vec<String>> {
        let dropped: Vec<String> = self.write(|tx| {
            let mut stmt = tx.prepare(
                "SELECT r.sha256 FROM dat_revisions r
                 WHERE r.blob_dropped_at IS NULL
                   AND r.id <> (SELECT max(n.id) FROM dat_revisions n WHERE n.dat_id = r.dat_id)
                   AND NOT EXISTS (
                       SELECT 1 FROM audits a WHERE a.dat_revision_id = r.id AND a.pruned_at IS NULL)
                 ORDER BY r.id",
            )?;
            let shas = stmt
                .query_map([], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            let at = now();
            for sha in &shas {
                tx.execute(
                    "UPDATE dat_revisions SET blob_dropped_at = ?1 WHERE sha256 = ?2",
                    params![at, sha],
                )?;
            }
            Ok(shas)
        })?;
        // The rows no longer hold these blobs, so a crash before this point leaves only garbage.
        for sha in &dropped {
            remove_blob(&self.blob_path(sha))?;
        }
        Ok(dropped)
    }
}
