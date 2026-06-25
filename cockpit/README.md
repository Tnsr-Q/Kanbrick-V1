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

## What's here (P7.1 #87 · P7.2 #88 · P7.3 #89 · P7.4 #90)

The shell window (P7.1), the **managed `kanbrick-api` sidecar** (P7.2), **login +
JWT custody** (P7.3), and the **IPC auth bridge** (P7.4 / ADR-0016). On launch the
host spawns the API on an ephemeral localhost port, health-gates it, and publishes
the base URL; the splash then shows a login form. `login` forwards to the sidecar
`POST /login` and holds the JWT **host-side, in memory**; every authenticated
host→sidecar call attaches that token as Bearer — the webview only ever learns
`authenticated: bool` and can never supply identity.

```
cockpit/
├── package.json, vite.config.ts, index.html, tsconfig*.json
├── scripts/prepare-sidecar.sh   # builds + stages kanbrick-api as the sidecar
├── src/                         # React + Vite webview
│   ├── main.tsx, App.tsx, App.css   # orchestrator: sidecar × auth state
│   ├── api.ts                   # typed Tauri IPC wrappers
│   └── Login.tsx                # login form (token never reaches the webview)
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

## Verification status of this commit

This scaffold was authored in a headless CI-style environment with **no Tauri
toolchain / webview / display**, so the GUI build was not run here. What *was*
verified in-repo:

- ✅ `cargo metadata` / `cargo build --workspace` is **unchanged** —
  `cockpit/src-tauri` is excluded from the workspace graph.
- ✅ All JSON (`tauri.conf.json`, `capabilities/default.json`, `package.json`,
  `tsconfig*.json`) parses; all `.rs` files pass `rustfmt --check`.
- ✅ Bundle icons are valid PNGs at the three referenced sizes.

Pending a Tauri-capable machine / the P7.6 CI job (#92):

- ⏳ `npm run tauri dev` renders the splash in a window.
- ⏳ `npm run tauri build` produces a bundle for the host triple.
- ⏳ The pure-std unit tests in `src-tauri/src/sidecar.rs` (health probe) and
  `src-tauri/src/auth.rs` (`Session` set/clear/token) run under the cockpit crate's
  own `cargo test` — they need Tauri's deps to compile the crate, so they run in a
  Tauri-capable env, not this one.
- ⏳ End-to-end spawn → `/health` 200 → window-close teardown (#88); login →
  reload → token re-validated against `/me` → still-authenticated → logout
  (#89/#90, seed a user via `kanbrick-cli set-password`); expired-token → 401 →
  session cleared (#90) — exercised by P7.6 (#92).

Cross-platform icons (`.ico`/`.icns`) can be regenerated from a single source
with `npm run tauri icon path/to/icon.png`; the committed PNGs cover the Linux
host triple.

## Next slices (Phase 7 · #78)

Done: P7.1 scaffold (#87) · P7.2 sidecar (#88) · P7.3 login + JWT custody (#89) ·
P7.4 IPC auth bridge / ADR-0016 (#90). Next: P7.5 `/me` panel · P7.6 CI e2e.
