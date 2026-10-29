# Title Select

Audits ROM collections against DAT files, and records exactly what was verified and against which DAT revision.

Status: early pilot. A headless Audit command, a store of DAT Revisions, Source Sets and Audits, and the Service: a
JSON API that runs Audits as durable Jobs. Selection, Change Plans and a UI follow.
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
first use. Only one Title Select process may have a store open at a time: it holds an exclusive lock on
`<store>/lock`, and another process fails at once with "the store is in use by another Title Select process".

- The database runs in WAL mode. Every connection uses `synchronous=FULL`, `foreign_keys=ON` and a busy timeout.
- A DAT file is written to a temp file, fsynced and renamed into place before its row is inserted. Files under
  `dats/` that no DAT Revision holds are deleted each time the store opens.
- Migrations are embedded and forward-only. Before one runs on a store that already has a schema, the database is
  copied with `VACUUM INTO` to `backups/schema-<version>-<time>.sqlite`. A store from a newer Title Select is refused.
- A DAT Revision's file is kept while a retained or running Audit refers to it or while it is the newest imported
  revision of its DAT. Otherwise the file is deleted after the next import or Audit; the DAT Revision's row stays, so
  history still names it. Importing the same file again restores it and keeps it until an Audit using it finishes or a
  newer revision of its DAT is imported.

## Configuration

`title-select.toml`, passed with `--config <file>` or `TITLE_SELECT_CONFIG`, declares the Roots Title Select may
read (ADR 0006). Without it there are no Roots.

