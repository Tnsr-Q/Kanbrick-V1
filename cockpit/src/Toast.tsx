// Global toast notifications. A single <ToastProvider> near the root exposes a
// stable `useToast()` api (success / error / info) to every view, and renders the
// stacked, auto-dismissing viewport itself. Replaces the per-view inline error
// lines and Messenger's bespoke popup with one consistent, accessible surface.
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";

export type ToastKind = "success" | "error" | "info";

type Toast = { id: number; kind: ToastKind; message: string };

type ToastApi = {
  /** Show a toast of an explicit kind. */
  push: (kind: ToastKind, message: string) => void;
  success: (message: string) => void;
  error: (message: string) => void;
  info: (message: string) => void;
  /** Dismiss one toast early by id. */
  dismiss: (id: number) => void;
};

const ToastContext = createContext<ToastApi | null>(null);

/** Auto-dismiss delay per kind — errors linger longer so they aren't missed. */
const TTL: Record<ToastKind, number> = {
  success: 4000,
  info: 4500,
  error: 7000,
};

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const nextId = useRef(0);
  const timers = useRef<Map<number, ReturnType<typeof setTimeout>>>(new Map());

  const dismiss = useCallback((id: number) => {
    setToasts((ts) => ts.filter((t) => t.id !== id));
    const timer = timers.current.get(id);
    if (timer) {
      clearTimeout(timer);
      timers.current.delete(id);
    }
  }, []);

  const push = useCallback(
    (kind: ToastKind, message: string) => {
      const id = (nextId.current += 1);
      setToasts((ts) => [...ts, { id, kind, message }]);
      timers.current.set(
        id,
        setTimeout(() => dismiss(id), TTL[kind]),
      );
    },
    [dismiss],
  );

  // Clear any pending timers if the provider itself ever unmounts.
  useEffect(() => {
    const pending = timers.current;
    return () => {
      for (const timer of pending.values()) clearTimeout(timer);
      pending.clear();
    };
  }, []);

  const api = useMemo<ToastApi>(
    () => ({
      push,
      success: (m) => push("success", m),
      error: (m) => push("error", m),
      info: (m) => push("info", m),
      dismiss,
    }),
    [push, dismiss],
  );

  return (
    <ToastContext.Provider value={api}>
      {children}
      <ToastViewport toasts={toasts} onDismiss={dismiss} />
    </ToastContext.Provider>
  );
}

/** Access the toast api. Throws if used outside a {@link ToastProvider}. */
export function useToast(): ToastApi {
  const ctx = useContext(ToastContext);
  if (!ctx) throw new Error("useToast must be used within a <ToastProvider>");
  return ctx;
}

/** Glyph per kind (decorative — the role attr carries the semantics). */
const ICON: Record<ToastKind, string> = {
  success: "✓",
  error: "!",
  info: "i",
};

function ToastViewport({
  toasts,
  onDismiss,
}: {
  toasts: Toast[];
  onDismiss: (id: number) => void;
}) {
  if (toasts.length === 0) return null;
  return (
    <div className="toast-viewport" role="region" aria-label="Notifications">
      {toasts.map((t) => (
        <button
          key={t.id}
          type="button"
          className={`toast toast-${t.kind}`}
          role={t.kind === "error" ? "alert" : "status"}
          onClick={() => onDismiss(t.id)}
          title="Dismiss"
        >
          <span className="toast-icon" aria-hidden="true">
            {ICON[t.kind]}
          </span>
          <span className="toast-msg">{t.message}</span>
        </button>
      ))}
    </div>
  );
}
