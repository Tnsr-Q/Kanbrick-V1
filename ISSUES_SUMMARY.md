# Kanbrick-V1 — Published Issues Summary

Total: 51 issues across 7 phases.


### Phase 0 — Repository Setup & Dependency Resolution

- **#3** — Cargo workspace scaffold with empty member crates  _(AFK)_
- **#4** — Vendor the four upstream repos & resolve version conflicts  _(HITL)_
- **#5** — CI pipeline — build, lint, and test gates  _(AFK)_

### Phase 1 — Foundation: SparrowDB + Schema

- **#6** — SparrowDB embedded lifecycle — init, open, close  _(AFK)_
- **#7** — ClearanceLevel enum & core shared types  _(AFK)_
- **#8** — Firm schema — Person, Company, Segment nodes & edges  _(HITL)_
- **#9** — Cypher query executor — parameterized queries & deserialization  _(AFK)_
- **#10** — Migration system — versioned schema & seed data  _(AFK)_
- **#11** — Seed data loader CLI — kanbrick-cli seed  _(AFK)_
- **#12** — Org chart hierarchy verification query  _(AFK)_

### Phase 2 — Identity & Auth: Ironclaw Security + FirmContext

- **#13** — FirmContext struct & construction from JWT claims  _(AFK)_
- **#14** — Ironclaw JWT issuance & validation  _(HITL)_
- **#15** — Login flow — email + password to JWT with clearance  _(AFK)_
- **#16** — Clearance gate middleware — require_clearance(level)  _(AFK)_
- **#17** — Post-query clearance filtering — filter_by_clearance  _(HITL)_
- **#18** — SparrowDB query interceptor — auto-inject clearance filters  _(HITL)_
- **#19** — Audit log — every authenticated query logged  _(AFK)_
- **#20** — API key management — service-to-service auth for WASM guests  _(HITL)_

### Phase 3 — Orchestration: Tachyon-Mesh WASM Runtime

- **#21** — Tachyon-Mesh runtime init & echo WASM guest  _(HITL)_
- **#22** — Host-Guest ABI — HostFunctions & GuestModule traits  _(HITL)_
- **#23** — FirmContext passthrough — HTTP to mesh to guest  _(AFK)_
- **#24** — Guest query_graph host function — WASM to SparrowDB round-trip  _(AFK)_
- **#25** — Task scheduler — immediate execution with timeout enforcement  _(AFK)_
- **#26** — Task scheduler — scheduled (cron-like) & event-triggered execution  _(AFK)_
- **#27** — Inter-module event bus — typed events & subscriptions  _(AFK)_
- **#28** — WASM memory & resource enforcement  _(AFK)_
- **#29** — WASM module hot-reload  _(AFK)_

### Phase 4 — Discovery: graphify-rs → SparrowDB Integration

- **#30** — graphify-rs to SparrowDB adapter layer  _(HITL)_
- **#31** — reporting_path — shortest path between two persons  _(AFK)_
- **#32** — span_of_control — direct + indirect report counts  _(AFK)_
- **#33** — company_stakeholders — all persons managing a company  _(AFK)_
- **#34** — segment_overview & cross_segment_links  _(AFK)_
- **#35** — org_neighborhood & common_manager  _(AFK)_
- **#36** — Clearance-aware discovery — filtered graph traversal  _(HITL)_
- **#37** — Discovery caching layer with TTL & invalidation  _(AFK)_
- **#38** — Code graph ingest — graphify-rs cypher.txt into SparrowDB  _(HITL)_

### Phase 5 — Business Logic: WASM Guests

- **#39** — kanbrick-guest-sdk — typed bindings to the host ABI  _(AFK)_
- **#40** — WASM guest build toolchain validation  _(AFK)_
- **#41** — Compliance guest — org chart integrity check  _(AFK)_
- **#42** — Compliance guest — clearance consistency validation  _(AFK)_
- **#43** — Reporting guest — portfolio dashboard (L5 full view)  _(HITL)_
- **#44** — Reporting guest — clearance-tiered output (L1–L4 filtered views)  _(HITL)_
- **#45** — Valuation guest — DCF model for portfolio companies  _(HITL)_
- **#46** — Cross-guest event flow — valuation triggers reporting  _(AFK)_

### Phase 6 — Testing & Validation

- **#47** — E2E test suite — full lifecycle at all 5 clearance levels  _(AFK)_
- **#48** — Security audit tests — escalation, injection, escape  _(HITL)_
- **#49** — Performance benchmarks — latency, throughput, memory  _(AFK)_
- **#50** — Data integrity validation — compliance guest as system check  _(AFK)_
- **#51** — "One Command" build validation — clone-to-running-system  _(AFK)_
- **#52** — Documentation — README, ARCHITECTURE, SECURITY, CONTRIBUTING  _(HITL)_
- **#53** — Deployment artifacts — static binary & optional Docker image  _(AFK)_