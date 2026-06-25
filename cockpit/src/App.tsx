import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

/**
 * Mirror of the Rust `SidecarStatus` (serde internally-tagged on `state`).
 * The host (P7.2) spawns `kanbrick-api`, health-gates it, and pushes transitions
 * here over the `sidecar-status` event.
 */
type SidecarStatus =
  | { state: "starting" }
  | { state: "ready"; base_url: string }
  | { state: "failed"; reason: string };

const COPY: Record<SidecarStatus["state"], string> = {
  starting: "Starting local services…",
  ready: "API ready",
  failed: "Couldn't start the API",
};

const TONE: Record<SidecarStatus["state"], string> = {
  starting: "is-pending",
  ready: "is-ready",
  failed: "is-error",
};

export default function App() {
  const [status, setStatus] = useState<SidecarStatus>({ state: "starting" });

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;

    // Snapshot first — the transition event may fire before we subscribe.
    invoke<SidecarStatus>("sidecar_status")
      .then((s) => {
        if (active) setStatus(s);
      })
      .catch(() => {
        /* outside Tauri (plain `vite`), the command is absent — stay "starting". */
      });

    listen<SidecarStatus>("sidecar-status", (event) => setStatus(event.payload))
      .then((un) => {
        if (active) unlisten = un;
        else un();
      })
      .catch(() => {});

    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  return (
    <main className="splash">
      <div className="glow" aria-hidden="true" />
      <section className="card">
        <div className="mark" aria-hidden="true">
          <span className="mark-inner" />
        </div>
        <h1>Kanbrick Cockpit</h1>
        <p className="subtitle">L5 · Agentic Desktop on the Firm OS spine</p>

        <div className={`status ${TONE[status.state]}`} role="status">
          <span className="dot" />
          <span>
            {COPY[status.state]}
            {status.state === "ready" && (
              <span className="status-detail"> · {status.base_url}</span>
            )}
            {status.state === "failed" && (
              <span className="status-detail"> · {status.reason}</span>
            )}
          </span>
        </div>

        <footer className="meta">
          <span>Tauri v2</span>
          <span aria-hidden="true">·</span>
          <span>React + Vite</span>
          <span aria-hidden="true">·</span>
          <span>Phase 7 · #88</span>
        </footer>
      </section>
    </main>
  );
}
