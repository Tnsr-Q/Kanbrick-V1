# Kanbrick-V1

Kanbrick-V1 is a self-contained **Firm Operating System** for a modeled investment firm: identity and clearance, an embedded graph database, graph analytics, sandboxed WASM business logic, operator tooling, and an agentic desktop all live in one repository and one primary Rust workspace.

At its core, the system is the path:

**HTTP → Auth → Mesh → Guest → Graph**

A request enters the Axum API, is authenticated into a host-authoritative `FirmContext`, is dispatched into a sandboxed WASM guest under that identity, and can read the graph only through audited, clearance-filtered host imports.

The repo has grown well beyond the original Phase 0–6 spine. In addition to the four original layers, it now includes:

- a **control-plane / executor split** for scale-out guest execution,
- a **content-addressed guest registry** with hot-reload,
- a full **Tauri v2 desktop Cockpit**,
- **provider-key custody** and provider runtime abstractions,
- **scope grants**, **skill publishing/review**, and **loop execution** surfaces,
- internal RPC and sidecar/component registration,
- deployment and CI flows for both the API spine and the Cockpit.

If you are new to the repo, start with:

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
- [`docs/SECURITY.md`](docs/SECURITY.md)
- [`CONTRIBUTING.md`](CONTRIBUTING.md)
- [`cockpit/README.md`](cockpit/README.md)
- [`docs/adr/`](docs/adr/)

---

## Table of contents