```toml
[roots.sets]                # a Root called `sets`
path = "/data/source-sets"
writable = false            # the default; every Root is read-only until Change Plans exist

[audits]
keep = 20                   # full Audits kept per Source Set (the default)

[auth]                      # how the Service knows the Operator; see "Service"
identity_header = "Remote-User"
trusted_proxies = ["10.0.0.0/8"]
# dev_identity = "Developer"

[jobs]
concurrent = 1              # Jobs run at once (the default)
pace = "default"            # gentle | default | unthrottled
# readers = 2               # overrides the pace's payload readers
# hashers = 2               # overrides the pace's hash workers
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
its latest Audit's, the latest being the last to finish. `set history` lists a Source Set's Audits, latest first, with
the time, DAT Revision, counts, and whether the full Audit is kept or only its summary.

```sh
title-select audit --set <Source Set> [--revision <sha256 prefix>] [--output <report file>]
```

Audits a registered Source Set, read-only, and records it. Unlike the stateless Audit, it does not follow a symlink that
resolves outside the Root: such a file or folder is an Unreadable Entry with that reason. Without `--revision`, the DAT
Revision is the latest Audit's, else the newest imported revision of the bound DAT, so a newer DAT Revision is adopted
only by naming it. The Entries and Game Statuses are written in one transaction when the Audit finishes, and the report
below is generated from them. Afterwards, Audits beyond the newest `audits.keep` are pruned to a one-row summary; the
latest Audit, and any Audit cited as provenance, are never pruned.

## Service

```sh
title-select serve [--listen <address:port>] [--ui <dir>]
```

Runs the Service: the `/api/v1` JSON API, and the Jobs that run Audits (ADR 0003). It listens on `--listen` or
`TITLE_SELECT_LISTEN`, by default `127.0.0.1:8080`, and holds the store, and its lock, until it stops on SIGTERM or
Ctrl-C. While it runs, the store commands above fail with "the store is in use"; use the API, or the headless Audit
below, instead. This is the supported way to operate Title Select on the Host. With `--ui` or `TITLE_SELECT_UI` naming
the workbench's built bundle, it also serves the workbench at `/`; see "Workbench".

### Auth

Every API path but `/api/v1/health` needs the Operator (ADR 0004). The Service refuses to start without a way to
authenticate the Operator:

- `[auth] identity_header` with `[auth] trusted_proxies`: an authenticating reverse proxy sets the header to the
  Operator's identity. The header is trusted only from a peer inside one of the CIDRs, and a request carrying it from
  any other peer is rejected. The identity is recorded as the actor of each Job the request starts.
- The Operator token: a file named by `TITLE_SELECT_OPERATOR_TOKEN_FILE` (or `--token-file`) holds a secret, sent as
  `Authorization: Bearer <token>`. The Service keeps only its SHA-256 and compares in constant time. Jobs it starts
  record the actor `Operator token`.
- `[auth] dev_identity`, for local development: a request with no credential is the Operator under that identity. The
  Service refuses to start with it unless it listens on a loopback address.

Any other request, or a wrong token, gets `401`.

### Jobs

An Audit started through the API is a Job: `queued`, `running`, then `succeeded` or `failed`. Each change of state is
one transaction, and a Job runs to the end whether or not its client stays connected. The DAT Revision is chosen when
the Job is queued, as for `audit --set`, and its file is kept until the Job ends. A Source Set has at most one queued
or running Job; starting another is refused with `409`, naming the Job.

A Job found running when the Service starts was cut off by a stop: its unfinished Audit is discarded, the Job becomes
`interrupted`, and a fresh Job (attempt 2) is queued for it. If that Job is cut off too, it becomes `failed`.

`[jobs] concurrent` Jobs run at once, 1 by default. Each Audit reads payloads on `readers` threads, which also
decompress ZIP members, and checksums them on `hashers` threads. The pace sets both:

| `pace`        | readers | hashers | for                                                          |
|---------------|---------|---------|--------------------------------------------------------------|
| `gentle`      | 1       | 1       | one file at a time, leaving disks and CPUs to other Services |
| `default`     | 2       | 2       | the default: overlaps one file's reading with another's hash |
| `unthrottled` | CPUs    | CPUs    | one of each per CPU, for an otherwise idle Host              |

`[jobs] readers` or `hashers` overrides the pace's count. A Job records the counts it was queued with.

### API

Every body is JSON, and every error is `{"error": "...", "job": <id, when one is named>}`.

| Method and path                                    | What                                                         |
|----------------------------------------------------|--------------------------------------------------------------|
| `GET /api/v1/health`                               | `{"status": "ok"}`, unauthenticated                          |
| `GET /api/v1/roots`                                | the configured Roots                                         |
| `GET /api/v1/dats`                                 | each DAT with its DAT Revisions                              |
| `GET /api/v1/dat-revisions`                        | every DAT Revision                                           |
| `POST /api/v1/dat-revisions`                       | import a DAT: multipart `provider`, `source_url`?, `file`    |
| `GET /api/v1/source-sets`                          | the Source Sets, with the DAT Revision last verified against |
| `POST /api/v1/source-sets`                         | register one: `{"root", "path", "dat"}`                      |
| `GET /api/v1/source-sets/{name}`                   | one Source Set                                               |
| `GET /api/v1/source-sets/{name}/history`           | its Audits, latest first                                     |
| `POST /api/v1/audits`                              | queue an Audit Job: `{"source_set", "revision"?}`; `202`     |
| `GET /api/v1/audits/{id}`                          | an Audit's summary                                           |
| `GET /api/v1/audits/{id}/report`                   | the full `schema: 2` report                                  |
| `GET /api/v1/audits/{id}/entries`                  | Entries, paged; `?finding=`                                  |
| `GET /api/v1/audits/{id}/entries.ndjson`           | every Entry, one JSON object per line                        |
| `GET /api/v1/audits/{id}/games`                    | Game Statuses, paged; `?status=`                             |
| `GET /api/v1/jobs`                                 | the latest 100 Jobs, newest first                            |
| `GET /api/v1/jobs/{id}`                            | one Job                                                      |
| `GET /api/v1/jobs/{id}/events`                     | server-sent events for one Job, ending at its final state    |
| `GET /api/v1/events`                               | server-sent events for every Job, opening with a `resync`    |

A paged list is `{"items": [...], "next_cursor": "..."}`; pass `next_cursor` back as `?cursor=` for the next page, and
`?limit=` (1 to 1000, default 100) for its size. Each item carries its `seq`, its position in the report. A DAT upload
may be up to 512 MiB, and is `201` when it adds a DAT Revision, `200` when the bytes were already imported.

Server-sent events carry JSON data, under one of these event names:

| Event      | Data                                   | Sent                                                         |
| ---------- | -------------------------------------- | ------------------------------------------------------------ |
| `state`    | the Job                                | when a Job's state changes; first on a Job's own stream      |
| `progress` | `{"job", "files_done", "files_total"}` | while a Job is Running, at most five times a second          |
| `resync`   | `{"jobs": [...]}`, newest first        | by `/events` on connecting, and after the client fell behind |

A `resync` lists every Queued or Running Job and the 50 most recently finished; it replaces what the client knew,
since events in between may have been dropped. An idle stream sends a comment every 15 seconds.

Every Job carries a `version` that goes up with each change of its state, and `progress` carries the `version` of the
Running Job it belongs to. The Service drops events that a `resync` or a later `state` already superseded, and a
client should do the same: ignore a `state` whose `version` is at or below the one it holds for that Job, and
`progress` whose `version` is below it.

### Headless Audit

```sh
title-select audit --server <url> --set <Source Set> [--revision <sha256 prefix>] [--output <report file>]
```

Starts an Audit Job on the Service at `<url>`, sending the Operator token from `TITLE_SELECT_OPERATOR_TOKEN_FILE` (or
`--token-file`), and follows it to the end. Progress and a summary of the Findings and Game Statuses go to stderr; the
report goes to stdout, or to `--output`. If the Job's event stream sends nothing, not even its keep-alive, for 45
seconds, the client polls the Job's state instead. Exits 0 when the Audit completes, and non-zero when the Job fails,
the token is refused or the Service cannot be reached.

### Image

`ghcr.io/jbruns/title-select` is a static binary and the workbench's bundle (at `/ui`) on
`gcr.io/distroless/static:nonroot`, for linux/amd64. It runs `serve` on `0.0.0.0:8080` as the nonroot user, with the
store at the volume `/store`, and serves the workbench at `/`. It needs nothing else writable, so it runs with a
read-only root filesystem:

```sh
docker run --read-only -p 8080:8080 -v title-select-store:/store \
  -v /path/to/token:/run/operator-token:ro -e TITLE_SELECT_OPERATOR_TOKEN_FILE=/run/operator-token \
  -v /path/to/title-select.toml:/etc/title-select.toml:ro -e TITLE_SELECT_CONFIG=/etc/title-select.toml \
  -v /data/source-sets:/data/source-sets:ro ghcr.io/jbruns/title-select:sha-<commit>
