import { useEffect, useState } from "react";
import {
  getSidecarStatus,
  getSessionStatus,
  logout,
  onSidecarStatus,
  type SidecarStatus,
} from "./api";
import Login from "./Login";
import "./App.css";

const SIDECAR_COPY = {
  starting: "Starting local services…",
  ready: "API ready",
  failed: "Couldn't start the API",
} as const;

const SIDECAR_TONE = {
  starting: "is-pending",
  ready: "is-ready",
  failed: "is-error",
} as const;

export default function App() {
  const [sidecar, setSidecar] = useState<SidecarStatus>({ state: "starting" });
  const [authed, setAuthed] = useState(false);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;

    getSidecarStatus()
      .then((s) => active && setSidecar(s))
      .catch(() => {});
    onSidecarStatus((s) => setSidecar(s))
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

  // Re-check the session once the API is reachable — covers a token still held
  // host-side after a webview reload (P7.3 persistence).
  useEffect(() => {
    if (sidecar.state !== "ready") return;
    let active = true;
    getSessionStatus()
      .then((s) => active && setAuthed(s.authenticated))
      .catch(() => {});
    return () => {
      active = false;
    };
  }, [sidecar.state]);

  const signOut = async () => {
    try {
      await logout();
    } finally {
      setAuthed(false);
    }
  };

  return (
    <main className="splash">
      <div className="glow" aria-hidden="true" />
      <section className="card">
        <div className="mark" aria-hidden="true">
          <span className="mark-inner" />
        </div>
        <h1>Kanbrick Cockpit</h1>
        <p className="subtitle">L5 · Agentic Desktop on the Firm OS spine</p>

        {sidecar.state !== "ready" ? (
          <div className={`status ${SIDECAR_TONE[sidecar.state]}`} role="status">
            <span className="dot" />
            <span>
              {SIDECAR_COPY[sidecar.state]}
              {sidecar.state === "failed" && (
                <span className="status-detail"> · {sidecar.reason}</span>
              )}
            </span>
          </div>
        ) : authed ? (
          <div className="panel">
            <div className="status is-ready" role="status">
              <span className="dot" />
              <span>Signed in</span>
            </div>
            <p className="hint">Your identity panel (/me) lands in P7.5.</p>
            <button className="btn-secondary" onClick={signOut}>
              Sign out
            </button>
          </div>
        ) : (
          <Login onAuthenticated={() => setAuthed(true)} />
        )}

        <footer className="meta">
          <span>Tauri v2</span>
          <span aria-hidden="true">·</span>
          <span>React + Vite</span>
          <span aria-hidden="true">·</span>
          <span>Phase 7 · #89</span>
        </footer>
      </section>
    </main>
  );
}
