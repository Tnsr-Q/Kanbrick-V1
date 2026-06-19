# Security

Security is a spine that runs through every layer: identity is established once,
propagated host-authoritatively, and enforced at a single audited choke point.

## Clearance model (five tiers)

`ClearanceLevel` is `L1 < L2 < L3 < L4 < L5` (`kanbrick_core`). Data visibility is
resolved per caller by `kanbrick_auth::ClearanceScope`:

| Tier | Label | Sees |
| --- | --- | --- |
| **L5** | Admin | everything, unfiltered |
| **L4** | Strategic | every person and company, all fields |
| **L3** | Operational | own segment's companies (those they manage) + own direct/indirect reports + self |
| **L2** | Execution | assigned companies (those they manage, if any) + own record |
| **L1** | Support | own record + the **public company roster** |

Sensitive person fields (e.g. compensation) are additionally gated: a caller may
read them only for persons at or below their own clearance.

### PUBLIC_DATA (ADR-0005)

Company **identity is public**: the fields `company_id`, `name`, and `segment`
form a roster readable by **every** tier. Everything else — other company fields,
all personnel data, and all financials (`FinancialSnapshot`) — stays
clearance-gated. This is enforced uniformly in `ClearanceScope::retain_rows`: a
row projecting only public fields is always kept; a row with any non-public field
is gated by `can_see_company`; a sensitive projection exposing no clearance key is
**denied** (fail-closed).

## Authentication

- **Login** (`POST /login`): email + password → JWT. Passwords are hashed with
  **Argon2id** and stored on the `Person` node (never in plaintext).
- **JWT**: HS256 over a `Claims` payload mirroring the `FirmContext`. Issuance
  stamps `iat`/`exp` from a configurable TTL; validation checks the signature and
  expiry with **zero clock leeway**. Tampered, malformed, or expired tokens are
  rejected as `401` — never a panic.
- **Service identities** (`ApiKeyService`): scoped, rotatable API keys for
  service-to-service auth, bound to a fixed clearance, stored as a SHA-256 hash.

### Host-authoritative identity

A guest learns its caller's identity **only** through the read-only `kbk_ctx_*`
host imports (#23). Nothing in a request body — HTTP or guest payload — can set or
forge the `FirmContext`; there is no import to *set* it. The API never copies
identity from the body into the guest call.

## The audited, clearance-filtering choke point

Every authenticated read goes through `GuardedStore` (#18/#24):

1. the query text is **audited** (`AuditLog` — an `(:AuditEntry)` with `user_id`,
   `clearance`, a SHA-256 `query_hash`, and a timestamp; raw query bodies are
   never stored);
2. parameters are **bound, never interpolated** (Cypher-injection safe);
3. every returned row is **clearance-filtered** before it crosses any boundary.

Because the guest's `query_graph` routes through this, a guest can never see data
above its caller's clearance, and a guest cannot dodge filtering by projecting raw
columns (fail-closed).

## Sandbox

Guests run as `wasm32-wasip1` modules under wasmtime with a **locked-down** WASI
context: no preopened filesystem, no network, no inherited stdio. Per-dispatch
limits (ADR-0002): 64 MiB max linear memory, a fuel budget, and a wall-clock
timeout (epoch interruption). A guest cannot reach host memory or the host
filesystem.

## Threat model & test coverage

| Vector | Mitigation | Tested by |
| --- | --- | --- |
| Clearance escalation (forged claims) | signature check; identity host-authoritative | `kanbrick-api/tests/security.rs` |
| Privilege via a valid low token | per-guest + per-route clearance gates | `security.rs`, `e2e.rs` |
| Expired / tampered / malformed JWT | strict validation, no leeway | `security.rs`, `jwt` unit tests |
| Cypher injection | bound parameters | `security.rs`, `guarded` tests |
| Data leakage across tiers | `ClearanceScope` + fail-closed `retain_rows` | `scope`/`guarded`/discovery tests |
| Guest sandbox escape | WASIp1 lockdown + resource limits | `mesh/tests/resource_limits.rs` |
| Audit completeness | every guarded query audited | `guarded`, `data_integrity` tests |

## Known limitations (for the security review — #48 is HITL)

- **Token replay.** JWTs are stateless bearer tokens: a captured token is
  replayable until it expires. This is standard bearer-token behaviour, mitigated
  by a **short TTL** and TLS in transport. Server-side revocation / one-time-use
  (a `jti` deny-list, or session tracking) is a deliberate future addition, not
  yet implemented.
- **Dev JWT secret.** `kanbrick-api` falls back to an insecure dev secret if
  `KANBRICK_JWT_SECRET` is unset (and logs a warning). Always set it in any real
  deployment.
- **Transport.** The API speaks plain HTTP; terminate TLS at a reverse proxy (or
  add TLS) before exposing it.
- **Metrics exposure (#63).** `GET /metrics` is **unauthenticated** so Prometheus
  can scrape it, and it carries no identities, tokens, or business data — only
  per-guest invocation counters and `kanbrick_mesh_pressure_ratio`. It does,
  however, expose the **guest catalogue** through the `guest="…"` label (e.g.
  `valuation`, `compliance`). Treat it as an **in-cluster scrape surface only**:
  bind it to the internal `Service`/`ServiceMonitor` and never route `/metrics`
  through the public ingress.
