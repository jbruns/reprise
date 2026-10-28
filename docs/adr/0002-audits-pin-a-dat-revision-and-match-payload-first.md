# 2. Audits pin a DAT Revision and match payload first

Date: 2026-10-28

## Status

Accepted

## Context

A Verified Set kept without a record of its DAT Revision cannot say how old it is. A prototype Audit showed that
checking an Entry's name before its payload reports a good dump of one revision, stored under another revision's
name, as Bad. DATs also differ per ROM in which checksums they carry: some ROMs have SHA-256, others only CRC32, MD5
and SHA-1.

## Decision

- Every Audit is pinned to one DAT Revision, identified by the SHA-256 of the unmodified DAT file, and its report
  records it.
- An Audit looks an Entry up by payload before name, and reports a name that points at one ROM while the payload
  matches another as Conflicting.
- Evidence is recorded per Entry: the Evidence Level and the algorithms actually compared.
- An Audit opens a Verified Set read-only and never writes to it. An Entry that cannot be read is Unreadable, never
  silently skipped.

## Consequences

- The Verified Set's "last verified against" falls out of its latest Audit.
- Audits cost a full payload read until Claim and incremental Evidence Levels exist, and those must stay labelled
  as such.
