# 1. Rust engine, with pinned Retool for Selection

Date: 2026-10-28

## Status

Accepted. The Rust port is planned for a milestone after the pilot. It is still gated on the conformance suite.

## Context

Title Select needs fast, explainable Audits of large Verified Sets, durable jobs and a service. It also needs a 1G1R
Selection that explains each choice. Existing tools leave gaps: igir has no DAT Revision store, no Change Plans, no
bad, conflicting or unreadable Findings and no jobs; ClrMamePro and RomVault use region-priority parent/clone
selection with no rule trace. Retool's filter-pipeline Selection is the best available, but it is unmaintained Python
of about 7,900 lines of order-sensitive selection code with no written specification.

## Decision

The Audit engine, job engine and service are Rust. Selection runs a pinned Retool commit inside the image, patched to
emit a structured trace that names the rule responsible for each decision. A Rust port of Retool is a later, separately
approved option, gated on a conformance suite that uses the patched Retool as its oracle.

## Consequences

- The image carries a Python runtime for Selection.
- Title Select maintains a small patch set against unmaintained upstream code, and shows clone-list and metadata drift
  against each DAT Revision instead of hiding it.
