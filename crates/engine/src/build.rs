//! Writing a Build's files: copying a source archive as-is or repacking verified payloads as TorrentZip, and checking
//! what was written before it is renamed into place.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read};
use std::path::Path;

use title_select_api::build::ExpectedRom;

use crate::payload::{Checksums, Hasher};
use crate::torrentzip::{self, Writer, compare_names};

const CHUNK: usize = 1 << 16;

/// Where one ROM's payload is read from.
#[derive(Debug, Clone, Copy)]
pub struct Source<'a> {
    pub path: &'a Path,
    /// The member of the ZIP archive at `path`, or `None` for a plain file.
    pub member: Option<&'a str>,
}

/// Whether `checksums` are the payload `rom` expects: its size and every checksum it lists.
pub fn matches(checksums: &Checksums, rom: &ExpectedRom) -> bool {
    let same = |want: &Option<String>, got: &str| {
        want.as_deref()
            .is_none_or(|want| want.eq_ignore_ascii_case(got))
    };
    checksums.size == rom.size
        && same(&rom.crc, &checksums.crc)
        && same(&rom.md5, &checksums.md5)
        && same(&rom.sha1, &checksums.sha1)
        && same(&rom.sha256, &checksums.sha256)
}

/// Whether the archive at `path` is structurally valid TorrentZip, read from its central directory alone.
pub fn is_torrentzip(path: &Path) -> bool {
    File::open(path)
        .map_err(torrentzip::Error::from)
        .and_then(|f| torrentzip::central(&mut BufReader::new(f)))
        .is_ok()
}

/// Copies `source` to the new file `dest`. On Linux the standard library uses `copy_file_range`, so that the
/// filesystem may share or offload the copy; nowhere is a hardlink made.
pub fn copy(source: &Path, dest: &Path) -> io::Result<u64> {
    let mut from = File::open(source)?;
    let mut to = File::options().write(true).create_new(true).open(dest)?;
    let copied = io::copy(&mut from, &mut to)?;
    to.sync_all()?;
    Ok(copied)
}

/// Writes the TorrentZip of `roms` to the new file `dest`, each ROM's payload read from its source. A payload of
/// the wrong size is refused; checksums are checked by [`verify`] afterwards.
pub fn repack(dest: &Path, roms: &[(&ExpectedRom, Source)]) -> Result<(), String> {
    let mut order: Vec<_> = roms.iter().collect();
    order.sort_by(|a, b| compare_names(&a.0.name, &b.0.name));
    let file = File::options()
        .write(true)
        .create_new(true)
        .open(dest)
        .map_err(|e| format!("{}: {e}", dest.display()))?;
    let mut writer = Writer::new(BufWriter::new(file)).map_err(|e| e.to_string())?;
    for (rom, source) in order {
        let opened =
            File::open(source.path).map_err(|e| format!("{}: {e}", source.path.display()))?;
        let mut reader = BufReader::new(opened);
        let added = match source.member {
            None => writer.add(&rom.name, rom.size, &mut reader),
            Some(member) => {
                let mut archive = zip::ZipArchive::new(reader)
                    .map_err(|e| format!("{}: {e}", source.path.display()))?;
                let mut payload = archive
                    .by_name(member)
                    .map_err(|e| format!("{} ({member}): {e}", source.path.display()))?;
                writer.add(&rom.name, rom.size, &mut payload)
            }
        };
        added.map_err(|e| format!("{} ({}): {e}", source.path.display(), rom.name))?;
    }
    let out = writer.finish().map_err(|e| e.to_string())?;
    let file = out.into_inner().map_err(|e| e.error().to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}

/// Checks that the file at `path` is valid TorrentZip whose members are exactly `roms`, under their names, and that
/// each member's payload, decompressed and hashed again, is the one its ROM expects.
pub fn verify(path: &Path, roms: &[ExpectedRom]) -> Result<(), String> {
    let mut file = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    let members = torrentzip::validate(&mut file).map_err(|e| e.to_string())?;
    let mut names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
    let mut wanted: Vec<&str> = roms.iter().map(|r| r.name.as_str()).collect();
    names.sort_unstable();
    wanted.sort_unstable();
    if names != wanted {
        return Err(format!(
            "it holds {} where the game's ROMs are {}",
            names.join(", "),
            wanted.join(", ")
        ));
    }
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    for rom in roms {
        let mut member = archive
            .by_name(&rom.name)
            .map_err(|e| format!("{}: {e}", rom.name))?;
        let mut hasher = Hasher::new();
        let mut buf = vec![0; CHUNK];
        loop {
            let n = member
                .read(&mut buf)
                .map_err(|e| format!("{}: {e}", rom.name))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        if !matches(&hasher.finish(), rom) {
            return Err(format!(
                "{}: its payload does not hash to the DAT's",
                rom.name
            ));
        }
    }
    Ok(())
}
