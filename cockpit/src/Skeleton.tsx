// Loading-state placeholders. A <Skeleton> is one shimmering block sized to the
// content it stands in for; <SkeletonRows> is a quick stack of line placeholders
// for a loading list. The shimmer is pure CSS and is disabled under
// prefers-reduced-motion (see App.css), so these stay calm and dependency-free.
import type { CSSProperties } from "react";

/** A single shimmering placeholder block. `width`/`height`/`radius` take any CSS
 * length (numbers → px); defaults fill the row at one line tall. */
export function Skeleton({
  width,
  height,
  radius = 8,
  className = "",
}: {
  width?: number | string;
  height?: number | string;
  radius?: number | string;
  className?: string;
}) {
  const style: CSSProperties = {
    width: width ?? "100%",
    height: height ?? "1em",
    borderRadius: radius,
  };
  return (
    <span className={`skeleton ${className}`.trim()} style={style} aria-hidden="true" />
  );
}

/** A vertical stack of `rows` line placeholders — a one-liner for a loading list. */
export function SkeletonRows({
  rows = 3,
  className = "",
}: {
  rows?: number;
  className?: string;
}) {
  return (
    <div
      className={`skeleton-rows ${className}`.trim()}
      role="status"
      aria-busy="true"
      aria-label="Loading"
    >
      {Array.from({ length: rows }, (_, i) => (
        <span className="skeleton skeleton-row" key={i} aria-hidden="true" />
      ))}
    </div>
  );
}
