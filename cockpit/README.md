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

## What's here (P7.1 #87 · P7.2 #88)

The shell window (P7.1) plus the **managed `kanbrick-api` sidecar** (P7.2): on
launch the host spawns the API binary on an ephemeral localhost port, polls
`GET /health` until green, publishes the base URL to the webview, and kills the
child on exit. The splash reflects live sidecar state (starting → ready / failed).
No identity yet — login/JWT custody (P7.3) and the IPC auth bridge (P7.4) come next.

```
cockpit/
├── package.json, vite.config.ts, index.html, tsconfig*.json
├── scripts/prepare-sidecar.sh   # builds + stages kanbrick-api as the sidecar
├── src/                         # React + Vite webview
│   ├── main.tsx, App.tsx, App.css   # status-aware health splash
└── src-tauri/                   # Tauri v2 host (excluded from the cargo workspace)
    ├── Cargo.toml, tauri.conf.json, build.rs
    ├── src/main.rs, src/lib.rs  # builder + run-event teardown
    ├── src/sidecar.rs           # spawn → /health gate → status events → kill
    ├── capabilities/default.json
    ├── binaries/                # kanbrick-api-<triple> (gitignored; staged at build)
    └── icons/                   # bundle icons (PNG)
```

The sidecar binary is **not committed** (large, per-triple build artifact). It is
built and staged into `src-tauri/binaries/` by `scripts/prepare-sidecar.sh`, which
runs automatically from `beforeDevCommand`/`beforeBuildCommand`.

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
- ⏳ The sidecar health-probe tests in `src-tauri/src/sidecar.rs` (pure-std logic:
  passes on a 200 stub, fails on non-200 / a closed port) run under the cockpit
  crate's own `cargo test` — they need Tauri's deps to compile the crate, so they
  run in a Tauri-capable env, not this one.
- ⏳ End-to-end spawn → `/health` 200 → window-close teardown (the #88 integration
  criterion) — exercised by P7.6 (#92) once a sidecar binary is staged.

Cross-platform icons (`.ico`/`.icns`) can be regenerated from a single source
with `npm run tauri icon path/to/icon.png`; the committed PNGs cover the Linux
host triple.

## Next slices (Phase 7 · #78)

Done: P7.1 scaffold (#87) · P7.2 sidecar (#88). Next: P7.3 login + JWT custody ·
P7.4 IPC auth bridge (ADR-0016) · P7.5 `/me` panel · P7.6 CI e2e.
