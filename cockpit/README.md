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

## What's here (P7.1 · #87)

The empty shell only: one window rendering a static React splash. No IPC, no
identity, no API call yet — those arrive in the following slices.

```
cockpit/
├── package.json, vite.config.ts, index.html, tsconfig*.json
├── src/                     # React + Vite webview
│   ├── main.tsx, App.tsx, App.css
└── src-tauri/               # Tauri v2 host (excluded from the cargo workspace)
    ├── Cargo.toml, tauri.conf.json, build.rs
    ├── src/{main,lib}.rs    # `run()` builds the window; no commands yet
    ├── capabilities/default.json
    └── icons/               # bundle icons (PNG)
```

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
npm run tauri dev        # vite dev server + Tauri window
npm run tauri build      # bundle for the host triple
```

`npm run tauri dev` starts Vite on `:1420` and opens the desktop window;
`npm run tauri build` produces a bundle for the host triple.

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

Cross-platform icons (`.ico`/`.icns`) can be regenerated from a single source
with `npm run tauri icon path/to/icon.png`; the committed PNGs cover the Linux
host triple.

## Next slices (Phase 7 · #78)

P7.2 sidecar (`kanbrick-api` spawn → `/health` → teardown) · P7.3 login + JWT
custody · P7.4 IPC auth bridge (ADR-0016) · P7.5 `/me` panel · P7.6 CI e2e.
