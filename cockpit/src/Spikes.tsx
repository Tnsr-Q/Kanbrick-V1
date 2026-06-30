import { useCallback, useRef, useState } from "react";

/**
 * P8.5 (#97) — frontend de-risk spike for ADR-0011.
 *
 * Proves the two heavy UI surfaces the feature phases need actually render in the
 * chosen **React + Vite** webview: a **node-edge graph** (P13 access visualizer)
 * and a **whiteboard canvas** (P10 messenger/brainstorm). Deliberately built with
 * **zero extra dependencies** (inline SVG + the Canvas 2D API) so the de-risk
 * stays CI-deterministic; ADR-0011 records the production libraries that slot in
 * behind the same typed-IPC contract — **React Flow / Cytoscape.js** for the graph
 * and **tldraw / Excalidraw** for the whiteboard — all of which are *above* the
 * Tauri IPC boundary and therefore swappable without touching the spine.
 */
export default function Spikes() {
  return (
    <div className="spikes">
      <div className="spikes-head">
        <h2>UI surface spikes</h2>
      </div>
      <p className="hint">
        ADR-0011 · React + Vite, swappable above the IPC boundary. Production swaps
        in React&nbsp;Flow / Cytoscape (graph) and tldraw / Excalidraw (whiteboard).
      </p>
      <div className="spikes-grid">
        <GraphSpike />
        <WhiteboardSpike />
      </div>
    </div>
  );
}

type Node = { id: string; label: string; x: number; y: number; kind: "firm" | "company" | "person" };
type Edge = { from: string; to: string };

// A tiny static portfolio/org graph — stand-in for the live graph.access stream
// (P13). Layout is hand-placed; production uses a force/layered layout from the
// graph lib.
const NODES: Node[] = [
  { id: "firm", label: "Kanbrick", x: 150, y: 38, kind: "firm" },
  { id: "c1", label: "Acme", x: 56, y: 116, kind: "company" },
  { id: "c2", label: "Globex", x: 150, y: 116, kind: "company" },
  { id: "c3", label: "Initech", x: 244, y: 116, kind: "company" },
  { id: "p1", label: "B. Humphrey", x: 100, y: 190, kind: "person" },
  { id: "p2", label: "Analyst", x: 200, y: 190, kind: "person" },
];
const EDGES: Edge[] = [
  { from: "firm", to: "c1" },
  { from: "firm", to: "c2" },
  { from: "firm", to: "c3" },
  { from: "c1", to: "p1" },
  { from: "c2", to: "p1" },
  { from: "c2", to: "p2" },
  { from: "c3", to: "p2" },
];

const NODE_TONE: Record<Node["kind"], string> = {
  firm: "#6366f1",
  company: "#2dd4bf",
  person: "#f5c542",
};

function GraphSpike() {
  const [selected, setSelected] = useState<string | null>(null);
  const byId = (id: string) => NODES.find((n) => n.id === id)!;
  const isLit = (id: string) =>
    selected === null ||
    selected === id ||
    EDGES.some(
      (e) =>
        (e.from === selected && e.to === id) ||
        (e.to === selected && e.from === id),
    );

  return (
    <figure className="spike-card">
      <figcaption>Node-edge graph (SVG)</figcaption>
      <svg viewBox="0 0 300 224" role="img" aria-label="portfolio graph" className="spike-svg">
        {EDGES.map((e, i) => {
          const a = byId(e.from);
          const b = byId(e.to);
          const lit = isLit(e.from) && isLit(e.to);
          return (
            <line
              key={i}
              x1={a.x}
              y1={a.y}
              x2={b.x}
              y2={b.y}
              stroke={lit ? "rgba(148,163,184,0.7)" : "rgba(148,163,184,0.15)"}
              strokeWidth={1.5}
            />
          );
        })}
        {NODES.map((n) => {
          const lit = isLit(n.id);
          return (
            <g
              key={n.id}
              className="spike-node"
              onClick={() => setSelected((s) => (s === n.id ? null : n.id))}
              style={{ cursor: "pointer", opacity: lit ? 1 : 0.3 }}
            >
              <circle cx={n.x} cy={n.y} r={n.kind === "firm" ? 13 : 10} fill={NODE_TONE[n.kind]} />
              <text x={n.x} y={n.y + 26} textAnchor="middle" className="spike-node-label">
                {n.label}
              </text>
            </g>
          );
        })}
      </svg>
      <p className="hint">Click a node to highlight its neighbours.</p>
    </figure>
  );
}

function WhiteboardSpike() {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const drawing = useRef(false);
  const last = useRef<{ x: number; y: number } | null>(null);

  const pos = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    return {
      x: ((e.clientX - rect.left) / rect.width) * e.currentTarget.width,
      y: ((e.clientY - rect.top) / rect.height) * e.currentTarget.height,
    };
  };

  const onDown = (e: React.PointerEvent<HTMLCanvasElement>) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    drawing.current = true;
    last.current = pos(e);
  };

  const onMove = (e: React.PointerEvent<HTMLCanvasElement>) => {
    if (!drawing.current) return;
    const ctx = canvasRef.current?.getContext("2d");
    const p = pos(e);
    if (!ctx || !last.current) return;
    ctx.strokeStyle = "#6366f1";
    ctx.lineWidth = 2.5;
    ctx.lineCap = "round";
    ctx.beginPath();
    ctx.moveTo(last.current.x, last.current.y);
    ctx.lineTo(p.x, p.y);
    ctx.stroke();
    last.current = p;
  };

  const onUp = () => {
    drawing.current = false;
    last.current = null;
  };

  const clear = useCallback(() => {
    const c = canvasRef.current;
    const ctx = c?.getContext("2d");
    if (c && ctx) ctx.clearRect(0, 0, c.width, c.height);
  }, []);

  return (
    <figure className="spike-card">
      <figcaption>Whiteboard (Canvas 2D)</figcaption>
      <canvas
        ref={canvasRef}
        width={300}
        height={200}
        className="spike-canvas"
        onPointerDown={onDown}
        onPointerMove={onMove}
        onPointerUp={onUp}
        onPointerLeave={onUp}
      />
      <p className="hint">
        Draw with the pointer.{" "}
        <button className="link-btn" onClick={clear}>
          Clear
        </button>
      </p>
    </figure>
  );
}
