# 6. No Host assumptions

Date: 2026-10-29

## Status

Accepted

## Context

Title Select is the Organizer of Source Sets and of the managed parts of a Play Library, so it writes to both through
approved Change Plans. It runs as a reusable container on Hosts it does not control. Some filesystems, such as ZFS with
block cloning or Btrfs and XFS with reflinks, can copy without duplicating blocks. Depending on them, or on Host
privileges, would tie Title Select to one Host.

## Decision

- Title Select needs no privileged access, no Docker control, and no particular filesystem. It needs only its Roots,
  mounted read-write where Change Plans write to them and read-only elsewhere. Backups and snapshots of Roots are the
  Operator's concern, outside Title Select.
- Each Root's configuration declares whether Title Select may write to it. Title Select refuses a Change Plan that
  writes to a read-only Root.
- An Audit opens a Source Set read-only even when its Root is mounted read-write (ADR 0002).
- Copies use `copy_file_range`, which shares blocks where the filesystem supports it and falls back to a byte copy
  otherwise. Every copy is re-hashed against the expected payload, whichever path it took.
- Title Select never uses hardlinks between Roots.

## Consequences

- Block sharing is an optimisation Title Select may report but never relies on for correctness or space planning.
