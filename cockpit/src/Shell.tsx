// The persistent app shell (the chrome that stays mounted across views): a left
// nav rail with the brand, the destination links (active one highlighted), and a
// footer carrying the host-authoritative identity (ADR-0016 — the webview only
// renders what the host sent) plus sign-out. The active view renders into the
// scrollable content region as `children`, so navigating no longer tears the whole
// screen down and back up — only the panel swaps.
import type { ReactNode } from "react";

/** The shell's navigable destinations. "home" is the /me identity panel; the rest
 * are the tool surfaces previously reached via the splash footer links. */
export type View =
  | "home"
  | "loops"
  | "skills"
  | "visualizer"
  | "messenger"
  | "providers"
  | "spikes";

type NavItem = { key: View; label: string };

const NAV: NavItem[] = [
  { key: "home", label: "Home" },
  { key: "loops", label: "Loops" },
  { key: "skills", label: "Skill Studio" },
  { key: "visualizer", label: "Visualizer" },
  { key: "messenger", label: "Messenger" },
  { key: "providers", label: "BYO-AI" },
  { key: "spikes", label: "UI Spikes" },
];

/** Minimal inline glyphs (stroke = currentColor) — no icon dependency. */
function NavIcon({ view }: { view: View }) {
  const paths: Record<View, ReactNode> = {
    home: (
      <>
        <path d="M4 10l8-6 8 6" />
        <path d="M6 9v10h12V9" />
      </>
    ),
    loops: (
      <>
        <path d="M20 11a8 8 0 0 0-14-4L4 9" />
        <path d="M4 4v5h5" />
        <path d="M4 13a8 8 0 0 0 14 4l2-2" />
        <path d="M20 20v-5h-5" />
      </>
    ),
    skills: (
      <>
        <path d="M6 4h11a2 2 0 0 1 2 2v14H8a2 2 0 0 1-2-2z" />
        <path d="M6 16h13" />
      </>
    ),
    visualizer: <path d="M3 12h4l3 7 4-15 3 8h4" />,
    messenger: <path d="M4 5h16v11H9l-5 4z" />,
    providers: <path d="M13 3L5 13h6l-1 8 8-10h-6z" />,
    spikes: (
      <>
        <path d="M9 3h6" />
        <path d="M10 3v6l-5 9a2 2 0 0 0 2 3h10a2 2 0 0 0 2-3l-5-9V3" />
        <path d="M7 15h10" />
      </>
    ),
  };
  return (
    <svg
      viewBox="0 0 24 24"
      width="18"
      height="18"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.7"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      {paths[view]}
    </svg>
  );
}

export default function Shell({
  view,
  onNavigate,
  email,
  clearance,
  onSignOut,
  children,
}: {
  view: View;
  onNavigate: (view: View) => void;
  email: string | null;
  clearance: string | null;
  onSignOut: () => void;
  children: ReactNode;
}) {
  const initial = (email?.trim().charAt(0) || "?").toUpperCase();
  return (
    <div className="shell">
      <aside className="shell-nav">
        <div className="shell-brand">
          <div className="shell-mark" aria-hidden="true">
            <span />
          </div>
          <div className="shell-brand-text">
            <span className="shell-title">Cockpit</span>
            <span className="shell-sub">Firm OS · L5</span>
          </div>
        </div>

        <nav className="shell-links" aria-label="Primary">
          {NAV.map((item) => (
            <button
              key={item.key}
              type="button"
              className={`shell-link${view === item.key ? " is-active" : ""}`}
              aria-current={view === item.key ? "page" : undefined}
              onClick={() => onNavigate(item.key)}
            >
              <span className="shell-link-icon" aria-hidden="true">
                <NavIcon view={item.key} />
              </span>
              <span className="shell-link-label">{item.label}</span>
            </button>
          ))}
        </nav>

        <div className="shell-user">
          <div className="shell-avatar" aria-hidden="true">
            {initial}
          </div>
          <div className="shell-user-id">
            <span className="shell-user-email">{email ?? "…"}</span>
            {clearance && (
              <span className="shell-user-clearance">{clearance}</span>
            )}
          </div>
          <button
            type="button"
            className="shell-signout"
            onClick={onSignOut}
            aria-label="Sign out"
            title="Sign out"
          >
            ⏻
          </button>
        </div>
      </aside>

      <main className="shell-main">{children}</main>
    </div>
  );
}
