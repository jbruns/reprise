# 3. SQLite store with content-addressed blobs

Date: 2026-10-28

## Status

Accepted

## Context

The Service needs durable DAT Revisions, a Verified Set registry, Audits, jobs and, later, Selections and Change
Plans. Jobs and Change Plans need transactional state changes that survive a restart. Imported DATs must stay
byte-exact. The UI filters and pages Audit Entries in real time, and an Audit of a Redump or MAME set can hold over
100,000 Entries. The alternatives were plain JSON files, which have no transactions or indexed queries, and an external
Postgres, which adds a Service to run and back up for a single-Operator application.

## Decision

- One SQLite database in the Service's data volume. Blobs, starting with unmodified DAT files, are stored beside it
  under `dats/<sha256>`, addressed by their SHA-256.
- WAL mode, with one writer connection that serialises writes and a pool of readers. Every connection sets
  `synchronous=FULL`, `foreign_keys=ON` and a `busy_timeout`.
- A blob is written to a temp file in the store directory, fsynced, renamed into place, and its directory fsynced, all
  before the row that refers to it is inserted. A blob without a row is garbage; a row without its blob cannot occur.
- Each job state transition is one transaction. An Audit's Entries and Game Statuses are rows, written in one
  transaction when the Audit finishes, so a partial Audit is never visible. The versioned JSON report is generated from
  those rows.
- Migrations are embedded and forward-only. Each runs at startup in a transaction, after a `VACUUM INTO` backup.
- A DAT Revision's blob is kept while a retained Audit, a Selection or a Change Plan refers to it, or while it is the
  newest imported revision of its DAT. Otherwise it is deleted. The DAT Revision's row stays, with its SHA-256,
  header and provenance, so history still names it. Importing the same bytes again restores the blob.
- Backups use SQLite's online backup, never a raw copy of the database files.

## Consequences

- The database and its blobs must live on one volume that supports fsync and atomic rename.
- One writer bounds write throughput. That is enough for one Operator, and readers never block it in WAL mode.
