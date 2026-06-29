// Root error boundary for the Cockpit webview.
//
// A render/lifecycle throw in any panel (e.g. a malformed payload hitting a
// `JSON.parse`, or an unexpected `undefined` access) would otherwise white-screen
// the entire webview. This catches it and degrades to a recoverable card with the
// error message + Dismiss/Reload affordances, so one panel's crash never takes the
// whole desktop down. React only surfaces render errors through a class component's
// `getDerivedStateFromError`/`componentDidCatch`, so this is deliberately a class.
import { Component, type ErrorInfo, type ReactNode } from "react";

type Props = { children: ReactNode };
type State = { error: Error | null };

export default class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // The webview console is the dev signal; the host forwards it to the log.
    console.error("Cockpit panel crashed:", error, info.componentStack);
  }

  private dismiss = () => this.setState({ error: null });

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    return (
      <main className="splash">
        <section className="card">
          <h1>Something went wrong</h1>
          <p className="subtitle">A panel hit an unexpected error.</p>
          <div className="status is-error" role="alert">
            <span className="dot" />
            <span className="status-detail">{error.message}</span>
          </div>
          <div className="error-actions">
            <button className="btn-secondary" onClick={this.dismiss}>
              Dismiss
            </button>
            <button
              className="btn-primary"
              onClick={() => window.location.reload()}
            >
              Reload
            </button>
          </div>
        </section>
      </main>
    );
  }
}
