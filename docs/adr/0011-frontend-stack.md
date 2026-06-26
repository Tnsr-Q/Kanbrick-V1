# ADR 0011 — Frontend stack: React + Vite webview, swappable above the IPC boundary

- **Status:** Accepted
- **Date:** 2026-06-25
- **Context:** P8.5 (#97), **Phase 8 — Upstream De-Risk** (#79), L5 Cockpit
  program (#77). The frontend framework is a one-way door for every UI slice in
  P10–P14. Builds on ADR-0016 (the typed IPC contract) and the P7.1 scaffold.
- **Deciders:** operator (frontend is an operator decision) + P8 de-risk agent.

## Context

Every feature phase ships UI: messenger + whiteboard (P10), skill / loop consoles
(P11), token dashboards (P12), the access visualizer (P13), tenant admin (P14).
The framework choice gates all of them. The operator chose **React + Vite**, kept
**upgradeable** — the spine stays framework-agnostic *below* the Tauri IPC
boundary, so the webview can be swapped later without touching Rust.

## Decision

1. **React + Vite webview** (already scaffolded in P7.1).
2. **Swappable-above-IPC guarantee.** Every datum crosses the boundary through
   typed Tauri `invoke` commands and `listen` event subscriptions in
   `cockpit/src/api.ts`; identity stays host-authoritative (ADR-0016); there is
   **no framework lock-in below IPC**. Replacing React later is a contained change
   above the boundary.
3. **Component libraries:** **tldraw / Excalidraw** for the whiteboard (P10);
   **Cytoscape.js / React Flow** for graph visualization (P13). Both sit above the
   IPC boundary and are therefore swappable.

## Probe evidence

The spike `cockpit/src/Spikes.tsx` renders **both** heavy surfaces — a node-edge
graph (inline SVG) and a whiteboard (Canvas 2D) — with **zero new npm
dependencies**. The zero-dep choice is deliberate: a de-risk should prove the
surfaces *render* and the typed-IPC seam holds without committing heavy libraries
during a probe. It is wired into `App.tsx` via a view toggle; styling in
`App.css`. Type-check + build are gated by `.github/workflows/cockpit.yml`; as in
Phase 7, the GUI itself cannot run in this headless environment. Production swaps
in tldraw / React Flow behind the **same** `api.ts` contract.

## Alternatives considered

- **Svelte / SolidJS.** Rejected: smaller mature-canvas / graph ecosystems for our
  needs.
- **Native Rust UI (egui).** Rejected: weaker for a rich whiteboard / graph and
  slower iteration; also forfeits the swappable-webview property.
- **Commit tldraw + React Flow now.** Deferred to P10 / P13 — a de-risk proves
  rendering and the contract; it does not finalize the lib.

## Consequences

- ADR-0011 **unblocks every UI slice** (P10.3/10.5, P11.6/11.7, P12.5, P13.6,
  P14.6 — the staging matrix in `docs/handoffs/cockpit-program.md` §5a).
- `cockpit/src/api.ts` is the typed boundary all UI slices honour; swapping the
  webview later is contained above IPC.
- The spike is a throwaway surface; production components land with their phases.
