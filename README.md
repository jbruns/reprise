# Title Select

Audits ROM collections against DAT files, and records exactly what was verified and against which DAT revision.

Status: early pilot. A headless Audit command and a store of DAT Revisions, Source Sets and Audits; Selection,
Change Plans and a service follow.
Vocabulary is defined in [GLOSSARY.md](GLOSSARY.md), decisions in [docs/adr/](docs/adr/).

## Audit

```sh
title-select audit --dat <DAT file> <Source Set directory> [--output <report file>]
```

This Audit is stateless: it needs no store or configuration. `audit --set` (below) audits a registered Source Set
and records the Audit in the store; its report has the same shape.

Audits every file under the Source Set directory against a Logiqx XML DAT (for example a No-Intro DAT) and
writes a JSON report to stdout, or to `--output`. It only reads: the Source Set is never changed.

- Every payload is read and checksummed (size, CRC32, MD5, SHA-1, SHA-256). Each member of a `.zip` archive is
  an Entry; any other file, including other archive formats, is one Entry. Nested archives are not opened.
- A Finding is decided by payload before name (ADR 0002). A payload matches a ROM when every checksum the DAT
  supplies for it agrees.
- An unreadable file, directory, archive or member becomes an Unreadable Entry and the Audit continues.
  Symbolic links are followed.
- Exits 0 when the Audit completes, whatever it found. Exits non-zero with a message on stderr when the DAT
  cannot be read or the Source Set directory does not exist.

### Report

```jsonc
{
  "schema": 2,                       // version of this report's shape
  "dat_revision": {                  // the DAT Revision the Audit is pinned to
    "sha256": "254e…",               // SHA-256 of the unmodified DAT file: its identity
    "name": "Atari - Atari 2600",    // from the DAT header
    "version": "20261027-195049",
    "file": "Atari - Atari 2600 (20261027-195049).dat"
  },
  "source_set": "/path/to/set",
  "started_at": "2026-10-28T22:45:46.539Z",
  "finished_at": "2026-10-28T22:45:46.942Z",
  "summary": {
    "findings": { "matched": 786, "misnamed": 19, "conflicting": 3, "mismatched": 0, "unmatched": 41, "unreadable": 0 },
    "games": { "complete": 802, "incomplete": 0, "missing": 70 }
  },
  "entries": [
    {
      "path": "Breakout (Europe).zip",           // relative to the Source Set
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

- `finding` is one of `matched`, `misnamed`, `conflicting`, `mismatched`, `unmatched` or `unreadable` (see the
  glossary).
- `roms` lists the ROMs the Entry counts towards. For a Matched Entry that is every ROM with its name and payload,
  so an identical ROM shared by several Games counts for each. For a Misnamed or Conflicting Entry it is every
  ROM the payload matches, so the Entry completes the Game its payload belongs to.
- `named_rom` is the ROM the Entry's name gives, present when it differs from the payload's (Conflicting, Mismatched).
- `evidence.algorithms` lists what the Finding rests on. For a payload match, it is size plus each checksum that
  every ROM in `roms` supplies; all of them were compared and agreed. A Mismatched or Unmatched Entry has only size and
  CRC32, the lookup that found no matching ROM. An Unreadable Entry has no Evidence and a `reason` instead.
- A Game is `complete`, `incomplete` or `missing`; `missing_roms` lists its ROMs no Entry's payload matched.

## Store

The store is one directory: a SQLite database (`title-select.sqlite`), the unmodified DAT files under
`dats/<sha256>`, and `backups/` (ADR 0003). Pass it with `--store <dir>` or `TITLE_SELECT_STORE`; it is created on
first use.

- The database runs in WAL mode. Every connection uses `synchronous=FULL`, `foreign_keys=ON` and a busy timeout.
- A DAT file is written to a temp file, fsynced and renamed into place before its row is inserted. Files under
  `dats/` that no DAT Revision holds are deleted each time the store opens.
- Migrations are embedded and forward-only. Before one runs on a store that already has a schema, the database is
  copied with `VACUUM INTO` to `backups/schema-<version>-<time>.sqlite`. A store from a newer Title Select is refused.
- A DAT Revision's file is kept while a retained Audit refers to it or while it is the newest imported revision of
  its DAT. Otherwise the file is deleted; the DAT Revision's row stays, so history still names it. Importing the
  same file again restores it.

## Configuration

`title-select.toml`, passed with `--config <file>` or `TITLE_SELECT_CONFIG`, declares the Roots Title Select may
read (ADR 0006). Without it there are no Roots.

```toml
[roots.sets]                # a Root called `sets`
path = "/data/source-sets"
writable = false            # the default; every Root is read-only until Change Plans exist

[audits]
keep = 20                   # full Audits kept per Source Set (the default)
```

## Commands

```sh
title-select dat import <DAT file> --provider <name> [--source-url <url>]
```

Imports an unmodified DAT file as a DAT Revision, recording its SHA-256, header `name` and `version`, original
filename, import time, provider and source URL. A DAT is identified by its provider and header name, written
`<provider>/<name>`; each import of new bytes adds a DAT Revision of it. Importing the same bytes again changes
nothing and prints the existing DAT Revision, restoring its file if the store had dropped it.

```sh
title-select set add <Root> <relative path> --dat <provider>/<name>
title-select set list
title-select set history <Source Set>
```

`set add` registers a folder in a Root, such as `sets "No-Intro/Atari - 2600"`, as a Source Set bound to an imported
DAT. The Source Set is named by its folder (`Atari - 2600`). An absolute path, `..`, or a symlink that resolves
outside the Root is refused. `set list` prints each Source Set with the DAT Revision it was last verified against:
its latest Audit's. `set history` lists a Source Set's Audits, newest first, with the time, DAT Revision, counts, and
whether the full Audit is kept or only its summary.

```sh
title-select audit --set <Source Set> [--revision <sha256 prefix>] [--output <report file>]
```

Audits a registered Source Set, read-only, and records it. Without `--revision`, the DAT Revision is the latest
Audit's, else the newest imported revision of the bound DAT, so a newer DAT Revision is adopted only by naming it.
The Entries and Game Statuses are written in one transaction when the Audit finishes, and the report below is
generated from them. Afterwards, Audits beyond the newest `audits.keep` are pruned to a one-row summary; the latest
Audit, and any Audit cited as provenance, are never pruned.

## Licence

MIT. See [LICENSE](LICENSE).
