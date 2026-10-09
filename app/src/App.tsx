import { useEffect } from "react";
import { useShallow } from "zustand/react/shallow";
import { api, subscribe } from "./api";
import { ApprovalPanel } from "./components/ApprovalPanel";
import { Banner } from "./components/Banner";
import { SessionList } from "./components/SessionList";
import { Tabs } from "./components/Tabs";
import { useStore } from "./store";

const REFRESH_DEBOUNCE_MS = 150;

function useDaemonLink() {
  useEffect(() => {
    let cancelled = false;
    let refresh: number | undefined;
    let sawState = false;
    const store = useStore.getState;
    const scheduleRefresh = () => {
      window.clearTimeout(refresh);
      refresh = window.setTimeout(() => void store().refreshSessions(), REFRESH_DEBOUNCE_MS);
    };
    // Handlers check `cancelled`, so a second listener set (StrictMode's double mount
    // in development) never handles an event twice.
    const unsubscribe = subscribe({
      daemonState: (d) => {
        sawState = true;
        if (!cancelled) store().setDaemon(d);
      },
      daemonEvent: (e) => {
        if (cancelled) return;
        if (e.kind.startsWith("session.")) scheduleRefresh();
        if (e.kind.startsWith("policy.")) void store().refreshAsks();
      },
      output: (o) => !cancelled && store().onOutput(o),
      exited: (e) => !cancelled && store().onExited(e),
      feedClosed: (f) => !cancelled && store().onFeedClosed(f),
    });
    // The state may have changed before the listeners were in place; a state event
    // that arrived since wins over this snapshot.
    unsubscribe
      .then(() => api.daemonState())
      .then((d) => {
        if (!cancelled && !sawState) store().setDaemon(d);
      })
      .catch((e) => store().showError(e, "Connect the UI to its backend"));
    return () => {
      cancelled = true;
      window.clearTimeout(refresh);
      void unsubscribe.then((off) => off()).catch(() => {});
    };
  }, []);
}

function ErrorPanel() {
  const { error, dismissError } = useStore(useShallow((s) => ({ error: s.error, dismissError: s.dismissError })));
  if (!error) return null;
  return (
    <div className="error-panel" role="alert" data-testid="error">
      <div>
        <strong>{error.what}</strong> failed: {error.why}
        {error.next_steps.length > 0 && (
          <ul>
            {error.next_steps.map((s) => (
              <li key={s}>{s}</li>
            ))}
          </ul>
        )}
      </div>
      <button type="button" className="btn btn-quiet" onClick={dismissError}>
        Dismiss
      </button>
    </div>
  );
}

export function App() {
  useDaemonLink();
  return (
    <div className="shell-app">
      <SessionList />
      <div className="main-column">
        <Banner />
        <ApprovalPanel />
        <ErrorPanel />
        <Tabs />
      </div>
    </div>
  );
}
