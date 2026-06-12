# Kanbrick-V1 — Published Issues

All 51 implementation issues were published to **[Tnsr-Q/Kanbrick-V1](https://github.com/Tnsr-Q/Kanbrick-V1)** on 2026-06-12.

- **Repository:** https://github.com/Tnsr-Q/Kanbrick-V1
- **Issues board:** https://github.com/Tnsr-Q/Kanbrick-V1/issues
- **Total issues:** 51 (38 AFK / 13 HITL) across 7 phases
- **Numbering note:** GitHub issue numbers run **#3–#53** (#1 was the templates PR, #2 a permission-probe). Cross-references inside each issue body use these real numbers and resolve correctly.

> **Labels pending:** the Abacus GitHub App currently has *create-only* issue access, so labels could not be attached automatically. Grant **Issues: Read & write** to the app, then a labeling pass can apply the intended labels (listed per issue below).


### Phase 0 — Repository Setup & Dependency Resolution

- [**#3**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/3) — Cargo workspace scaffold with empty member crates
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-0`, `AFK`
- [**#4**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/4) — Vendor the four upstream repos & resolve version conflicts
  - Type: HITL · Intended labels: `needs-triage`, `enhancement`, `phase-0`, `HITL`
  - Blocked by: #3
- [**#5**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/5) — CI pipeline — build, lint, and test gates
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-0`, `AFK`
  - Blocked by: #4

### Phase 1 — Foundation: SparrowDB + Schema

- [**#6**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/6) — SparrowDB embedded lifecycle — init, open, close
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-1`, `AFK`
  - Blocked by: #4
- [**#7**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/7) — ClearanceLevel enum & core shared types
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-1`, `AFK`
  - Blocked by: #3
- [**#8**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/8) — Firm schema — Person, Company, Segment nodes & edges
  - Type: HITL · Intended labels: `needs-triage`, `enhancement`, `phase-1`, `HITL`
  - Blocked by: #6, #7
- [**#9**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/9) — Cypher query executor — parameterized queries & deserialization
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-1`, `AFK`
  - Blocked by: #6, #8
- [**#10**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/10) — Migration system — versioned schema & seed data
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-1`, `AFK`
  - Blocked by: #9
- [**#11**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/11) — Seed data loader CLI — kanbrick-cli seed
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-1`, `AFK`
  - Blocked by: #10
- [**#12**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/12) — Org chart hierarchy verification query
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-1`, `AFK`
  - Blocked by: #11

### Phase 2 — Identity & Auth: Ironclaw Security + FirmContext

- [**#13**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/13) — FirmContext struct & construction from JWT claims
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-2`, `AFK`
  - Blocked by: #7
- [**#14**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/14) — Ironclaw JWT issuance & validation
  - Type: HITL · Intended labels: `needs-triage`, `enhancement`, `phase-2`, `HITL`
  - Blocked by: #13, #4
- [**#15**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/15) — Login flow — email + password to JWT with clearance
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-2`, `AFK`
  - Blocked by: #14, #11
- [**#16**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/16) — Clearance gate middleware — require_clearance(level)
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-2`, `AFK`
  - Blocked by: #14
- [**#17**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/17) — Post-query clearance filtering — filter_by_clearance
  - Type: HITL · Intended labels: `needs-triage`, `enhancement`, `phase-2`, `HITL`
  - Blocked by: #13, #9
- [**#18**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/18) — SparrowDB query interceptor — auto-inject clearance filters
  - Type: HITL · Intended labels: `needs-triage`, `enhancement`, `phase-2`, `HITL`
  - Blocked by: #17, #9
- [**#19**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/19) — Audit log — every authenticated query logged
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-2`, `AFK`
  - Blocked by: #18
- [**#20**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/20) — API key management — service-to-service auth for WASM guests
  - Type: HITL · Intended labels: `needs-triage`, `enhancement`, `phase-2`, `HITL`
  - Blocked by: #14

### Phase 3 — Orchestration: Tachyon-Mesh WASM Runtime

- [**#21**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/21) — Tachyon-Mesh runtime init & echo WASM guest
  - Type: HITL · Intended labels: `needs-triage`, `enhancement`, `phase-3`, `HITL`
  - Blocked by: #4
- [**#22**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/22) — Host-Guest ABI — HostFunctions & GuestModule traits
  - Type: HITL · Intended labels: `needs-triage`, `enhancement`, `phase-3`, `HITL`
  - Blocked by: #21
- [**#23**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/23) — FirmContext passthrough — HTTP to mesh to guest
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-3`, `AFK`
  - Blocked by: #22, #16
- [**#24**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/24) — Guest query_graph host function — WASM to SparrowDB round-trip
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-3`, `AFK`
  - Blocked by: #23, #9
- [**#25**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/25) — Task scheduler — immediate execution with timeout enforcement
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-3`, `AFK`
  - Blocked by: #21
- [**#26**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/26) — Task scheduler — scheduled (cron-like) & event-triggered execution
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-3`, `AFK`
  - Blocked by: #25
- [**#27**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/27) — Inter-module event bus — typed events & subscriptions
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-3`, `AFK`
  - Blocked by: #22
- [**#28**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/28) — WASM memory & resource enforcement
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-3`, `AFK`
  - Blocked by: #21
- [**#29**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/29) — WASM module hot-reload
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-3`, `AFK`
  - Blocked by: #25

### Phase 4 — Discovery: graphify-rs → SparrowDB Integration

- [**#30**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/30) — graphify-rs to SparrowDB adapter layer
  - Type: HITL · Intended labels: `needs-triage`, `enhancement`, `phase-4`, `HITL`
  - Blocked by: #6, #4
- [**#31**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/31) — reporting_path — shortest path between two persons
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-4`, `AFK`
  - Blocked by: #30
- [**#32**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/32) — span_of_control — direct + indirect report counts
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-4`, `AFK`
  - Blocked by: #30
- [**#33**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/33) — company_stakeholders — all persons managing a company
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-4`, `AFK`
  - Blocked by: #30
- [**#34**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/34) — segment_overview & cross_segment_links
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-4`, `AFK`
  - Blocked by: #30
- [**#35**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/35) — org_neighborhood & common_manager
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-4`, `AFK`
  - Blocked by: #30
- [**#36**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/36) — Clearance-aware discovery — filtered graph traversal
  - Type: HITL · Intended labels: `needs-triage`, `enhancement`, `phase-4`, `HITL`
  - Blocked by: #30, #17
- [**#37**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/37) — Discovery caching layer with TTL & invalidation
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-4`, `AFK`
  - Blocked by: #30
- [**#38**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/38) — Code graph ingest — graphify-rs cypher.txt into SparrowDB
  - Type: HITL · Intended labels: `needs-triage`, `enhancement`, `phase-4`, `HITL`
  - Blocked by: #6, #4

### Phase 5 — Business Logic: WASM Guests

- [**#39**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/39) — kanbrick-guest-sdk — typed bindings to the host ABI
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-5`, `AFK`
  - Blocked by: #22
- [**#40**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/40) — WASM guest build toolchain validation
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-5`, `AFK`
  - Blocked by: #39, #5
- [**#41**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/41) — Compliance guest — org chart integrity check
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-5`, `AFK`
  - Blocked by: #39, #24
- [**#42**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/42) — Compliance guest — clearance consistency validation
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-5`, `AFK`
  - Blocked by: #41
- [**#43**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/43) — Reporting guest — portfolio dashboard (L5 full view)
  - Type: HITL · Intended labels: `needs-triage`, `enhancement`, `phase-5`, `HITL`
  - Blocked by: #39, #24
- [**#44**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/44) — Reporting guest — clearance-tiered output (L1–L4 filtered views)
  - Type: HITL · Intended labels: `needs-triage`, `enhancement`, `phase-5`, `HITL`
  - Blocked by: #43
- [**#45**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/45) — Valuation guest — DCF model for portfolio companies
  - Type: HITL · Intended labels: `needs-triage`, `enhancement`, `phase-5`, `HITL`
  - Blocked by: #39, #24
- [**#46**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/46) — Cross-guest event flow — valuation triggers reporting
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-5`, `AFK`
  - Blocked by: #27, #45, #43

### Phase 6 — Testing & Validation

- [**#47**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/47) — E2E test suite — full lifecycle at all 5 clearance levels
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-6`, `AFK`
  - Blocked by: #41, #43, #45
- [**#48**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/48) — Security audit tests — escalation, injection, escape
  - Type: HITL · Intended labels: `needs-triage`, `enhancement`, `phase-6`, `HITL`
  - Blocked by: #47
- [**#49**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/49) — Performance benchmarks — latency, throughput, memory
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-6`, `AFK`
  - Blocked by: #47
- [**#50**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/50) — Data integrity validation — compliance guest as system check
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-6`, `AFK`
  - Blocked by: #41, #42
- [**#51**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/51) — "One Command" build validation — clone-to-running-system
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-6`, `AFK`
  - Blocked by: #47
- [**#52**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/52) — Documentation — README, ARCHITECTURE, SECURITY, CONTRIBUTING
  - Type: HITL · Intended labels: `needs-triage`, `enhancement`, `phase-6`, `HITL`
  - Blocked by: #51
- [**#53**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/53) — Deployment artifacts — static binary & optional Docker image
  - Type: AFK · Intended labels: `needs-triage`, `enhancement`, `phase-6`, `AFK`
  - Blocked by: #51
