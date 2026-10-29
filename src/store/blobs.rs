//! Content-addressed DAT blobs under `dats/<sha256>` (ADR 0003).

use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use super::{DATS, Result, Store, io_error};

/// Writes a blob to a temp file in the blob directory, fsyncs it, renames it into place and fsyncs the
/// directory, so it is durable before any row refers to it.
pub(super) fn write_blob(dats: &Path, sha256: &str, bytes: &[u8]) -> Result<()> {
    let tmp = dats.join(format!(".{sha256}.tmp"));
    let mut file = File::create(&tmp).map_err(|e| io_error(&tmp, e))?;
    file.write_all(bytes).map_err(|e| io_error(&tmp, e))?;
    file.sync_all().map_err(|e| io_error(&tmp, e))?;
    drop(file);
    let path = dats.join(sha256);
    std::fs::rename(&tmp, &path).map_err(|e| io_error(&path, e))?;
    sync_dir(dats)
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| io_error(dir, e))
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

pub(super) fn remove_blob(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io_error(path, e)),
        _ => Ok(()),
    }
}

impl Store {
    /// Deletes every file in the blob directory that is not the blob of a DAT Revision whose row holds it,
    /// including temp files left by an interrupted write. Returns the names it deleted.
    pub fn collect_garbage(&self) -> Result<Vec<String>> {
        let held: HashSet<String> = self.read(|c| {
            let mut stmt =
                c.prepare("SELECT sha256 FROM dat_revisions WHERE blob_dropped_at IS NULL")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })?;
        let dats = self.dir.join(DATS);
        let mut removed = Vec::new();
        for item in std::fs::read_dir(&dats).map_err(|e| io_error(&dats, e))? {
            let item = item.map_err(|e| io_error(&dats, e))?;
            let name = item.file_name().to_string_lossy().into_owned();
            let is_file = item.file_type().is_ok_and(|t| t.is_file());
            if is_file && !held.contains(&name) {
                remove_blob(&item.path())?;
                removed.push(name);
            }
        }
        removed.sort();
        Ok(removed)
    }
}
