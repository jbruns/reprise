//! Our TorrentZip writer against an archive made by an independent tool.

use std::io::Cursor;

use title_select_engine::torrentzip::{Writer, compare_names, validate};

/// `tests/data/golden-game.zip` was made once by igir 5.8.0 (`igir copy zip`), which writes TorrentZip with zlib
/// 1.1.3, from a one-Game DAT listing these synthetic payloads.
fn golden_members() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("b.bin", b"title-select golden payload ".repeat(64)),
        (
            "A.bin",
            (0..1000u32).map(|i| (i * 31 % 251) as u8).collect(),
        ),
        ("Café.bin", b"cafe".to_vec()),
        ("★ star.bin", b"star".to_vec()),
    ]
}

#[test]
fn the_writer_reproduces_an_independent_torrentzip_byte_for_byte() {
    let golden = include_bytes!("data/golden-game.zip");
    let mut members = golden_members();
    members.sort_by(|a, b| compare_names(a.0, b.0));
    let mut writer = Writer::new(Cursor::new(Vec::new())).unwrap();
    for (name, payload) in &members {
        writer
            .add(name, payload.len() as u64, &mut payload.as_slice())
            .unwrap();
    }

    let written = writer.finish().unwrap().into_inner();

    assert_eq!(written, golden);
    let checked = validate(&mut Cursor::new(golden)).unwrap();
    assert_eq!(
        checked
            .iter()
            .map(|m| (m.name.as_str(), m.utf8))
            .collect::<Vec<_>>(),
        [
            ("A.bin", false),
            ("b.bin", false),
            ("Café.bin", false),
            ("★ star.bin", true)
        ]
    );
}
