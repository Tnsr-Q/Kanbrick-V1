# Architecture

Kanbrick-V1 is a single Cargo workspace that compiles to one self-contained
binary. It has **no external runtime dependencies** (no Python, Node, or
Docker-compose): the graph database, the WASM runtime, and all business logic are
linked in.

## The four layers

```
                         ┌─────────────────────────────────────────────┐
   HTTP client ───────▶  │  kanbrick-api  (Axum)                        │
   (JWT bearer)          │  POST /login  /me  /admin  /health           │
                         │  POST /guests/{name}                         │
                         └───────────────┬─────────────────────────────┘
                                         │ FirmContext (host-authoritative)
            ┌────────────────────────────┼────────────────────────────┐
            ▼                            ▼                             ▼
   ┌─────────────────┐        ┌────────────────────┐        ┌───────────────────┐
   │ L1 — Guard      │        │ L2 — Nerves        │        │ L4 — Map          │
   │ kanbrick-auth   │        │ kanbrick-mesh      │        │ kanbrick-discovery│
   │ JWT, Argon2id,  │        │ wasmtime 45 runtime│        │ graphify-rs libs: │
   │ ClearanceScope, │◀──────▶│ guest registry,    │        │ org/portfolio     │
   │ GuardedStore,   │ guard  │ dispatch, EventBus │        │ analytics, scopes │
   │ AuditLog,       │        │ host imports:      │        └─────────┬─────────┘
   │ ApiKeyService   │        │  kbk_ctx/query/    │                  │
   └────────┬────────┘        │  emit/log          │                  │
            │                 └─────────┬──────────┘                  │
            │                           │ query_graph (clearance-filtered)
            ▼                           ▼                              ▼
   ┌──────────────────────────────────────────────────────────────────────────┐
   │ L3 — Brain :  kanbrick-store  →  SparrowDB (embedded, file-backed, Cypher) │
   │ Person · Company · Segment · FinancialSnapshot · AuditEntry · ApiKey       │
   └──────────────────────────────────────────────────────────────────────────┘

   Business logic runs as sandboxed WASM guests (wasm32-wasip1), embedded in the
   API binary and driven by the mesh:  valuation · reporting · compliance.
```

## Crate inventory

| Crate | Layer | Responsibility |
| --- | --- | --- |
| `kanbrick-core` | shared | `FirmContext`, `ClearanceLevel`, ids, errors, graph vocab, **host↔guest ABI** (`abi`) |
| `kanbrick-store` | L3 | SparrowDB lifecycle, typed schema, parameterized queries, migrations, seed loader |
| `kanbrick-auth` | L1 | JWT, Argon2id, `LoginService`, `require_clearance`, `ClearanceScope`, `GuardedStore`, `AuditLog`, `ApiKeyService` |
| `kanbrick-mesh` | L2 | wasmtime runtime, guest registry + dispatch, scheduler, `EventBus`, host imports |
| `kanbrick-discovery` | L4 | graphify-backed org/portfolio analytics, composable `VisibilityScope`, cache |
| `kanbrick-guest-sdk` | — | typed bindings to the host ABI for guests (`firm_context`/`query_graph`/`emit`/`log`) |
| `kanbrick-api` | API | HTTP surface; embeds + serves the guests |
| `kanbrick-cli` | tooling | `seed`, `set-password` |
| `guests/{valuation,reporting,compliance}` | guests | the three business modules (wasm) |

## Request data flow (`POST /guests/{name}`)

1. **Auth** — the `Authorization: Bearer` JWT is validated (`kanbrick-auth`) into a
   host-authoritative `FirmContext`. Identity is never read from the body.
2. **Gate** — the API checks the guest's minimum clearance (defense in depth; the
   guest also enforces its own). Insufficient → `403`.
3. **Audit** — the invocation is recorded (`AuditLog`).
4. **Dispatch** — the mesh instantiates the guest (`wasm32-wasip1`, locked-down
   WASIp1: no fs/net/stdio) on a blocking thread and injects the `FirmContext`
   through the read-only `kbk_ctx_*` imports.
5. **Query** — the guest's `query_graph` calls route through `GuardedStore`, which
   audits each query and **clearance-filters** every returned row before it
   crosses the WASM boundary.
6. **Respond / emit** — the guest returns a JSON `GuestResponse`; it may `emit`
   events onto the `EventBus` (e.g. `valuation.completed` → reporting).

## The host↔guest ABI

The boundary contract (`kanbrick_core::abi`, ADR-0002) is JSON over WASM linear
memory. Guests export `kbk_alloc`/`kbk_run`; the host publishes imports under the
`"kanbrick"` module: `kbk_ctx_len`/`kbk_ctx_read` (identity, #23),
`kbk_query_graph` (clearance-filtered reads, #24), `kbk_emit_event` (#27/#46),
`kbk_log`. The guest SDK wraps these in typed Rust (see `CONTRIBUTING.md`).

## Key decisions (ADRs)

- **ADR-0001** — SparrowDB Cypher dialect capabilities & workarounds.
- **ADR-0002** — Phase 3 WASM runtime: build on `wasmtime` directly; the minimal ABI.
- **ADR-0003** — Discovery: depend on graphify *library* sub-crates; analytics
  privileged, answers scoped.
- **ADR-0004** — Business guests: SDK shape, guest crate architecture, valuation
  financials + DCF parameters.
- **ADR-0005** — `PUBLIC_DATA`: company name/segment are a public roster.

See `docs/adr/` for the full text.
