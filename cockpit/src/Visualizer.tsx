// Live component visualizer (P10.5, #117): a card per running component with live
// health gauges fed by a Tauri Channel stream, the component's clearance, and a
// clearance-gated action. The catalogue is read host-side through the auth bridge
// (ADR-0016) — no identity or secret crosses the IPC outward; the webview holds
// only what the host chooses to send.
import { useEffect, useState } from "react";
import {
  listComponents,
  me,
  stopWatching,
  watchComponents,
  type ComponentsEvent,
  type ComponentStatus,
} from "./api";

/** L1..L5 → human label (the firm's five-tier clearance model). */
const CLEARANCE_LABEL: Record<string, string> = {
  L1: "Support",
  L2: "Execution",
  L3: "Operational",
  L4: "Strategic",
  L5: "Admin",
};

/** L1..L5 → comparable rank. */
const RANK: Record<string, number> = { L1: 1, L2: 2, L3: 3, L4: 4, L5: 5 };

/** Whether a `viewer` clearance meets a component's `required` floor. */
const permits = (viewer: string, required: string): boolean =>
  (RANK[viewer] ?? 0) >= (RANK[required] ?? Number.MAX_SAFE_INTEGER);

function Gauge({ label, value }: { label: string; value: number }) {
  return (
    <div className={`gauge ${value > 0 ? "is-live" : ""}`}>
      <span className="gauge-value">{value}</span>
      <span className="gauge-label">{label}</span>
    </div>
  );
}

export default function Visualizer({ onBack }: { onBack: () => void }) {
  const [components, setComponents] = useState<ComponentStatus[]>([]);
  const [clearance, setClearance] = useState<string>("");
  const [error, setError] = useState<string | null>(null);
  const [live, setLive] = useState(false);
  const [expanded, setExpanded] = useState<string | null>(null);

  // Viewer clearance (for gating) + the initial catalogue.
  useEffect(() => {
    let active = true;
    me()
      .then((id) => active && setClearance(id.clearance))
      .catch(() => {});
    listComponents()
      .then((c) => active && setComponents(c))
      .catch((e) => active && setError(String(e)));
    return () => {
      active = false;
    };
  }, []);

  // Live snapshots over the Channel; the watch is stopped on unmount.
  useEffect(() => {
    let active = true;
    let watchId: string | null = null;
    watchComponents((event: ComponentsEvent) => {
      if (!active) return;
      switch (event.event) {
        case "snapshot":
          setComponents(event.components);
          setLive(true);
          break;
        case "error":
          setError(event.message);
          break;
        case "stopped":
          setLive(false);
          break;
      }
    })
      .then((id) => {
        watchId = id;
        // Unmounted before the watch id came back — stop it immediately.
        if (!active) void stopWatching(id);
      })
      .catch((e) => active && setError(String(e)));
    return () => {
      active = false;
      if (watchId) void stopWatching(watchId);
    };
  }, []);

  return (
    <section className="card visualizer">
      <button className="link-btn back" onClick={onBack}>
        ← Back
      </button>
      <h1>Visualizer</h1>
      <p className="subtitle">
        Live component health
        {live && <span className="chip live"> live</span>}
      </p>

      {error && <p className="error">{error}</p>}

      {components.length === 0 ? (
        <p className="hint">No components running.</p>
      ) : (
        <div className="component-grid">
          {components.map((c) => {
            const label = CLEARANCE_LABEL[c.clearance] ?? "";
            const canManage = permits(clearance, c.clearance);
            const isOpen = expanded === c.name && canManage;
            return (
              <div className="component-card" key={c.name}>
                <div className="component-head">
                  <span className="component-name">{c.name}</span>
                  <span className={`badge badge-${c.clearance.toLowerCase()}`}>
                    <span className="badge-level">{c.clearance}</span>
                    {label && <span className="badge-label">{label}</span>}
                  </span>
                </div>
                <div className="component-version">v{c.version}</div>

                <div className="gauges">
                  <Gauge label="active" value={c.active} />
                  <Gauge label="completed" value={c.completed} />
                  <Gauge label="failed" value={c.failed} />
                  <Gauge label="timed out" value={c.timed_out} />
                </div>

                <div className="component-actions">
                  {canManage ? (
                    <button
                      className="btn-secondary"
                      onClick={() =>
                        setExpanded((e) => (e === c.name ? null : c.name))
                      }
                    >
                      {isOpen ? "Hide" : "Manage"}
                    </button>
                  ) : (
                    <span className="hint locked">
                      Requires {c.clearance} to manage
                    </span>
                  )}
                </div>

                {isOpen && (
                  <p className="hint">
                    Control actions (reload / restart) land in a later slice — this
                    panel is gated to {c.clearance}+.
                  </p>
                )}
              </div>
            );
          })}
        </div>
      )}
    </section>
  );
}
