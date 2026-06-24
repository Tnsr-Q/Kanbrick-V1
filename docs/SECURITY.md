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

## Guest registry (#64)

Guests can be added or replaced at runtime from a content-addressed, air-gapped
asset store (`tachyon://sha256:<hex>`). This is a privileged trust surface, so it
is constrained on three axes:

- **L5 only.** Both `POST /admin/assets/guests` (upload) and
  `POST /admin/guests/{name}/activate` (bind + hot-reload) require admin (L5)
  clearance. Lower tiers get a `403`.
- **Integrity.** Artifacts are addressed by SHA-256 and the digest is verified on
  write *and* re-verified on every read, so a corrupted or swapped file is caught
  before it is ever compiled. Activation is compile-first and atomic: if the
  artifact fails to compile, the previously-active guest keeps serving and **no**
  policy is written.
- **Clearance floor.** A registry activation may *raise* a guest's minimum
  clearance but never set it below the embedded baseline (e.g. `compliance` can
  never drop below L4). New guest names must specify a minimum clearance.

Every upload and activation is recorded through the same `AuditLog` as guest
queries. The policy that binds a name to a version/clearance/asset URI is stored
in SparrowDB (the source of truth) and replayed at boot; the asset *bytes* live
on the asset volume, which must be on durable, single-pod (control-plane) storage
(see `deploy/k8s/scale-out.md`).

## Control-plane / executor split (#70/#71, ADR-0008)

When scaled out, WASM execution moves onto a stateless **executor** pool while the
single **control plane** (CP) keeps the graph, the asset registry, every write,
and the authoritative identity. This preserves the host-authoritative-identity
invariant **across the network hop**:

- **Capability tokens.** Per invocation, the CP mints a short-lived, single-use,
  unguessable **capability** (two v4 UUIDs ⇒ 244 bits of entropy) bound
  server-side to the caller's `FirmContext`. It forwards the invocation to an
  executor relaying **only** the opaque capability — never the identity. When a
  guest calls back to read the graph or emit an event, the executor presents the
  capability; the CP resolves it to the bound identity **server-side** and runs
  the read through the same clearance-enforcing `GuardedStore`. A compromised
  executor (or a WASM escape) therefore cannot name a different identity or read
  above the clearance the CP bound to the invocation. A forged or expired
  capability is a `401` ⇒ the guest's query traps (no data leak). The capability
  is revoked the moment the invocation returns. The `FirmContext` bytes never
  leave the CP process; the `ctx` sent to the executor is read-only state for the
  guest's `kbk_ctx_*` imports and is never trusted on a callback.
- **Transport secret.** Both the CP's internal RPC surface and the executor's
  `/internal/invoke` are gated by a shared secret (`x-kanbrick-internal-token`),
  compared in **constant time** and **failing closed** when unset. It is supplied
  via the `internal-token` Secret key, never logged.
- **Network confinement.** The internal RPC surface and the executor are
  **ClusterIP-only**; `deploy/k8s/networkpolicy.yaml` confines the internal port
  to CP↔executor and keeps the executor off any public ingress. The executor is
  **not internet-facing**: no public surface, no store, no JWT.
- **In-cluster transport.** CP↔executor traffic is plain HTTP between ClusterIP
  Services (never through the public ingress). As with the public API, terminate
  TLS at the mesh/proxy layer if your threat model requires in-cluster
  confidentiality.

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
| Malicious/forged guest artifact | L5-only registry, SHA-256 verify on write+read, compile-first atomic swap | `kanbrick-api/tests/registry.rs` |
| Clearance downgrade via activation | embedded clearance floor enforced | `registry.rs` |
| Identity forgery over the network (executor→CP) | host-authoritative capability; `ctx` never trusted on a callback | `kanbrick-api/tests/executor.rs` |
| Compromised executor reading above clearance | cap resolves to the CP-bound clearance server-side; forged/expired cap ⇒ `401` ⇒ trap | `executor.rs` |
| Unauthorized access to the internal RPC surface | shared transport secret (constant-time, fails closed); ClusterIP-only + NetworkPolicy | `internal.rs`, `executor.rs` |

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
  through the public ingress. The **executor** pool serves the same `/metrics`
  (it is the KEDA scale signal, #71) under the same in-cluster-only treatment, and
  is confined off any public ingress by `deploy/k8s/networkpolicy.yaml`.
