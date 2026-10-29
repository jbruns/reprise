# 5. Leptos client-side UI over the versioned JSON API

Date: 2026-10-29

## Status

Accepted

## Context

The workbench shows tens of thousands of Entries for one system, filters them as the Operator types, and updates live
as Jobs progress. The Operator wants one Rust toolchain and one build. A server-rendered UI, with htmx and a SQLite
query per keystroke, was the alternative. It needs no WASM target, but its responsiveness depends on round trips and
it suits a page with many live, mutating signals less well.

## Decision

- The UI is a Leptos application rendered on the client and compiled to WASM. The server serves its bundle with
  `cargo leptos build` as the single build.
- The UI uses the same versioned JSON API as the headless client. It does not use Leptos server functions, because
  they would form a second, unversioned API.
- API request and response types live in a shared crate compiled into both the server and the UI.
- Job progress and state changes reach the UI as server-sent events.

## Consequences

- The workspace builds a `wasm32-unknown-unknown` target as well as the server.
- Filtering a system's Entries happens in the client. Slice 1d's acceptance sets a budget of 100 ms at p95 from
  keystroke to render for 100,000 Entries.
