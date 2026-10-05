import { useShallow } from "zustand/react/shallow";
import type { ShellKind } from "../api";
import { shownStatus, useStore } from "../store";

const NEW_KINDS: { kind: ShellKind; label: string }[] = [
  { kind: "cmd", label: "cmd" },
  { kind: "powershell", label: "PowerShell" },
  { kind: "pwsh", label: "pwsh" },
  { kind: "wsl", label: "WSL" },
];

const shortId = (id: string) => id.slice(-6);

export function SessionList() {
  const { sessions, known, daemon, tabs, openNew, attach, kill } = useStore(
    useShallow((s) => ({
      sessions: s.sessions,
      known: s.daemon.connected && s.sessionsFresh,
      daemon: s.daemon,
      tabs: s.tabs,
      openNew: s.openNew,
      attach: s.attach,
      kill: s.kill,
    })),
  );
  const open = new Set(tabs.map((t) => t.id));
  return (
    <aside className="sidebar">
      <header className="sidebar-head">
        <span className="wordmark">amiri</span>
        <span className="daemon-chip" data-connected={daemon.connected}>
          {daemon.connected ? `amirid ${daemon.version ?? ""} · pid ${daemon.pid ?? "?"}` : "offline"}
        </span>
      </header>

      <section aria-labelledby="new-heading" className="new-shells">
        <h2 id="new-heading" className="eyebrow">New shell</h2>
        <div className="new-grid">
          {NEW_KINDS.map(({ kind, label }) => (
            <button type="button" key={kind} className="btn" disabled={!daemon.connected} onClick={() => void openNew(kind)} data-testid={`new-${kind}`}>
              {label}
            </button>
          ))}
        </div>
      </section>

      <section aria-labelledby="sessions-heading" className="sessions">
        <h2 id="sessions-heading" className="eyebrow">Shells in the daemon</h2>
        {sessions.length === 0 && <p className="empty">No shells yet.</p>}
        <ul>
          {sessions.map((s) => {
            const status = shownStatus(s, known);
            return (
              <li key={s.id} className="session-row" data-testid="session-row" data-session-id={s.id} data-session-status={status}>
                <span className="status" data-status={status}>{status}</span>
                <span className="session-name">
                  {s.shell} <span className="mono dim">{shortId(s.id)}</span>
                </span>
                <span className="row-actions">
                  {status === "RUNNING" && !open.has(s.id) && (
                    <button type="button" className="btn btn-quiet" onClick={() => void attach(s.id)} data-testid="attach">Attach</button>
                  )}
                  {status === "RUNNING" && (
                    <button type="button" className="btn btn-quiet btn-danger" onClick={() => void kill(s.id)} data-testid="kill">Kill</button>
                  )}
                  {status === "EXITED" && <span className="mono dim">exit {s.exit_code ?? "?"}</span>}
                </span>
              </li>
            );
          })}
        </ul>
      </section>
    </aside>
  );
}