1. [What this system is](#what-this-system-is)
2. [High-level architecture](#high-level-architecture)
3. [Repository map](#repository-map)
4. [Core crates and their roles](#core-crates-and-their-roles)
5. [Runtime model](#runtime-model)
6. [Security model](#security-model)
7. [HTTP API surface](#http-api-surface)
8. [Cockpit desktop app](#cockpit-desktop-app)
9. [Getting started](#getting-started)
10. [Development workflow](#development-workflow)
11. [Guest development model](#guest-development-model)
12. [Deployment model](#deployment-model)
13. [Testing and CI](#testing-and-ci)
14. [Current feature status](#current-feature-status)
15. [Suggested reading paths](#suggested-reading-paths)

---

## What this system is

Kanbrick-V1 models a 12-person investment firm and its 9-company portfolio as a graph. It enforces a five-tier clearance model end to end and runs business logic as sandboxed WASM guests.

The repo is opinionated in several important ways:

- **Embedded data store**: the graph lives in file-backed SparrowDB, not an external server.
- **Host-authoritative identity**: identity is established once and never trusted from request bodies or guest payloads.
- **Sandboxed business logic**: business modules run as `wasm32-wasip1` guests under Wasmtime.
- **Audited read path**: authenticated graph reads go through a single audited, clearance-filtering choke point.
- **Single primary workspace**: the core system is one Rust workspace; large optional surfaces such as the Cockpit are isolated into their own build graphs.
- **Composable future layers**: provider integrations, skills, loops, grants, egress, tokens, and desktop workflows are layered on top of the original L1–L4 spine rather than replacing it.

---

## High-level architecture

### The original four layers

| Layer | Crate | Technology | Responsibility |
| --- | --- | --- | --- |
| **L1 — Guard** | `kanbrick-auth` | JWT, Argon2id | identity, password verification, clearance enforcement, audit |
| **L2 — Nerves** | `kanbrick-mesh` | Wasmtime 45, WASIp1 | guest runtime, dispatch, scheduler, host imports, event bus |
| **L3 — Brain** | `kanbrick-store` | SparrowDB | embedded firm graph, migrations, policy/state registries |
| **L4 — Map** | `kanbrick-discovery` | graphify-rs libs | graph analytics, org/portfolio analysis, scoped discovery |

### The API and runtime spine

The crate `kanbrick-api` assembles these layers into one canonical runtime:

- validates JWTs into a `FirmContext`,
- routes requests,
- enforces route- and guest-level clearance,
- audits invocations,
- dispatches WASM guests,
- exposes metrics and health endpoints,
- manages runtime guest activation from content-addressed assets,
- optionally forwards guest execution to executor nodes,
- hosts later L5 surfaces like provider keys, skills, loops, grants, and component registration.

### The L5 desktop and orchestration surfaces

The repo now includes an L5 desktop experience, the **Cockpit**, implemented in `cockpit/` as a Tauri v2 app with a React/Vite frontend and a Rust host process. The Cockpit bundles `kanbrick-api` as a sidecar and remains a client of the same end-to-end path rather than re-implementing backend logic.

### Business guests

Three core business guests are embedded into the API binary at build time:

- `valuation` — minimum clearance **L3**
- `compliance` — minimum clearance **L4**
- `reporting` — minimum clearance **L1**

These guests compile to `wasm32-wasip1` and run under a locked-down WASI configuration.

---

## Repository map

```text
.
├── kanbrick-api/           # Axum HTTP API, executor split, admin/runtime surfaces
├── kanbrick-auth/          # JWT auth, login, password hashing, audit, guarded queries
├── kanbrick-core/          # Shared types, errors, firm context, clearance, ABI
├── kanbrick-store/         # SparrowDB wrapper, migrations, registries, persistence glue
├── kanbrick-mesh/          # Wasmtime runtime, scheduler, event bus, asset store, host imports
├── kanbrick-discovery/     # Clearance-aware graph analytics over the firm graph
├── kanbrick-cli/           # Seed, set-password, optional code-graph ingest
├── kanbrick-guest-sdk/     # Typed guest-side bindings to the host ABI
├── kanbrick-providers/     # Provider abstraction layer for BYO-AI/provider steps
├── kanbrick-tokens/        # Token ledger / budgeting layer
├── kanbrick-egress/        # Egress gate / DLP / RBAC layer
├── kanbrick-loops/         # Skill/loop domain model, currently SKILL.md manifest parsing
├── guests/                 # Business guests and reference/test guests
├── cockpit/                # Tauri v2 desktop app (separate build graph)
├── crates/                 # Vendored upstream submodules
├── deploy/                 # Deployment artifacts and operational manifests
├── docs/                   # Architecture, security, ADRs, handoffs, probes, benchmarks
├── seed/                   # Seed data loaded into the embedded graph
├── scripts/                # Smoke tests, guest builds, Docker release automation
├── tests/                  # Integration / e2e / security tests
├── probes/                 # Standalone de-risk experiments outside the main workspace
├── schema/                 # Schema-related assets
├── graph/                  # Default on-disk DB location / graph artifacts
└── .out-of-scope/          # Wontfix knowledge base
```

Notes:

- `cockpit/src-tauri` is deliberately **excluded** from the root Cargo workspace.
- The vendored upstreams under `crates/` are also excluded from the main workspace graph.
- Only `crates/sparrowdb` is required for the primary build.

---

## Core crates and their roles

### `kanbrick-core`

Shared domain vocabulary and cross-layer contracts:

- `FirmContext`
- `ClearanceLevel`
- IDs and schema vocabulary
- status and error types
- the host↔guest ABI (`kanbrick_core::abi`)

`FirmContext` is the security identity propagated through the system. It includes the caller’s `user_id`, `email`, `clearance`, `roles`, `session_id`, issue timestamp, and firm ID.

### `kanbrick-store`

Thin lifecycle and query wrapper around file-backed SparrowDB:

- open/close/checkpoint the DB,
- execute parameterized Cypher queries,
- run migrations,
- load seed data,
- persist guest policy state,
- persist registries for later features (skills, loops, messenger, etc.).

The store is shared behind `Arc` and follows SparrowDB’s single-writer / multi-reader model.

### `kanbrick-auth`

Owns the identity and authorization story:

- password hashing with **Argon2id**,
- JWT issue/validate,
- login flow,
- clearance scoping,
- `GuardedStore`, the audited clearance-enforcing query interceptor,
- audit logging,
- API key service identities.

The key invariant is that callers never directly shape their authority: authenticated graph reads are always mediated through host-owned logic.

### `kanbrick-mesh`

The in-process WASM orchestration runtime:

- guest registry,
- guest invocation,
- hot-reload support,
- event bus,
- scheduler,
- per-guest admission/pressure metrics,
- content-addressed asset support,
- host imports for context, query, emit, and log,
- optional `HostServices` indirection for the control-plane/executor split.

The runtime is built directly on **Wasmtime 45** and **WASIp1**, not on the vendored Tachyon-Mesh host stack.

### `kanbrick-discovery`

Loads the firm graph into graphify-backed structures and exposes graph analytics over it:

- org-chart traversal and reporting paths,
- span-of-control and neighborhood analysis,
- portfolio and stakeholder views,
- influence ranking,
- scope/grant-related graph reasoning,
- optional code-graph ingest support.

Analytics are privileged, but answers are filtered back to the caller’s visibility scope.

### `kanbrick-api`

The main service runtime. It now does substantially more than the original README described. Beyond login and guest dispatch, it owns:

- health and Prometheus metrics,
- guest admission control,
- registry asset upload and activation,
- control-plane / executor wiring,
- internal RPC routing,
- component registration,
- provider-key custody endpoints,
- messenger endpoints,
- scope request / approval / revocation endpoints,
- skill publish / browse / review / bind endpoints,
- loop creation / listing / run / history endpoints.

### `kanbrick-cli`

Operational and data-loading CLI:

- `seed`
- `set-password`
- optional `code-ingest` when the `codegraph` feature is enabled.

### `kanbrick-guest-sdk`

The guest author’s typed interface to the host ABI. Guests use this to:

- read the host-authoritative `FirmContext`,
- perform graph queries,
- emit events,
- log back to the host.

### `kanbrick-loops`

Defines the initial domain model for the skill/loop ecosystem. Today it primarily implements parsing and rendering of `SKILL.md` manifests:

- frontmatter parsing,
- clearance vocabulary,
- guest binding,
- versioning,
- body/instruction storage.

Persistence of those manifests as graph entities lives elsewhere (`kanbrick-store` / `kanbrick-api`).

### `kanbrick-providers`, `kanbrick-tokens`, `kanbrick-egress`

These crates represent later L5 platform layers for provider execution, cost/token accounting, and egress-policy enforcement. They are already present in the workspace and surfaced in API/Cockpit code paths, even where some capabilities are still staged behind later-phase implementation work.

---

## Runtime model

## Single-node mode

In the simplest deployment, `kanbrick-api` runs everything in one process:

- embedded SparrowDB store,
- JWT auth,
- in-process mesh runtime,
- embedded guests,
- event bus,
- admin/registry surfaces.

This is the easiest local development path and remains the default mental model.

## Control-plane / executor mode

The runtime now also supports a split mode for scale-out execution.

### Control plane

The control plane:

- serves the public API,
- owns the graph database,
- owns the authoritative identity model,
- persists guest policies,
- mints per-invocation capabilities,
- optionally hosts the internal RPC surface,
- can forward guest execution to executors.

### Executor

The executor:

- does **not** own the graph,
- does **not** validate public JWTs,
- does **not** expose the public API,
- runs guest WASM in a stateless pool,
- proxies guest graph/event callbacks back to the control plane.

This preserves the invariant that identity remains host-authoritative even across the network hop.

## Guest runtime behavior

Each guest invocation:

1. gets a fresh sandboxed Wasmtime store,
2. sees identity only via read-only host imports,
3. can query the graph only via a host import that routes into clearance-filtered services,
4. may emit events onto the event bus,
5. runs under limits for memory, fuel, and timeout.

Default sandbox limits are:

- **64 MiB** max linear memory
- **1,000,000,000** fuel units
- **5 second** wall-clock timeout budget

## Embedded guests vs registry-activated guests

The system boots with embedded guests registered in the mesh. On top of that, it can replay registry-backed guest overrides from the asset store. This means:

- the binary provides a known-good baseline,
- operators can upload new artifacts,
- the system can hot-reload named guests to content-addressed versions,
- embedded guests still define the minimum clearance floor that a registry override may never go below.

---

## Security model

Security is a central design concern, not a bolt-on. The best detailed reference remains [`docs/SECURITY.md`](docs/SECURITY.md), but the major invariants are summarized here.

### Five-tier clearance model

The system uses five clearance levels:

- **L1** — Support
- **L2** — Execution
- **L3** — Operational
- **L4** — Strategic
- **L5** — Admin

Clearance determines what rows, routes, guests, and privileged control surfaces the caller may access.

### Host-authoritative identity

Identity is established by the auth layer and becomes a `FirmContext`. That identity:

- is serialized into JWT claims,
- is rehydrated by the API,
- is injected into guests by the host,
- is never trusted from a request body,
- is never accepted from the webview in Cockpit,
- is never chosen by the guest.

### Audited, clearance-filtered read path

Every authenticated read is intended to pass through `GuardedStore`:

- the query is audited,
- parameters are bound, never interpolated,
- returned rows are filtered based on the caller’s scope,
- projections that cannot be proven safe fail closed for non-see-all callers.

### Public vs non-public data

A specific `PUBLIC_DATA` rule exists for company identity:

- `company_id`
- `name`
- `segment`

These can be visible across all tiers as the public company roster. Other fields remain clearance-gated.

### Passwords and tokens

- passwords are hashed with **Argon2id**,
- JWTs use **HS256**,
- token expiry is enforced with zero clock leeway,
- invalid/expired/malformed tokens return `401` rather than crashing,
- service-to-service auth is supported via scoped API keys.

### Guest sandboxing

Guests run with:

- no preopened filesystem,
- no inherited stdio,
- no direct network access,
- bounded memory and fuel,
- timeout support.

### Registry and hot-reload protections

Runtime guest replacement is constrained by:

- **L5-only** upload and activation,
- **SHA-256** content-addressing and verification,
- compile-first atomic swap,
- clearance floors for embedded guest names.

### Internal RPC protections

The control-plane/executor boundary is protected by:

- a shared internal transport secret,
- fail-closed behavior when the secret is absent,
- cluster-internal networking assumptions,
- capability-bound callback resolution on the control plane.

---

## HTTP API surface

The API surface is much broader than the original README listed.

## Core public endpoints

| Route | Auth | Purpose |
| --- | --- | --- |
| `POST /login` | none | email + password → JWT |
| `GET /me` | JWT | caller identity |
| `GET /admin` | JWT, L4+ | example clearance-gated route |
| `GET /health` | none | liveness, guest count, version |
| `GET /metrics` | none | Prometheus mesh/admission metrics |
| `POST /guests/{name}` | JWT | invoke a guest under caller context |

## Guest registry/admin endpoints

| Route | Auth | Purpose |
| --- | --- | --- |
| `POST /admin/assets/guests` | JWT, L5 | upload a guest WASM artifact into the content-addressed asset store |
| `POST /admin/guests/{name}/activate` | JWT, L5 | activate a named guest from a stored artifact |

## Provider-key endpoints

| Route | Auth | Purpose |
| --- | --- | --- |
| `POST /me/provider-keys` | JWT | create/store a provider key for the caller |
| `GET /me/provider-keys` | JWT | list provider-key metadata for the caller |
| `DELETE /me/provider-keys/{id}` | JWT | delete a stored provider key |

## Messenger endpoints

| Route | Auth | Purpose |
| --- | --- | --- |
| `POST /me/messenger/send` | JWT | send/store a messenger message |
| `GET /me/messenger/log` | JWT | read message log/history |

## Component/sidecar endpoints

| Route | Auth | Purpose |
| --- | --- | --- |
| `GET /me/components` | JWT | list registered components surfaced to the visualizer/Cockpit |

## Scope grant endpoints

| Route | Auth | Purpose |
| --- | --- | --- |
| `POST /me/scope-requests` | JWT | create a scope request |
| `GET /me/scope-requests/{id}` | JWT | read a scope request |
| `POST /me/scope-requests/{id}/approve` | JWT | approve a scope request |
| `POST /me/scope-requests/{id}/deny` | JWT | deny a scope request |
| `GET /me/scopes` | JWT | list active scopes |
| `POST /me/scopes/{id}/revoke` | JWT | revoke a scope |

## Skill endpoints

| Route | Auth | Purpose |
| --- | --- | --- |
| `POST /me/scopes/{id}/skills` | JWT | bind a skill to a scope |
| `GET /me/scopes/{id}/skills` | JWT | list skills bound to a scope |
| `POST /me/skills` | JWT | publish a skill |
| `GET /me/skills` | JWT | browse published skills |
| `GET /me/skills/{name}` | JWT | view skill history |
| `GET /me/skill-reviews` | JWT | list skill reviews |
| `POST /me/skill-reviews/{name}/{version}` | JWT | review a skill version |

## Loop endpoints

| Route | Auth | Purpose |
| --- | --- | --- |
| `POST /me/loops` | JWT | create a loop |
| `GET /me/loops` | JWT | list loops |
| `GET /me/loops/{id}` | JWT | read loop definition |
| `POST /me/loops/{id}/run` | JWT | run a loop |
| `GET /me/loops/runs/{id}` | JWT | inspect loop run status/history |

## Example local run

```bash
git clone <repo> && cd Kanbrick-V1
git submodule update --init --depth 1 crates/sparrowdb
cargo build --release --workspace

cargo run --release -p kanbrick-cli -- seed --db graph/firm.db
cargo run --release -p kanbrick-cli -- set-password \
  --email tracy.brittcool@kanbrick.com \
  --password secret \
  --db graph/firm.db

KANBRICK_JWT_SECRET=change-me \
  cargo run --release -p kanbrick-api -- --port 8080 --db graph/firm.db

curl localhost:8080/health
TOKEN=$(curl -s localhost:8080/login -H 'content-type: application/json' \
  -d '{"email":"tracy.brittcool@kanbrick.com","password":"secret"}' | jq -r .token)

curl -s localhost:8080/me -H "Authorization: Bearer $TOKEN"
curl -s localhost:8080/guests/reporting -H "Authorization: Bearer $TOKEN" -d '{}'
```

---

## Cockpit desktop app

The `cockpit/` directory is an important part of the repository now and should be thought of as the L5 operator environment.

It is a **Tauri v2** application with:

- a React/Vite webview frontend in `cockpit/src/`,
- a Rust host in `cockpit/src-tauri/`,
- a managed `kanbrick-api` sidecar,
- host-side JWT custody,
- typed IPC wrappers,
- UI surfaces for identity, loops, providers, messenger, skills, whiteboard/visualizer-related workflows, and more.

The core security rule in the Cockpit is the same as in the backend: **identity remains host-authoritative**.

### Key Cockpit properties

- The JWT never has to live in browser storage.
- The host process can outlive a webview reload.
- The sidecar API is health-gated and supervised.
- The desktop app is intentionally **outside** the main workspace graph.

### Run the Cockpit

```bash
cd cockpit
npm install
npm run sidecar
npm run tauri dev
```

Read [`cockpit/README.md`](cockpit/README.md) for the detailed desktop-specific architecture and CI workflow.

---

## Getting started

## Prerequisites

### Required for the main workspace

- **Rust 1.94.1** (pinned by `rust-toolchain.toml`)
- targets/components from the pinned toolchain:
  - `wasm32-wasip1`
  - `rustfmt`
  - `clippy`
- Git submodule support

### Required submodule

Initialize at least SparrowDB:

```bash
git submodule update --init --depth 1 crates/sparrowdb
```

Other vendored submodules are initialized best-effort in CI and are excluded from the current core workspace graph.

### Optional for Cockpit

- **Node 18+** / npm
- Tauri v2 system dependencies for your OS

## Fresh-clone build

```bash
git clone <repo> && cd Kanbrick-V1
git submodule update --init --depth 1 crates/sparrowdb
cargo build --release --workspace
```

## Seed the graph and create a password

```bash
cargo run --release -p kanbrick-cli -- seed --db graph/firm.db
cargo run --release -p kanbrick-cli -- set-password \
  --email tracy.brittcool@kanbrick.com \
  --password secret \
  --db graph/firm.db
```

## Run the API

```bash
KANBRICK_JWT_SECRET=change-me \
  cargo run --release -p kanbrick-api -- --port 8080 --db graph/firm.db
```

## Validate the path

```bash
curl localhost:8080/health

TOKEN=$(curl -s localhost:8080/login -H 'content-type: application/json' \
  -d '{"email":"tracy.brittcool@kanbrick.com","password":"secret"}' | jq -r .token)

curl -s localhost:8080/me -H "Authorization: Bearer $TOKEN"
curl -s localhost:8080/guests/reporting -H "Authorization: Bearer $TOKEN" -d '{}'
```

---

## Development workflow

The documented local gate is:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --all-features
cargo test --workspace --all-features
scripts/build-guests.sh
```

### One-command smoke path

The end-to-end clone-to-running-system path is validated by:

```bash
scripts/smoke.sh
```

This script:

- ensures the SparrowDB submodule is present,
- builds the workspace in release mode,
- checks the `kanbrick-api` binary size budget,
- seeds a fresh DB,
- provisions a password,
- boots the API,
- waits for `/health`,
- logs in,
- calls the reporting guest,
- verifies the expected company count.

### Guest build validation

```bash
scripts/build-guests.sh
```

This compiles the business guests to `wasm32-wasip1` and enforces a **10 MiB** artifact budget for each guest.

### Docker release path

```bash
scripts/docker-release.sh
```

This script builds the container image, reports image size, then smoke-tests it by seeding and serving inside the container.

---

## Guest development model

Guests are small Rust crates that generally:

- compile to both `rlib` and `cdylib`,
- keep pure logic testable natively,
- use `kanbrick-guest-sdk` for all host interaction,
- read identity from `sdk::firm_context()`,
- query through `sdk::query_graph()`,
- optionally emit events with `sdk::emit(...)`.

The canonical guidance for writing guests is in [`CONTRIBUTING.md`](CONTRIBUTING.md).

### Existing guests

- `guests/valuation`
- `guests/reporting`
- `guests/compliance`
- `guests/sdk-example`
- `guests/echo`

### Important invariants for guest authors

- Never accept caller identity in the request payload.
- Enforce guest-local minimum clearance as defense in depth.
- Assume graph reads are clearance-filtered by the host.
- Return structured errors; do not panic for normal bad input.

---

## Deployment model

### Container image

A multi-stage `Dockerfile` builds:

- `kanbrick-api`
- `kanbrick-cli`
- seed assets

The runtime image is a slim Debian image running as a fixed non-root UID/GID.

### Modes

`kanbrick-api` supports two modes:

- `control-plane` (default)
- `executor`

### Important runtime settings

Common important variables/flags include:

- `KANBRICK_JWT_SECRET`
- `KANBRICK_MODE`
- `KANBRICK_GUEST_CONCURRENCY`
- `KANBRICK_GUEST_QUEUE_LIMIT`
- `KANBRICK_ASSET_DIR`
- `KANBRICK_INTERNAL_TOKEN`
- `KANBRICK_INTERNAL_PORT`
- `KANBRICK_EXECUTOR_URL`
- `KANBRICK_CP_URL` (executor mode)

### Security deployment notes

- The dev JWT secret fallback exists only for convenience; do **not** rely on it outside development.
- `/metrics` is intended as an **in-cluster** scrape surface.
- Public TLS termination is expected to happen at a reverse proxy / mesh layer.
- The control-plane/executor split assumes internal-only networking and transport-secret protection.

For deeper details, see [`docs/SECURITY.md`](docs/SECURITY.md) and deployment assets under `deploy/`.

---

## Testing and CI

## Main CI workflow

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs the core workspace gate:

- checkout
- SparrowDB submodule init
- best-effort remaining submodule init
- pinned Rust toolchain install
- format check
- clippy with warnings denied
- full workspace build
- WASM guest build validation
- full workspace tests

## Cockpit CI workflow

[`.github/workflows/cockpit.yml`](.github/workflows/cockpit.yml) is a dedicated Cockpit workflow that runs only when Cockpit-relevant paths change. It installs Node and Tauri dependencies, stages the API sidecar, builds the frontend, runs Cockpit Rust checks/tests, builds the Tauri app, and executes a headless login→`/me` smoke.

This separation keeps the core Rust gates fast while still continuously validating the desktop surface.

---

## Current feature status

The old README’s “Phases 0–6 complete” summary is no longer enough to describe the repo.

The current repository clearly includes and/or actively stages:

- **foundation / core shared types**
- **embedded store integration**
- **JWT auth + password flows**
- **WASM mesh runtime**
- **graph analytics / discovery**
- **business guests**
- **smoke testing / validation / CI gates**
- **Cockpit desktop app**
- **guest asset registry + activation**
- **control-plane / executor scale-out model**
- **provider-key custody**
- **scope request / grant surfaces**
- **skill publish / review / binding flows**
- **loop definition / scheduling / run surfaces**
- **component registration / visualizer integration seams**
- **deployment/containerization support**

Some of these layers are more mature than others, but they are now first-class parts of the codebase and should be treated that way when navigating the repo.

Performance notes are tracked in [`docs/benchmarks.md`](docs/benchmarks.md).

---

## Suggested reading paths

### If you want to understand the whole system

1. [`README.md`](README.md)
2. [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
3. [`docs/SECURITY.md`](docs/SECURITY.md)
4. [`kanbrick-api/src/lib.rs`](kanbrick-api/src/lib.rs)
5. [`kanbrick-api/src/main.rs`](kanbrick-api/src/main.rs)
6. [`kanbrick-mesh/src/runtime.rs`](kanbrick-mesh/src/runtime.rs)
7. [`kanbrick-auth/src/guarded.rs`](kanbrick-auth/src/guarded.rs)

### If you want to understand auth and data access

1. [`kanbrick-core/src/context.rs`](kanbrick-core/src/context.rs)
2. [`kanbrick-auth/src/login.rs`](kanbrick-auth/src/login.rs)
3. [`kanbrick-auth/src/guarded.rs`](kanbrick-auth/src/guarded.rs)
4. [`docs/SECURITY.md`](docs/SECURITY.md)

### If you want to understand guest execution

1. [`kanbrick-core/src/abi.rs`](kanbrick-core/src/abi.rs)
2. [`kanbrick-guest-sdk`](kanbrick-guest-sdk)
3. [`kanbrick-mesh/src/lib.rs`](kanbrick-mesh/src/lib.rs)
4. [`kanbrick-mesh/src/runtime.rs`](kanbrick-mesh/src/runtime.rs)
5. [`guests/valuation/src/lib.rs`](guests/valuation/src/lib.rs)
6. [`guests/reporting/src/lib.rs`](guests/reporting/src/lib.rs)
7. [`guests/compliance/src/lib.rs`](guests/compliance/src/lib.rs)

### If you want to understand the desktop app

1. [`cockpit/README.md`](cockpit/README.md)
2. [`cockpit/src/App.tsx`](cockpit/src/App.tsx)
3. [`cockpit/src/api.ts`](cockpit/src/api.ts)
4. [`cockpit/src-tauri/src/sidecar.rs`](cockpit/src-tauri/src/sidecar.rs)
5. [`cockpit/src-tauri/src/auth.rs`](cockpit/src-tauri/src/auth.rs)

---

## Quick command reference

### Build core workspace

```bash
git submodule update --init --depth 1 crates/sparrowdb
cargo build --workspace
```

### Full local gate

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --all-features
cargo test --workspace --all-features
scripts/build-guests.sh
```

### Seed + run API

```bash
cargo run -p kanbrick-cli -- seed --db graph/firm.db
cargo run -p kanbrick-cli -- set-password --email tracy.brittcool@kanbrick.com --password secret --db graph/firm.db
KANBRICK_JWT_SECRET=change-me cargo run -p kanbrick-api -- --port 8080 --db graph/firm.db
```

### Run smoke test

```bash
scripts/smoke.sh
```

### Build and smoke-test container

```bash
scripts/docker-release.sh
```

### Run Cockpit

```bash
cd cockpit
npm install
npm run tauri dev
```

---

## Final note

The most important thing to understand about Kanbrick-V1 is that it is no longer just a small Rust API with three embedded guests. It is now a multi-surface platform repo with:

- a secure host-authoritative data path,
- a pluggable WASM execution model,
- runtime guest activation,
- a desktop operator environment,
- scale-out execution mechanics,
- and emerging agent/skill/loop/provider layers built on top of the same spine.

That spine — **identity → clearance → audited graph access → sandboxed execution** — is still the architectural center of gravity for everything else in the repository.
