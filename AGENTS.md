# Kanbrick-V1 — Agent Guide

Kanbrick-V1 is a 4-layer Firm Operating System built on an all-Rust stack:

- **Layer 1 — Ironclaw** (`JoasASantos/ironclaw`): secure agent runtime, 13-layer security, RBAC/DLP/audit, embedded Axum UI.
- **Layer 2 — Tachyon-Mesh** (`astorise/Tachyon-Mesh`): WASM (WASIp2) orchestration, IOTA Stronghold secrets, GPU routing.
- **Layer 3 — SparrowDB + SparrowOntology** (`ryaker/SparrowDB`): embedded, Cypher-native, schema-enforced knowledge graph.
- **Layer 4 — graphify-rs** (`TtTRz/graphify-rs`): Rust code/document discovery; exports `cypher.txt` ingested into SparrowDB.

`FirmContext` is the central identity abstraction (user, clearance tier L1–L5, portfolio scope). 12 employees across 5 clearance tiers; 9 portfolio companies across 4 segments.

## Agent skills

### Issue tracker

Issues live as GitHub issues on `Tnsr-Q/Kanbrick-V1`; use the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Canonical triage roles map 1:1 to label strings (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`) plus categories `bug` / `enhancement`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context repo. Domain vocabulary and ADR layout described in `docs/agents/domain.md`.