```

CI publishes `sha-<commit>` (the full commit SHA) for each push to `main`, and `<version>` and `<major>.<minor>` for
each `v<version>` tag.

## Workbench

The workbench is the Operator's browser interface to the Service (ADR 0005): a Leptos app compiled to WebAssembly that
uses only the `/api/v1` JSON API, from the same origin. Its screens:

- **Source Sets** lists each Source Set with its latest Audit and the DAT Revision it was last verified against, and
  registers another from a Root, a path in it and a DAT. Opening a Source Set shows its facts, starts an Audit against
  the default DAT Revision or a newer one, and lists its history, latest first, with a Re-audit for each row.
- **DAT Revisions** lists every DAT Revision, newest import first within each DAT, with its provenance (provider,
  source URL, file, SHA-256, import time), whether its file is kept, and the Audits that refer to it. It uploads a DAT
  file as a new DAT Revision.
- **Audit** shows the Audit's Source Set, DAT Revision and times, exports its JSON report and re-audits. Its summary
  counts each Finding and Game Status, and two derived states: an Entry is Verified when it is Matched with Payload
  Evidence, and Needs Review when it is Misnamed, Conflicting, Mismatched, Unmatched or Unreadable. Its Entries load in
  one request from the bulk NDJSON endpoint, then filter by name, Finding, state, Game Status and Evidence algorithm,
  and sort by any column, in the browser as each key is typed. Selecting an Entry shows its Evidence and the ROMs it
  matches. A pruned Audit shows only its summary.
- **Jobs** lists the latest Jobs with their state and progress, live: it follows `/api/v1/events`, applies `state`
  and `progress` events by the Job `version` rule above, and replaces what it holds with each `resync` snapshot,
  which also arrives after a reconnect.

Every table is virtualized, sorts by its column headers and works from the keyboard: Tab to the table, then the arrow
keys, Page Up, Page Down, Home and End move the selected row, Enter opens it and Escape clears it. Each screen's
controls are ordinary buttons, links and form fields. The workbench needs an evergreen browser; it makes no request
to any other origin and keeps no credential in browser storage. Its fonts, Liberation Sans and Liberation Mono, are
served with it under the SIL Open Font License (`crates/ui/assets/fonts/LICENSE-OFL.txt`).

The workbench's own files are served without auth, with a same-origin Content Security Policy; everything it shows
comes from the API, which needs the Operator as above. Behind an authenticating reverse proxy, the proxy's identity
header covers the browser's API requests. To run it locally, with `[auth] dev_identity` on a loopback address:

```sh
rustup target add wasm32-unknown-unknown   # rust-toolchain.toml lists it
cargo install cargo-leptos --locked
cargo leptos build --release               # the workbench into target/site, and the binary
printf '[roots.sets]\npath = "/data/source-sets"\n\n[auth]\ndev_identity = "Developer"\n' > dev.toml
target/release/title-select --store dev-store --config dev.toml serve --ui target/site
```

Then open `http://127.0.0.1:8080/`.

`crates/ui/bench/filter.py` measures the Audit screen's filtering over 100,000 synthetic Entries in headless Chromium.
It builds the workbench with the `bench` feature, which adds a `/bench` screen of synthetic Entries that no release or
image has, serves it on a loopback port with a scratch store under `target/bench`, and reports p50 and p95. It needs
Playwright for Python; from the repository's root:

```sh
python3 -m venv .venv && .venv/bin/pip install playwright && .venv/bin/playwright install chromium
.venv/bin/python crates/ui/bench/filter.py
```

## Development

A Cargo workspace: `crates/api` (the API's and the report's types), `crates/engine` (the Audit), `crates/store`
(the store and configuration), `crates/server` (the Service), `crates/cli` (the `title-select` binary) and
`crates/ui` (the workbench). The workbench's filtering, sorting, derived states and Job event handling are plain Rust,
tested on the host with `cargo test`; `cargo clippy --package title-select-ui --target wasm32-unknown-unknown` checks
it as the browser runs it.

## Licence

MIT. See [LICENSE](LICENSE).
