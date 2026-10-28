//! Checksums computed from an Entry's payload.

use std::io::Read;

use sha1::Digest;

use crate::dat::Rom;
use crate::hex;

pub struct Checksums {
    pub size: u64,
    pub crc: String,
    pub md5: String,
    pub sha1: String,
    pub sha256: String,
}

impl Checksums {
    pub fn of_reader(mut r: impl Read) -> std::io::Result<Checksums> {
        let mut crc = crc32fast::Hasher::new();
        let mut md5 = md5::Md5::new();
        let mut sha1 = sha1::Sha1::new();
        let mut sha256 = sha2::Sha256::new();
        let mut size = 0u64;
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let chunk = &buf[..n];
            crc.update(chunk);
            md5.update(chunk);
            sha1.update(chunk);
            sha256.update(chunk);
            size += n as u64;
        }
        Ok(Checksums {
            size,
            crc: format!("{:08x}", crc.finalize()),
            md5: hex(&md5.finalize()),
            sha1: hex(&sha1.finalize()),
            sha256: hex(&sha256.finalize()),
        })
    }

    pub fn matches(&self, rom: &Rom) -> bool {
        rom.size == self.size
            && rom.crc.as_deref().is_none_or(|v| v == self.crc)
            && rom.md5.as_deref().is_none_or(|v| v == self.md5)
            && rom.sha1.as_deref().is_none_or(|v| v == self.sha1)
            && rom.sha256.as_deref().is_none_or(|v| v == self.sha256)
    }
}
