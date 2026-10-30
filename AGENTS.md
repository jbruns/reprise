## Agent skills

Before repository work, run `tools/agent-skills/setup-agent-skills check`. If it fails,
run `tools/agent-skills/setup-agent-skills`, then start a fresh agent session to discover
the pinned skills. Invoke applicable installed skills before proceeding.
For installation, pin updates, or recovery, read `docs/agents/skills.md`.

### Issue tracker

Issues and specs live in this repo's GitHub Issues. Before reading or publishing tickets, read `docs/agents/issue-tracker.md`.

### Triage labels

Use the five default triage labels. Before triaging issues, read `docs/agents/triage-labels.md`.

### Domain docs

Single-context: root `GLOSSARY.md` and `docs/adr/`. Before exploring the codebase, read `docs/agents/domain.md`.

### Build and test

The toolchain is pinned in `rust-toolchain.toml`. CI runs exactly these, so run them before committing:

```sh
tools/retool/setup   # Retool for the Selection tests; needs uv, which fetches Python 3.11
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Tests build their DATs and Verified Sets from synthetic fixtures at test time. Never commit ROMs, and never commit a
real DAT unless its licence permits redistribution.

### Read-only by default

An Audit never writes to the Verified Set it reads (ADR 0002). Only an approved Change Plan may change a Verified Set
or a Library.

### Privacy

Prose (Markdown, code comments, docstrings, CLI help and output) names roles, never people, logins, hostnames,
domains or the deployments that run this application: "the Operator", "the Host", `<domain>`. Load-bearing
identifiers (paths, config keys, `uses:` refs) stay. Refer to this repo's issues as bare `#N`.
