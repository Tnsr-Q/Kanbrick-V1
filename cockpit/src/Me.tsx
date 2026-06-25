import { useEffect, useState } from "react";
import { me, type Identity } from "./api";

/** L1..L5 → human label (from the firm's five-tier clearance model). */
const CLEARANCE_LABEL: Record<string, string> = {
  L1: "Support",
  L2: "Execution",
  L3: "Operational",
  L4: "Strategic",
  L5: "Admin",
};

/**
 * P7.5 — the `/me` identity panel: the visible proof of the thin end-to-end path
 * (login → sidecar → auth bridge → identity). Fetches identity through the host
 * `me` command (ADR-0016); the webview never sees the token.
 */
export default function Me({ onSignOut }: { onSignOut: () => void }) {
  const [identity, setIdentity] = useState<Identity | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    me()
      .then((id) => active && setIdentity(id))
      .catch(
        (e) =>
          active &&
          setError(typeof e === "string" ? e : "could not load identity"),
      );
    return () => {
      active = false;
    };
  }, []);

  if (error) {
    return (
      <div className="panel">
        <div className="status is-error" role="alert">
          <span className="dot" />
          <span>{error}</span>
        </div>
        <button className="btn-secondary" onClick={onSignOut}>
          Back to sign in
        </button>
      </div>
    );
  }

  if (!identity) {
    return (
      <div className="status is-pending" role="status">
        <span className="dot" />
        <span>Loading identity…</span>
      </div>
    );
  }

  const level = identity.clearance;
  const label = CLEARANCE_LABEL[level] ?? "";
  const initial = identity.email.trim().charAt(0).toUpperCase() || "?";

  return (
    <div className="me">
      <div className="me-head">
        <div className="avatar" aria-hidden="true">
          {initial}
        </div>
        <div className="me-id">
          <div className="me-email">{identity.email}</div>
          <div className={`badge badge-${level.toLowerCase()}`}>
            <span className="badge-level">{level}</span>
            {label && <span className="badge-label">{label}</span>}
          </div>
        </div>
      </div>

      {identity.roles.length > 0 && (
        <div className="chips">
          {identity.roles.map((role) => (
            <span className="chip" key={role}>
              {role}
            </span>
          ))}
        </div>
      )}

      <div className="me-firm">Firm · Kanbrick (V1 — per-company scope in P14)</div>

      <button className="btn-secondary" onClick={onSignOut}>
        Sign out
      </button>
    </div>
  );
}
