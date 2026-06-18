# Kanbrick-V1

A four-layer **Firm Operating System** on an all-Rust stack — one workspace, one
command, zero external runtimes. It models a 12-person investment firm and its
9-company portfolio as a graph, enforces a five-tier clearance model end to end,
and runs business logic as sandboxed WASM guests.

| Layer | Crate | Upstream | Role |
| --- | --- | --- | --- |
| **L1 — Guard** | `kanbrick-auth` | Ironclaw primitives (JWT, Argon2id) | identity, clearance, audit |
| **L2 — Nerves** | `kanbrick-mesh` | wasmtime 45 (WASIp1) | WASM runtime, dispatch, events |
| **L3 — Brain** | `kanbrick-store` | SparrowDB (embedded) | the firm graph (Cypher) |
| **L4 — Map** | `kanbrick-discovery` | graphify-rs libs | graph analytics |

The HTTP API (`kanbrick-api`) ties them together as one path:
**HTTP → Auth → Mesh → Guest → Graph**. The three business guests
(`valuation`, `reporting`, `compliance`) are compiled to `wasm32-wasip1` and
**embedded** in the API binary.

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md),
[`docs/SECURITY.md`](docs/SECURITY.md), and
[`CONTRIBUTING.md`](CONTRIBUTING.md) (how to write a guest). Design decisions are
recorded in [`docs/adr/`](docs/adr/).

## One command

```bash
git clone <repo> && cd Kanbrick-V1
git submodule update --init --depth 1 crates/sparrowdb   # the one required upstream
cargo build --release --workspace
```

### Run it

```bash
# Seed a database and provision a login:
cargo run --release -p kanbrick-cli -- seed --db graph/firm.db
cargo run --release -p kanbrick-cli -- set-password \
    --email tracy.brittcool@kanbrick.com --password secret --db graph/firm.db

# Start the API (embeds all three guests):
KANBRICK_JWT_SECRET=change-me cargo run --release -p kanbrick-api -- --port 8080 --db graph/firm.db

# Health, login, and a guest query:
curl localhost:8080/health
TOKEN=$(curl -s localhost:8080/login -H 'content-type: application/json' \
  -d '{"email":"tracy.brittcool@kanbrick.com","password":"secret"}' | jq -r .token)
curl -s localhost:8080/guests/reporting -H "Authorization: Bearer $TOKEN" -d '{}'
```

The whole clone-to-running-system path is validated by
[`scripts/smoke.sh`](scripts/smoke.sh) (#51).

## HTTP surface

| Route | Auth | Purpose |
| --- | --- | --- |
| `POST /login` | — | email + password → JWT |
| `GET /me` | JWT | the caller's identity |
| `GET /admin` | JWT, L4+ | a clearance-gated example route |
| `GET /health` | — | liveness + embedded-guest count |
| `POST /guests/{name}` | JWT | invoke a guest (`valuation` L3+, `compliance` L4+, `reporting` any) |

## Develop

```bash
cargo test --workspace --all-features                 # unit + integration (native + real-wasm)
cargo clippy --workspace --all-targets -- -D warnings  # lint gate
cargo fmt --all --check                                # format gate
scripts/build-guests.sh                                # build + size-check the guest wasm (#40)
```

The toolchain is pinned in `rust-toolchain.toml` (Rust 1.94.1, target
`wasm32-wasip1`). The four vendored upstreams live as submodules under `crates/`
and are progressively integrated through the `kanbrick-*` wrapper crates; only
`crates/sparrowdb` is required to build.

## Status

Phases 0–6 complete: foundation, store, auth, mesh, discovery, business guests,
and testing/validation. Performance numbers are tracked in
[`docs/benchmarks.md`](docs/benchmarks.md). Deployment artifacts (a self-contained
binary and a container image) are described in [`docs/SECURITY.md`](docs/SECURITY.md)
and built by [`scripts/docker-release.sh`](scripts/docker-release.sh).
