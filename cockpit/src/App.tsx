import "./App.css";

/**
 * P7.1 — Cockpit shell splash.
 *
 * Deliberately static: no IPC, no identity, no API call. The kanbrick-api
 * sidecar (P7.2), login + JWT custody (P7.3), the IPC auth bridge (P7.4), and
 * the live `/me` panel (P7.5) replace this splash in later slices. The desktop
 * is a *client* of the finished spine — it re-implements nothing.
 */
export default function App() {
  return (
    <main className="splash">
      <div className="glow" aria-hidden="true" />
      <section className="card">
        <div className="mark" aria-hidden="true">
          <span className="mark-inner" />
        </div>
        <h1>Kanbrick Cockpit</h1>
        <p className="subtitle">L5 · Agentic Desktop on the Firm OS spine</p>
        <div className="status">
          <span className="dot" />
          <span>Shell ready — API sidecar wiring lands in P7.2</span>
        </div>
        <footer className="meta">
          <span>Tauri v2</span>
          <span aria-hidden="true">·</span>
          <span>React + Vite</span>
          <span aria-hidden="true">·</span>
          <span>Phase 7 · #87</span>
        </footer>
      </section>
    </main>
  );
}
