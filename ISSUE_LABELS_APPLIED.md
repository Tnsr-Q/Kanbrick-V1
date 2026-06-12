# Kanbrick-V1 — Issue Labels Applied

All 51 issues (**#3–#53**) in [Tnsr-Q/Kanbrick-V1](https://github.com/Tnsr-Q/Kanbrick-V1) are now fully labeled (verified 2026-06-12, 0 unlabeled).

### Label scheme

Each issue carries: one **state** label, one **category** label, one **phase** label, one **type** label, plus **component** and **layer** labels where relevant.

| Label | Count | Meaning |
|---|---|---|
| `needs-triage` | 51 | State — awaiting maintainer evaluation |
| `enhancement` | 51 | Category — feature/improvement |
| `AFK` | 35 | Type — autonomous (agent-implementable) |
| `HITL` | 16 | Type — human-in-the-loop decision required |
| `phase-0` | 3 | Phase 0 — Repo setup & dependency resolution |
| `phase-1` | 7 | Phase 1 — Foundation: SparrowDB + schema |
| `phase-2` | 8 | Phase 2 — Identity & auth: Ironclaw + FirmContext |
| `phase-3` | 9 | Phase 3 — Orchestration: Tachyon-Mesh WASM runtime |
| `phase-4` | 9 | Phase 4 — Discovery: graphify-rs → SparrowDB |
| `phase-5` | 8 | Phase 5 — Business logic: WASM guests |
| `phase-6` | 7 | Phase 6 — Testing & validation |
| `sparrowdb` | 15 | Component — SparrowDB graph store (Brain) |
| `ironclaw` | 6 | Component — Ironclaw security/auth (Face) |
| `tachyon-mesh` | 11 | Component — Tachyon-Mesh WASM runtime (Nerves) |
| `graphify-rs` | 10 | Component — graphify-rs discovery (Map) |
| `wasm` | 15 | Component — WASM guests & host ABI |
| `security` | 16 | Security, clearance & auth concerns |
| `layer-1-face` | 5 | Layer 1 — Ironclaw (Face/Guard) |
| `layer-2-nerves` | 9 | Layer 2 — Tachyon-Mesh (Nerves/Muscle) |
| `layer-3-brain` | 7 | Layer 3 — SparrowDB (Brain) |
| `layer-4-map` | 9 | Layer 4 — graphify-rs (Map) |

### Per-issue labels


#### Phase 0 — Repository Setup & Dependency Resolution

- [**#3**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/3) Cargo workspace scaffold with empty member crates
  - `needs-triage`, `enhancement`, `phase-0`, `AFK`
- [**#4**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/4) Vendor the four upstream repos & resolve version conflicts
  - `needs-triage`, `enhancement`, `phase-0`, `HITL`, `sparrowdb`, `ironclaw`, `tachyon-mesh`, `graphify-rs`
- [**#5**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/5) CI pipeline — build, lint, and test gates
  - `needs-triage`, `enhancement`, `phase-0`, `AFK`

#### Phase 1 — Foundation: SparrowDB + Schema

- [**#6**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/6) SparrowDB embedded lifecycle — init, open, close
  - `needs-triage`, `enhancement`, `phase-1`, `AFK`, `sparrowdb`, `layer-3-brain`
- [**#7**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/7) ClearanceLevel enum & core shared types
  - `needs-triage`, `enhancement`, `phase-1`, `AFK`, `security`
- [**#8**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/8) Firm schema — Person, Company, Segment nodes & edges
  - `needs-triage`, `enhancement`, `phase-1`, `HITL`, `sparrowdb`, `layer-3-brain`
- [**#9**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/9) Cypher query executor — parameterized queries & deserialization
  - `needs-triage`, `enhancement`, `phase-1`, `AFK`, `sparrowdb`, `layer-3-brain`
- [**#10**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/10) Migration system — versioned schema & seed data
  - `needs-triage`, `enhancement`, `phase-1`, `AFK`, `sparrowdb`, `layer-3-brain`
- [**#11**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/11) Seed data loader CLI — kanbrick-cli seed
  - `needs-triage`, `enhancement`, `phase-1`, `AFK`, `sparrowdb`, `layer-3-brain`
- [**#12**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/12) Org chart hierarchy verification query
  - `needs-triage`, `enhancement`, `phase-1`, `AFK`, `sparrowdb`, `layer-3-brain`

#### Phase 2 — Identity & Auth: Ironclaw Security + FirmContext

- [**#13**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/13) FirmContext struct & construction from JWT claims
  - `needs-triage`, `enhancement`, `phase-2`, `AFK`, `ironclaw`, `security`, `layer-1-face`
- [**#14**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/14) Ironclaw JWT issuance & validation
  - `needs-triage`, `enhancement`, `phase-2`, `HITL`, `ironclaw`, `security`, `layer-1-face`
- [**#15**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/15) Login flow — email + password to JWT with clearance
  - `needs-triage`, `enhancement`, `phase-2`, `AFK`, `ironclaw`, `security`, `layer-1-face`
- [**#16**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/16) Clearance gate middleware — require_clearance(level)
  - `needs-triage`, `enhancement`, `phase-2`, `AFK`, `ironclaw`, `security`, `layer-1-face`
- [**#17**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/17) Post-query clearance filtering — filter_by_clearance
  - `needs-triage`, `enhancement`, `phase-2`, `HITL`, `security`, `sparrowdb`, `layer-1-face`
- [**#18**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/18) SparrowDB query interceptor — auto-inject clearance filters
  - `needs-triage`, `enhancement`, `phase-2`, `HITL`, `security`, `sparrowdb`, `layer-3-brain`
- [**#19**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/19) Audit log — every authenticated query logged
  - `needs-triage`, `enhancement`, `phase-2`, `AFK`, `security`, `sparrowdb`
- [**#20**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/20) API key management — service-to-service auth for WASM guests
  - `needs-triage`, `enhancement`, `phase-2`, `HITL`, `security`, `ironclaw`

#### Phase 3 — Orchestration: Tachyon-Mesh WASM Runtime

- [**#21**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/21) Tachyon-Mesh runtime init & echo WASM guest
  - `needs-triage`, `enhancement`, `phase-3`, `HITL`, `tachyon-mesh`, `wasm`, `layer-2-nerves`
- [**#22**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/22) Host-Guest ABI — HostFunctions & GuestModule traits
  - `needs-triage`, `enhancement`, `phase-3`, `HITL`, `tachyon-mesh`, `wasm`, `layer-2-nerves`
- [**#23**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/23) FirmContext passthrough — HTTP to mesh to guest
  - `needs-triage`, `enhancement`, `phase-3`, `AFK`, `tachyon-mesh`, `wasm`, `security`, `layer-2-nerves`
- [**#24**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/24) Guest query_graph host function — WASM to SparrowDB round-trip
  - `needs-triage`, `enhancement`, `phase-3`, `AFK`, `tachyon-mesh`, `wasm`, `sparrowdb`, `layer-2-nerves`
- [**#25**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/25) Task scheduler — immediate execution with timeout enforcement
  - `needs-triage`, `enhancement`, `phase-3`, `AFK`, `tachyon-mesh`, `layer-2-nerves`
- [**#26**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/26) Task scheduler — scheduled (cron-like) & event-triggered execution
  - `needs-triage`, `enhancement`, `phase-3`, `AFK`, `tachyon-mesh`, `layer-2-nerves`
- [**#27**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/27) Inter-module event bus — typed events & subscriptions
  - `needs-triage`, `enhancement`, `phase-3`, `AFK`, `tachyon-mesh`, `layer-2-nerves`
- [**#28**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/28) WASM memory & resource enforcement
  - `needs-triage`, `enhancement`, `phase-3`, `AFK`, `wasm`, `tachyon-mesh`, `security`, `layer-2-nerves`
- [**#29**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/29) WASM module hot-reload
  - `needs-triage`, `enhancement`, `phase-3`, `AFK`, `wasm`, `tachyon-mesh`, `layer-2-nerves`

#### Phase 4 — Discovery: graphify-rs → SparrowDB Integration

- [**#30**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/30) graphify-rs to SparrowDB adapter layer
  - `needs-triage`, `enhancement`, `phase-4`, `HITL`, `graphify-rs`, `sparrowdb`, `layer-4-map`
- [**#31**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/31) reporting_path — shortest path between two persons
  - `needs-triage`, `enhancement`, `phase-4`, `AFK`, `graphify-rs`, `layer-4-map`
- [**#32**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/32) span_of_control — direct + indirect report counts
  - `needs-triage`, `enhancement`, `phase-4`, `AFK`, `graphify-rs`, `layer-4-map`
- [**#33**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/33) company_stakeholders — all persons managing a company
  - `needs-triage`, `enhancement`, `phase-4`, `AFK`, `graphify-rs`, `layer-4-map`
- [**#34**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/34) segment_overview & cross_segment_links
  - `needs-triage`, `enhancement`, `phase-4`, `AFK`, `graphify-rs`, `layer-4-map`
- [**#35**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/35) org_neighborhood & common_manager
  - `needs-triage`, `enhancement`, `phase-4`, `AFK`, `graphify-rs`, `layer-4-map`
- [**#36**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/36) Clearance-aware discovery — filtered graph traversal
  - `needs-triage`, `enhancement`, `phase-4`, `HITL`, `graphify-rs`, `security`, `layer-4-map`
- [**#37**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/37) Discovery caching layer with TTL & invalidation
  - `needs-triage`, `enhancement`, `phase-4`, `AFK`, `graphify-rs`, `layer-4-map`
- [**#38**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/38) Code graph ingest — graphify-rs cypher.txt into SparrowDB
  - `needs-triage`, `enhancement`, `phase-4`, `HITL`, `graphify-rs`, `sparrowdb`, `layer-4-map`

#### Phase 5 — Business Logic: WASM Guests

- [**#39**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/39) kanbrick-guest-sdk — typed bindings to the host ABI
  - `needs-triage`, `enhancement`, `phase-5`, `AFK`, `wasm`
- [**#40**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/40) WASM guest build toolchain validation
  - `needs-triage`, `enhancement`, `phase-5`, `AFK`, `wasm`
- [**#41**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/41) Compliance guest — org chart integrity check
  - `needs-triage`, `enhancement`, `phase-5`, `AFK`, `wasm`, `sparrowdb`
- [**#42**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/42) Compliance guest — clearance consistency validation
  - `needs-triage`, `enhancement`, `phase-5`, `AFK`, `wasm`, `security`
- [**#43**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/43) Reporting guest — portfolio dashboard (L5 full view)
  - `needs-triage`, `enhancement`, `phase-5`, `HITL`, `wasm`
- [**#44**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/44) Reporting guest — clearance-tiered output (L1–L4 filtered views)
  - `needs-triage`, `enhancement`, `phase-5`, `HITL`, `wasm`, `security`
- [**#45**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/45) Valuation guest — DCF model for portfolio companies
  - `needs-triage`, `enhancement`, `phase-5`, `HITL`, `wasm`
- [**#46**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/46) Cross-guest event flow — valuation triggers reporting
  - `needs-triage`, `enhancement`, `phase-5`, `AFK`, `wasm`, `tachyon-mesh`

#### Phase 6 — Testing & Validation

- [**#47**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/47) E2E test suite — full lifecycle at all 5 clearance levels
  - `needs-triage`, `enhancement`, `phase-6`, `AFK`, `security`
- [**#48**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/48) Security audit tests — escalation, injection, escape
  - `needs-triage`, `enhancement`, `phase-6`, `HITL`, `security`
- [**#49**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/49) Performance benchmarks — latency, throughput, memory
  - `needs-triage`, `enhancement`, `phase-6`, `AFK`
- [**#50**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/50) Data integrity validation — compliance guest as system check
  - `needs-triage`, `enhancement`, `phase-6`, `AFK`, `wasm`, `sparrowdb`
- [**#51**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/51) "One Command" build validation — clone-to-running-system
  - `needs-triage`, `enhancement`, `phase-6`, `AFK`
- [**#52**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/52) Documentation — README, ARCHITECTURE, SECURITY, CONTRIBUTING
  - `needs-triage`, `enhancement`, `phase-6`, `HITL`
- [**#53**](https://github.com/Tnsr-Q/Kanbrick-V1/issues/53) Deployment artifacts — static binary & optional Docker image
  - `needs-triage`, `enhancement`, `phase-6`, `AFK`
