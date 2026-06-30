// Loop run-and-watch panel (P11.7): list the caller's loops, run one, and watch its
// per-step status live over a Tauri Channel until the run reaches a terminal state.
// Identity is host-side (ADR-0016) — the webview passes only the loop/run id + input;
// the host injects the Bearer and the server gates each step at run time.
import { useEffect, useRef, useState } from "react";
import {
  listLoops,
  runLoop,
  stopRunWatch,
  watchRun,
  type LoopSummary,
  type RunEvent,
  type RunView,
} from "./api";
import { SkeletonRows } from "./Skeleton";

/** Per-step status → the shared status-pill tone. */
const STEP_TONE: Record<string, string> = {
  pending: "is-pending",
  running: "is-pending",
  completed: "is-ready",
  denied: "is-error",
  failed: "is-error",
  timed_out: "is-error",
};

/** Run status → the shared status-pill tone. */
const RUN_TONE: Record<string, string> = {
  running: "is-pending",
  completed: "is-ready",
  failed: "is-error",
};

/** A loop step's kind + detail, derived from the threaded fields (Slice 4) using the
 * server's resolution priority: tool → mcp-tool, else provider → provider, else guest.
 * The webview only reflects what the host sent (ADR-0016). */
function stepKind(s: {
  provider?: string | null;
  model?: string | null;
  tool?: string | null;
}): { kind: "guest" | "provider" | "mcp-tool"; detail: string } {
  if (s.tool) return { kind: "mcp-tool", detail: s.tool };
  if (s.provider)
    return {
      kind: "provider",
      detail: s.model ? `${s.provider} · ${s.model}` : s.provider,
    };
  return { kind: "guest", detail: "" };
}

export default function LoopRunner() {
  const [loops, setLoops] = useState<LoopSummary[]>([]);
  const [selected, setSelected] = useState<string>("");
  const [run, setRun] = useState<RunView | null>(null);
  const [running, setRunning] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const watchRef = useRef<string | null>(null);

  // Load the caller's loops; default the selection to the first.
  useEffect(() => {
    let active = true;
    listLoops()
      .then((ls) => {
        if (!active) return;
        setLoops(ls);
        if (ls.length > 0) setSelected((s) => s || ls[0].loop_id);
      })
      .catch((e) => active && setError(String(e)))
      .finally(() => active && setLoaded(true));
    return () => {
      active = false;
    };
  }, []);

  // Stop any live watch on unmount.
  useEffect(
    () => () => {
      if (watchRef.current) void stopRunWatch(watchRef.current);
    },
    [],
  );

  const selectedLoop = loops.find((l) => l.loop_id === selected) ?? null;

  const onRun = async () => {
    if (!selected || running) return;
    setError(null);
    setRunning(true);
    setRun(null);
    // Stop a prior watch before starting a new run.
    if (watchRef.current) {
      void stopRunWatch(watchRef.current);
      watchRef.current = null;
    }
    try {
      const initial = await runLoop(selected, {});
      setRun(initial);
      const watchId = await watchRun(initial.run_id, (event: RunEvent) => {
        switch (event.event) {
          case "snapshot":
            setRun(event.run);
            if (event.run.status !== "running") setRunning(false);
            break;
          case "error":
            setError(event.message);
            break;
          case "stopped":
            setRunning(false);
            break;
        }
      });
      watchRef.current = watchId;
    } catch (e) {
      setError(String(e));
      setRunning(false);
    }
  };

  return (
    <section className="card loop-runner">
      <h1>Loops</h1>
      <p className="subtitle">
        Run a loop and watch each step
        {running && <span className="chip live"> running</span>}
      </p>

      {error && <p className="error">{error}</p>}

      {!loaded && !error ? (
        <SkeletonRows rows={3} />
      ) : loops.length === 0 ? (
        <p className="hint">
          No loops yet. Author one via the API (<code>POST /me/loops</code>), then
          run it here.
        </p>
      ) : (
        <>
          <div className="loop-picker">
            <select
              aria-label="Loop"
              value={selected}
              onChange={(e) => {
                setSelected(e.target.value);
                setRun(null);
              }}
              disabled={running}
            >
              {loops.map((l) => (
                <option key={l.loop_id} value={l.loop_id}>
                  {l.name} · {l.steps.length} step
                  {l.steps.length === 1 ? "" : "s"}
                </option>
              ))}
            </select>
            <button
              className="btn-secondary"
              onClick={onRun}
              disabled={running || !selected}
            >
              {running ? "Running…" : "Run"}
            </button>
          </div>

          {/* Before a run: show the loop's static step plan. */}
          {selectedLoop && !run && (
            <ol className="step-list">
              {selectedLoop.steps.map((s) => {
                const k = stepKind(s);
                return (
                  <li className="step-row" key={s.position}>
                    <span className="step-pos">{s.position + 1}</span>
                    <span className="step-skill">{s.skill_name}</span>
                    <span className={`kind-tag kind-tag-${k.kind}`}>
                      {k.kind}
                    </span>
                    {k.detail && <span className="step-meta">{k.detail}</span>}
                    <span className="step-scope">{s.scope_id}</span>
                  </li>
                );
              })}
            </ol>
          )}

          {/* During/after a run: live per-step status. */}
          {run && (
            <div className="run-view">
              <div
                className={`status ${RUN_TONE[run.status] ?? "is-pending"}`}
                role="status"
              >
                <span className="dot" />
                <span>Run {run.status}</span>
              </div>
              <ol className="step-list">
                {run.steps.map((s) => {
                  const k = stepKind(s);
                  return (
                    <li
                      className={`step-row${s.status === "running" ? " is-current" : ""}`}
                      key={s.position}
                    >
                      <span className="step-pos">{s.position + 1}</span>
                      <span className="step-skill">{s.skill_name}</span>
                      <span className={`kind-tag kind-tag-${k.kind}`}>
                        {k.kind}
                      </span>
                      {k.detail && <span className="step-meta">{k.detail}</span>}
                      <span
                        className={`chip step-badge ${STEP_TONE[s.status] ?? ""}`}
                      >
                        {s.status.replace("_", " ")}
                      </span>
                      {s.detail && (
                        <span className="step-detail">{s.detail}</span>
                      )}
                    </li>
                  );
                })}
              </ol>
            </div>
          )}
        </>
      )}
    </section>
  );
}
