//! Reading payloads with bounded workers: `readers` threads open files and read (and decompress) their payloads,
//! handing each payload's bytes in chunks to one of `hashers` threads that checksums it.

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Mutex, PoisonError};

use crate::payload::{Checksums, Hasher};
use crate::{Progress, Workers};

const CHUNK: usize = 1 << 16;
/// Chunks a reader may get ahead of its hash worker.
const CHUNKS_IN_FLIGHT: usize = 4;

/// The Entries of one file, in archive order, and an error about the file as a whole. A member that fails to read
/// has its own error, and the rest of the archive is still read; an error about the archive ends it.
pub struct FileRead {
    pub members: Vec<(Option<String>, Result<Checksums, String>)>,
    pub error: Option<String>,
}

/// One payload for a hash worker: its chunks, and where to send its checksums.
struct HashJob {
    chunks: Receiver<Vec<u8>>,
    done: SyncSender<Checksums>,
}

/// Reads every file in `files` (a path, and whether it is a ZIP archive), returning one [`FileRead`] per file in the
/// same order.
pub fn read_files(
    files: &[(PathBuf, bool)],
    workers: Workers,
    progress: Option<&(dyn Fn(Progress) + Sync)>,
) -> Vec<FileRead> {
    let readers = workers.readers.clamp(1, files.len().max(1));
    let hashers = workers.hashers.max(1);
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<FileRead>>> = Mutex::new(files.iter().map(|_| None).collect());
    let (jobs, queue) = sync_channel::<HashJob>(0);
    let queue = Mutex::new(queue);
    std::thread::scope(|s| {
        for _ in 0..hashers {
            s.spawn(|| {
                loop {
                    let job = queue.lock().unwrap_or_else(PoisonError::into_inner).recv();
                    let Ok(job) = job else { break };
                    let mut hasher = Hasher::new();
                    for chunk in job.chunks {
                        hasher.update(&chunk);
                    }
                    let _ = job.done.send(hasher.finish());
                }
            });
        }
        for _ in 0..readers {
            let jobs = jobs.clone();
            let (next, done, results) = (&next, &done, &results);
            s.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((path, is_zip)) = files.get(i) else {
                        break;
                    };
                    let read = read_file(path, *is_zip, &jobs);
                    results.lock().unwrap_or_else(PoisonError::into_inner)[i] = Some(read);
                    let files_done = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if let Some(progress) = progress {
                        progress(Progress {
                            files_done,
                            files_total: files.len(),
                        });
                    }
                }
            });
        }
        // The hash workers stop once every reader has dropped its sender.
        drop(jobs);
    });
    results
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
        .into_iter()
        .map(|r| r.expect("every file is read"))
        .collect()
}

fn read_file(path: &PathBuf, is_zip: bool, jobs: &SyncSender<HashJob>) -> FileRead {
    let mut read = FileRead {
        members: Vec::new(),
        error: None,
    };
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) => {
            read.error = Some(e.to_string());
            return read;
        }
    };
    if !is_zip {
        match checksum(file, jobs) {
            Ok(sums) => read.members.push((None, Ok(sums))),
            Err(e) => read.error = Some(e),
        }
        return read;
    }
    let mut zip = match zip::ZipArchive::new(file) {
        Ok(zip) => zip,
        Err(e) => {
            read.error = Some(e.to_string());
            return read;
        }
    };
    for i in 0..zip.len() {
        let member = match zip.by_index(i) {
            Ok(member) => member,
            Err(e) => {
                read.error = Some(e.to_string());
                return read;
            }
        };
        if member.is_dir() {
            continue;
        }
        let name = match member.name() {
            Ok(name) => name.into_owned(),
            Err(e) => {
                read.error = Some(e.to_string());
                return read;
            }
        };
        read.members.push((Some(name), checksum(member, jobs)));
    }
    read
}

/// Reads `r` to its end, handing its bytes to a hash worker, and returns their checksums.
fn checksum(mut r: impl Read, jobs: &SyncSender<HashJob>) -> Result<Checksums, String> {
    let (chunks, chunks_rx) = sync_channel(CHUNKS_IN_FLIGHT);
    let (done_tx, done) = sync_channel(1);
    // Waits for a free hash worker.
    jobs.send(HashJob {
        chunks: chunks_rx,
        done: done_tx,
    })
    .map_err(|_| "no hash worker is running".to_owned())?;
    loop {
        let mut buf = vec![0u8; CHUNK];
        let n = r.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        buf.truncate(n);
        chunks
            .send(buf)
            .map_err(|_| "the hash worker stopped".to_owned())?;
    }
    drop(chunks);
    done.recv()
        .map_err(|_| "the hash worker stopped".to_owned())
}
