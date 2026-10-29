# Title Select

Audits ROM collections against DAT files, and records exactly what was verified and against which DAT revision.

Status: early pilot. The first slice is a headless Audit command; Selection, Change Plans and a service follow.
Vocabulary is defined in [GLOSSARY.md](GLOSSARY.md), decisions in [docs/adr/](docs/adr/).

## Audit

```sh
title-select audit --dat <DAT file> <Verified Set directory> [--output <report file>]
```

Audits every file under the Verified Set directory against a Logiqx XML DAT (for example a No-Intro DAT) and
writes a JSON report to stdout, or to `--output`. It only reads: the Verified Set is never changed.

- Every payload is read and checksummed (size, CRC32, MD5, SHA-1, SHA-256). Each member of a `.zip` archive is
  an Entry; any other file, including other archive formats, is one Entry. Nested archives are not opened.
- A Finding is decided by payload before name (ADR 0002). A payload matches a ROM when every checksum the DAT
  supplies for it agrees.
- An unreadable file, directory, archive or member becomes an Unreadable Entry and the Audit continues.
  Symbolic links are followed.
- Exits 0 when the Audit completes, whatever it found. Exits non-zero with a message on stderr when the DAT
  cannot be read or the Verified Set directory does not exist.

### Report

```jsonc
{
  "schema": 1,                       // version of this report's shape
  "dat_revision": {                  // the DAT Revision the Audit is pinned to
    "sha256": "254e…",               // SHA-256 of the unmodified DAT file: its identity
    "name": "Atari - Atari 2600",    // from the DAT header
    "version": "20261027-195049",
    "file": "Atari - Atari 2600 (20261027-195049).dat"
  },
  "verified_set": "/path/to/set",
  "started_at": "2026-10-28T22:45:46.539Z",
  "finished_at": "2026-10-28T22:45:46.942Z",
  "summary": {
    "findings": { "matched": 786, "misnamed": 19, "conflicting": 3, "bad": 0, "extra": 41, "unreadable": 0 },
    "games": { "complete": 802, "incomplete": 0, "missing": 70 }
  },
  "entries": [
    {
      "path": "Breakout (Europe).zip",           // relative to the Verified Set
      "member": "Breakout (Europe).a26",         // present for a ZIP member
      "finding": "conflicting",
      "roms": [{ "game": "Breakout (Europe) (Rev 1)", "name": "Breakout (Europe) (Rev 1).a26" }],
      "named_rom": { "game": "Breakout (Europe)", "name": "Breakout (Europe).a26" },
      "evidence": { "level": "payload", "algorithms": ["size", "crc32", "md5", "sha1", "sha256"] }
    }
  ],
  "games": [
    { "name": "Breakout (Europe)", "status": "missing", "missing_roms": ["Breakout (Europe).a26"] }
  ]
}
```

- `finding` is one of `matched`, `misnamed`, `conflicting`, `bad`, `extra` or `unreadable` (see the glossary).
- `roms` lists the ROMs the Entry counts towards. For a Matched Entry that is every ROM with its name and payload,
  so an identical ROM shared by several Games counts for each. For a Misnamed or Conflicting Entry it is every
  ROM the payload matches, so the Entry completes the Game its payload belongs to.
- `named_rom` is the ROM the Entry's name gives, present when it differs from the payload's (Conflicting, Bad).
- `evidence.algorithms` lists what the Finding rests on. For a payload match, it is size plus each checksum that
  every ROM in `roms` supplies; all of them were compared and agreed. A Bad or Extra Entry has only size and
  CRC32, the lookup that found no matching ROM. An Unreadable Entry has no Evidence and a `reason` instead.
- A Game is `complete`, `incomplete` or `missing`; `missing_roms` lists its ROMs no Entry's payload matched.

## Licence

MIT. See [LICENSE](LICENSE).
