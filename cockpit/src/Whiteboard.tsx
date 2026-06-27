// Lightweight collaborative whiteboard (P10.3, #115). Strokes are captured on a
// canvas and handed up via `onStroke`; the parent sends each as a messenger message
// (scoped to the `whiteboard` group), and incoming stroke messages come back as
// `strokes` props — so the board syncs over the messenger event stream with no extra
// backend surface. Points are normalized 0..1 so they render at any canvas size.
import { useEffect, useRef } from "react";

export type Stroke = { color: string; points: [number, number][] };

const STROKE_COLOR = "#818cf8";

function drawStroke(
  ctx: CanvasRenderingContext2D,
  stroke: Stroke,
  w: number,
  h: number,
) {
  if (stroke.points.length < 2) return;
  ctx.strokeStyle = stroke.color || STROKE_COLOR;
  ctx.lineWidth = 2;
  ctx.lineJoin = "round";
  ctx.lineCap = "round";
  ctx.beginPath();
  const [x0, y0] = stroke.points[0];
  ctx.moveTo(x0 * w, y0 * h);
  for (let i = 1; i < stroke.points.length; i++) {
    const [x, y] = stroke.points[i];
    ctx.lineTo(x * w, y * h);
  }
  ctx.stroke();
}

export default function Whiteboard({
  strokes,
  onStroke,
}: {
  strokes: Stroke[];
  onStroke: (stroke: Stroke) => void;
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const points = useRef<[number, number][]>([]);
  const drawing = useRef(false);

  // Redraw every known stroke whenever the set changes.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const rect = canvas.getBoundingClientRect();
    canvas.width = rect.width;
    canvas.height = rect.height;
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    for (const stroke of strokes) {
      drawStroke(ctx, stroke, canvas.width, canvas.height);
    }
  }, [strokes]);

  const at = (e: React.PointerEvent<HTMLCanvasElement>): [number, number] => {
    const rect = e.currentTarget.getBoundingClientRect();
    return [
      (e.clientX - rect.left) / rect.width,
      (e.clientY - rect.top) / rect.height,
    ];
  };

  const onDown = (e: React.PointerEvent<HTMLCanvasElement>) => {
    drawing.current = true;
    points.current = [at(e)];
    e.currentTarget.setPointerCapture(e.pointerId);
  };

  const onMove = (e: React.PointerEvent<HTMLCanvasElement>) => {
    if (!drawing.current) return;
    points.current.push(at(e));
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (canvas && ctx) {
      drawStroke(
        ctx,
        { color: STROKE_COLOR, points: points.current },
        canvas.width,
        canvas.height,
      );
    }
  };

  const onUp = () => {
    if (!drawing.current) return;
    drawing.current = false;
    if (points.current.length > 1) {
      onStroke({ color: STROKE_COLOR, points: points.current });
    }
    points.current = [];
  };

  return (
    <canvas
      ref={canvasRef}
      className="whiteboard"
      onPointerDown={onDown}
      onPointerMove={onMove}
      onPointerUp={onUp}
      onPointerLeave={onUp}
    />
  );
}
