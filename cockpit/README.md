# Kanbrick Cockpit (L5)

The **Cockpit** is the L5 agentic desktop — a **Tauri v2** app that sits on top of
the finished L1–L4 Firm OS. It does **not** re-implement the spine: it bundles the
existing `kanbrick-api` binary as a sidecar and is a *client* of the
`HTTP → Auth → Mesh → Guest → Graph` path. See the program handoff
([`docs/handoffs/cockpit-program.md`](../docs/handoffs/cockpit-program.md)) and
the program tracker (#77).

This directory is its **own build graph** — it is in the repo-root `Cargo.toml`
`exclude` set and carries its own empty `[workspace]` (under `src-tauri/`), so
Tauri's large dependency tree never enters the firm-OS workspace and
`cargo build --workspace` is unaffected.

## What's here (P7.1 #87 · P7.2 #88 · P7.3 #89 · P7.4 #90 · P7.5 #91 · P7.6 #92)

The shell window (P7.1), the **managed `kanbrick-api` sidecar** (P7.2), **login +
JWT custody** (P7.3), the **IPC auth bridge** (P7.4 / ADR-0016), and the **`/me`
identity panel** (P7.5). On launch the host spawns the API on an ephemeral
localhost port, health-gates it, and publishes the base URL; the splash then shows
a login form. `login` forwards to the sidecar `POST /login` and holds the JWT
**host-side, in memory**; every authenticated host→sidecar call attaches that token
as Bearer — the webview only ever learns `authenticated: bool` and the identity the
host fetches for it, and can never supply identity. Once signed in, the panel renders
the caller's email, clearance badge (L1–L5), and roles from `GET /me` — the visible
proof of the thin end-to-end path.

```
cockpit/
├── package.json, vite.config.ts, index.html, tsconfig*.json
├── scripts/prepare-sidecar.sh   # builds + stages kanbrick-api as the sidecar
├── src/                         # React + Vite webview
│   ├── main.tsx, App.tsx, App.css   # orchestrator: sidecar × auth state
│   ├── api.ts                   # typed Tauri IPC wrappers
│   ├── Login.tsx                # login form (token never reaches the webview)
│   └── Me.tsx                   # /me identity panel (email · clearance · roles)
└── src-tauri/                   # Tauri v2 host (excluded from the cargo workspace)
    ├── Cargo.toml, tauri.conf.json, build.rs
    ├── src/main.rs, src/lib.rs  # builder + run-event teardown
    ├── src/sidecar.rs           # spawn → /health gate → status events → kill
    ├── src/auth.rs              # Session (JWT custody) + IPC auth bridge (ADR-0016)
    ├── capabilities/default.json
    ├── binaries/                # kanbrick-api-<triple> (gitignored; staged at build)
    └── icons/                   # bundle icons (PNG)
```

The sidecar binary is **not committed** (large, per-triple build artifact). It is
built and staged into `src-tauri/binaries/` by `scripts/prepare-sidecar.sh`, which
runs automatically from `beforeDevCommand`/`beforeBuildCommand`.

### JWT custody (P7.3)

The session JWT lives **host-side, in memory** (`auth::Session`) — never in
`localStorage`, never in logs, never returned to the webview. Because the host
process outlives a webview reload, the session survives a reload (the UI just
re-queries `session_status`). Durable, cross-**restart** secure storage (OS
keychain vs IOTA Stronghold) is deliberately deferred to **P8.2 / ADR-0009**;
`Session` is the seam a durable backing slots into without changing callers.

### IPC auth contract (P7.4 / ADR-0016)

Identity stays **host-authoritative across the Tauri IPC boundary**, mirroring
ADR-0002 across the network:

- No webview→host command takes a `token`/`user`/`clearance`/`firm` argument; the
  webview can only "act as the signed-in user".
- Every authenticated host→sidecar call goes through one bridge (`auth::authed_get`)
  that injects `Authorization: Bearer <Session token>` — never a webview value.
- The **sidecar is the authority**: the host forwards the token and lets
  `kanbrick-api` (`require_clearance`) validate it and rehydrate `FirmContext`. A
  401 clears the session; the UI falls back to login.
- `session_refresh` round-trips `GET /me` so "authenticated" means the token
  actually validates (catches the 8 h TTL on a reload), not merely "present".

Full rationale: [`docs/adr/0016-cockpit-ipc-auth-contract.md`](../docs/adr/0016-cockpit-ipc-auth-contract.md).

## Prerequisites

- **Node** 18+ and **npm**
- **Rust** (repo toolchain; see `../rust-toolchain.toml`)
- **Tauri v2 system dependencies** for your OS (on Linux: `webkit2gtk-4.1`,
  `libayatana-appindicator`, `librsvg2`, etc.) — see
  <https://v2.tauri.app/start/prerequisites/>.

## Run

```bash
cd cockpit
npm install              # generates package-lock.json (not committed at P7.1)
npm run sidecar          # build + stage kanbrick-api (auto-run by dev/build too)
npm run tauri dev        # vite dev server + Tauri window + supervised sidecar
npm run tauri build      # bundle for the host triple
```

`npm run tauri dev` starts Vite on `:1420`, opens the desktop window, and spawns
the `kanbrick-api` sidecar; the splash flips to **API ready** once `GET /health`
returns 200. `npm run sidecar` is invoked automatically by `beforeDevCommand`/
`beforeBuildCommand`, so the binary is always staged before a run.

## CI (P7.6 · #92)

A dedicated workflow, [`.github/workflows/cockpit.yml`](../.github/workflows/cockpit.yml),
locks the thin path into CI. It is **separate** from `ci.yml` and **path-filtered**
to the Cockpit + the crates it bundles, so the heavy webkit/Tauri build only runs
when those change — the existing Rust gates stay untouched. On a matching push/PR to
`main` (or `workflow_dispatch`) it:

1. installs the Tauri webkit2gtk system deps + the `wasm32-wasip1` target
   (`kanbrick-mesh`'s build script compiles the guests during the API build);
2. stages the sidecar, type-checks + builds the frontend (`tsc` + `vite`);
3. gates the Cockpit Rust: `cargo fmt --check`, `clippy -D warnings`, `cargo test`
   (the `sidecar`/`auth` unit tests);
4. builds the Tauri app for the host triple (`tauri build --no-bundle`);
5. runs the **login → /me smoke** ([`scripts/smoke-e2e.sh`](scripts/smoke-e2e.sh)):
   seed → `set-password` → spawn `kanbrick-api` → `/health` → `POST /login` →
   `GET /me` (asserts email + L1–L5 clearance) → unauthenticated `/me` is `401`.

The smoke drives the **real** `kanbrick-api` over the exact login→/me contract the
Cockpit's host commands use, with no display — deterministic and headless. (A full
Tauri-GUI driver e2e — `tauri-driver` + WebKitWebDriver under xvfb — is possible
future hardening.)

## Verification status of this commit

This scaffold was authored in a headless CI-style environment with **no Tauri
toolchain / webview / display**, so the GUI build was not run here. What *was*
verified in-repo:

- ✅ `cargo metadata` / `cargo build --workspace` is **unchanged** —
  `cockpit/src-tauri` is excluded from the workspace graph.
- ✅ All JSON (`tauri.conf.json`, `capabilities/default.json`, `package.json`,
  `tsconfig*.json`) parses; all `.rs` files pass `rustfmt --check`.
- ✅ Bundle icons are valid PNGs at the three referenced sizes.

Exercised on a Tauri-capable machine and by the **Cockpit (L5) CI workflow**
(`.github/workflows/cockpit.yml`, P7.6) — which compiles the cockpit crate (so the
Rust authored across P7.1–P7.5 is finally built + linted + tested on a runner) and
runs the headless login→/me smoke:

- ⏳ `npm run tauri dev` renders the splash in a window (local only; needs a display).
- ✅/CI `tauri build --no-bundle` compiles the app for the host triple.
- ✅/CI The unit tests in `src-tauri/src/sidecar.rs` (health probe) and
  `src-tauri/src/auth.rs` (`Session` set/clear/token) run under the cockpit crate's
  own `cargo test` in the workflow.
- ✅/CI The login → /me contract (seed → `set-password` → `/health` → `POST /login`
  → `GET /me` → unauthenticated `401`) runs headlessly via `scripts/smoke-e2e.sh`.
- ⏳ The remaining GUI-only behaviours (window-close teardown, the visual reload →
  re-validate flow) are validated on a Tauri-capable desktop; a GUI-driver e2e is
  possible future hardening.

Cross-platform icons (`.ico`/`.icns`) can be regenerated from a single source
with `npm run tauri icon path/to/icon.png`; the committed PNGs cover the Linux
host triple.

## Phase 7 status (#78) — complete

P7.1 scaffold (#87) · P7.2 sidecar (#88) · P7.3 login + JWT custody (#89) · P7.4
IPC auth bridge / ADR-0016 (#90) · P7.5 `/me` panel (#91) · P7.6 CI e2e (#92).

The thin end-to-end path (Tauri desktop → bundled `kanbrick-api` sidecar → login →
host-authoritative `/me`) is built and CI-gated. Next phase: **P8 — upstream
de-risk** (#79), which unblocks the feature phases (see the staging matrix in
[`docs/handoffs/cockpit-program.md`](../docs/handoffs/cockpit-program.md) §5a).
