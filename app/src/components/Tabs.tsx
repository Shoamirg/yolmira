import type { KeyboardEvent } from "react";
import { useShallow } from "zustand/react/shallow";
import { useStore } from "../store";
import { TerminalPane } from "./TerminalPane";

const FEED_LABEL = { attaching: "attaching", live: "live", closed: "detached", exited: "exited" } as const;

export function Tabs() {
  const { tabs, active, sessions, setActive, closeTab } = useStore(
    useShallow((s) => ({ tabs: s.tabs, active: s.active, sessions: s.sessions, setActive: s.setActive, closeTab: s.closeTab })),
  );
  if (tabs.length === 0) {
    return (
      <main className="workspace empty-workspace">
        <p>Open a shell on the left, or attach to one that's already running.</p>
        <p className="dim">Closing this window leaves every shell running in the daemon.</p>
      </main>
    );
  }
  const name = (id: string) => `${sessions.find((s) => s.id === id)?.shell ?? "shell"} ${id.slice(-6)}`;
  // Arrow keys move between tabs (WAI-ARIA tabs pattern).
  const onKeyDown = (e: KeyboardEvent, index: number) => {
    const step = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    if (!step) return;
    const next = tabs[(index + step + tabs.length) % tabs.length];
    if (next) {
      setActive(next.id);
      document.getElementById(`tab-${next.id}`)?.focus();
    }
  };
  return (
    <main className="workspace">
      <div className="tab-bar" role="tablist" aria-label="Attached shells">
        {tabs.map((t, i) => (
          <div key={t.id} className="tab" data-testid="tab" data-session-id={t.id} data-feed={t.feed} data-selected={t.id === active}>
            <button
              type="button"
              id={`tab-${t.id}`}
              role="tab"
              aria-selected={t.id === active}
              aria-controls={`pane-${t.id}`}
              tabIndex={t.id === active ? 0 : -1}
              className="tab-label"
              onClick={() => setActive(t.id)}
              onKeyDown={(e) => onKeyDown(e, i)}
            >
              <span className="feed-dot" data-feed={t.feed} aria-hidden="true" />
              {name(t.id)}
              <span className="sr-only"> ({FEED_LABEL[t.feed]})</span>
            </button>
            <button
              type="button"
              className="tab-close"
              title="Detach (the shell keeps running)"
              aria-label={`Detach ${name(t.id)} (it keeps running)`}
              onClick={() => void closeTab(t.id)}
              data-testid="detach"
            >
              ×
            </button>
          </div>
        ))}
      </div>
      <div className="panes">
        {tabs.map((t) => (
          <div key={t.id} id={`pane-${t.id}`} className="pane" role="tabpanel" aria-labelledby={`tab-${t.id}`} hidden={t.id !== active}>
            {t.feed !== "live" && <div className="pane-note">{t.note ?? FEED_LABEL[t.feed]}</div>}
            <TerminalPane id={t.id} visible={t.id === active} />
          </div>
        ))}
      </div>
    </main>
  );
}
