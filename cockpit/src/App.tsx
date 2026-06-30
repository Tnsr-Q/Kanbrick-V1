import { useEffect, useState, type ReactNode } from "react";
import {
  getSidecarStatus,
  logout,
  me,
  onSidecarStatus,
  sessionRefresh,
  type Identity,
  type SidecarStatus,
} from "./api";
import Login from "./Login";
import Me from "./Me";
import Spikes from "./Spikes";
import Providers from "./Providers";
import Visualizer from "./Visualizer";
import Messenger from "./Messenger";
import LoopRunner from "./LoopRunner";
import SkillStudio from "./SkillStudio";
import Shell, { type View } from "./Shell";
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

/** `unknown` while the held token is being validated against `/me` (P7.4). */
type Auth = "unknown" | "in" | "out";

/** Map the active nav destination to its panel. The shell stays mounted around it,
 * so only the panel swaps when navigating. */
function renderView(view: View): ReactNode {
  switch (view) {
    case "home":
      return (
        <section className="card home">
          <h1>Welcome to the Cockpit</h1>
          <p className="subtitle">L5 · Agentic Desktop on the Firm OS spine</p>
          <Me />
        </section>
      );
    case "loops":
      return <LoopRunner />;
    case "skills":
      return <SkillStudio />;
    case "visualizer":
      return <Visualizer />;
    case "messenger":
      return <Messenger />;
    case "providers":
      return <Providers />;
    case "spikes":
      return <Spikes />;
  }
}

export default function App() {
  const [sidecar, setSidecar] = useState<SidecarStatus>({ state: "starting" });
  const [auth, setAuth] = useState<Auth>("unknown");
  const [view, setView] = useState<View>("home");
  const [identity, setIdentity] = useState<Identity | null>(null);

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

  // Once the API is reachable, validate any held token against `/me` through the
  // host auth bridge (ADR-0016) — covers a session surviving a webview reload,
  // and detects an expired token (a 401 clears it host-side).
  useEffect(() => {
    if (sidecar.state !== "ready") return;
    let active = true;
    setAuth("unknown");
    sessionRefresh()
      .then((s) => active && setAuth(s.authenticated ? "in" : "out"))
      .catch(() => active && setAuth("out"));
    return () => {
      active = false;
    };
  }, [sidecar.state]);

  // Identity for the shell's footer (email + clearance). Host-side via the auth
  // bridge; the webview only renders what the host sends (ADR-0016).
  useEffect(() => {
    if (auth !== "in") {
      setIdentity(null);
      return;
    }
    let active = true;
    me()
      .then((id) => active && setIdentity(id))
      .catch(() => {});
    return () => {
      active = false;
    };
  }, [auth]);

  const signOut = async () => {
    try {
      await logout();
    } finally {
      setAuth("out");
      setView("home");
    }
  };

  // Signed in → the persistent nav shell wraps the active view.
  if (sidecar.state === "ready" && auth === "in") {
    return (
      <Shell
        view={view}
        onNavigate={setView}
        email={identity?.email ?? null}
        clearance={identity?.clearance ?? null}
        onSignOut={signOut}
      >
        {renderView(view)}
      </Shell>
    );
  }

  // Pre-auth (sidecar booting, session check, or signed out) → the splash.
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
        ) : auth === "unknown" ? (
          <div className="status is-pending" role="status">
            <span className="dot" />
            <span>Checking session…</span>
          </div>
        ) : (
          <Login onAuthenticated={() => setAuth("in")} />
        )}

        <footer className="meta">
          <span>Tauri v2</span>
          <span aria-hidden="true">·</span>
          <span>React + Vite</span>
        </footer>
      </section>
    </main>
  );
}
