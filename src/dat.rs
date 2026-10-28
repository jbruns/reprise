//! Reading a Logiqx XML DAT into a DAT Revision.

use std::collections::HashMap;
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use sha2::Digest;

use crate::Error;

pub struct Rom {
    pub game: usize,
    pub name: String,
    pub size: u64,
    pub crc: Option<String>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
}

pub struct Dat {
    pub sha256: String,
    pub name: String,
    pub version: String,
    pub games: Vec<String>,
    pub roms: Vec<Rom>,
    by_name: HashMap<String, Vec<usize>>,
    by_size_crc: HashMap<(u64, String), Vec<usize>>,
}

fn attrs(e: &BytesStart) -> Result<HashMap<String, String>, Error> {
    let mut out = HashMap::new();
    for a in e.attributes() {
        let a = a.map_err(|err| Error::Dat(err.to_string()))?;
        let key = a.key.as_ref().to_string();
        let value = a
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|err| Error::Dat(err.to_string()))?;
        out.insert(key, value.into_owned());
    }
    Ok(out)
}

impl Dat {
    pub fn read(path: &Path) -> Result<Dat, Error> {
        let bytes =
            std::fs::read(path).map_err(|err| Error::Dat(format!("{}: {err}", path.display())))?;
        let sha256 = crate::hex(&sha2::Sha256::digest(&bytes));
        let mut reader = quick_xml::Reader::from_reader(bytes.as_slice());
        let mut buf = Vec::new();
        let mut dat = Dat {
            sha256,
            name: String::new(),
            version: String::new(),
            games: Vec::new(),
            roms: Vec::new(),
            by_name: HashMap::new(),
            by_size_crc: HashMap::new(),
        };
        let mut stack: Vec<String> = Vec::new();
        let mut saw_root = false;
        loop {
            let event = reader
                .read_event_into(&mut buf)
                .map_err(|err| Error::Dat(format!("{}: {err}", path.display())))?;
            match event {
                Event::Start(e) => {
                    let tag = e.local_name().as_ref().to_string();
                    if tag == "datafile" {
                        saw_root = true;
                    }
                    if tag == "game" || tag == "machine" {
                        dat.start_game(&e)?;
                    }
                    if tag == "rom" {
                        dat.add_rom(&e)?;
                    }
                    stack.push(tag);
                }
                Event::Empty(e) => {
                    let tag = e.local_name().as_ref().to_string();
                    if tag == "rom" {
                        dat.add_rom(&e)?;
                    }
                    if tag == "game" || tag == "machine" {
                        dat.start_game(&e)?;
                    }
                }
                Event::Text(t) => {
                    let in_header = stack.len() >= 2 && stack[stack.len() - 2] == "header";
                    match stack.last().map(String::as_str) {
                        Some("name") if in_header => dat.name.push_str(&t.xml10_content()),
                        Some("version") if in_header => dat.version.push_str(&t.xml10_content()),
                        _ => {}
                    }
                }
                Event::End(_) => {
                    stack.pop();
                }
                Event::Eof => break,
                _ => {}
            }
            buf.clear();
        }
        if !saw_root {
            return Err(Error::Dat(format!(
                "{}: not a Logiqx XML DAT",
                path.display()
            )));
        }
        Ok(dat)
    }

    fn start_game(&mut self, e: &BytesStart) -> Result<(), Error> {
        let a = attrs(e)?;
        let name = a
            .get("name")
            .ok_or_else(|| Error::Dat("a game has no name".into()))?;
        self.games.push(name.clone());
        Ok(())
    }

    fn add_rom(&mut self, e: &BytesStart) -> Result<(), Error> {
        let a = attrs(e)?;
        let game = self
            .games
            .len()
            .checked_sub(1)
            .ok_or_else(|| Error::Dat("a rom is outside any game".into()))?;
        let name = a
            .get("name")
            .ok_or_else(|| Error::Dat("a rom has no name".into()))?
            .clone();
        let size = a
            .get("size")
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| Error::Dat(format!("rom {name} has no valid size")))?;
        let lower = |k: &str| a.get(k).map(|v| v.to_ascii_lowercase());
        let rom = Rom {
            game,
            name,
            size,
            crc: lower("crc"),
            md5: lower("md5"),
            sha1: lower("sha1"),
            sha256: lower("sha256"),
        };
        let idx = self.roms.len();
        self.by_name.entry(rom.name.clone()).or_default().push(idx);
        if let Some(crc) = &rom.crc {
            self.by_size_crc
                .entry((rom.size, crc.clone()))
                .or_default()
                .push(idx);
        }
        self.roms.push(rom);
        Ok(())
    }

    pub fn roms_named(&self, name: &str) -> &[usize] {
        self.by_name.get(name).map_or(&[], Vec::as_slice)
    }

    pub fn roms_with_size_crc(&self, size: u64, crc: &str) -> &[usize] {
        self.by_size_crc
            .get(&(size, crc.to_string()))
            .map_or(&[], Vec::as_slice)
    }
}
