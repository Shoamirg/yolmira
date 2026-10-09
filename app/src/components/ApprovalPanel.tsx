import { useShallow } from "zustand/react/shallow";
import type { PendingAsk } from "../api";
import { useStore } from "../store";

/** A loosening edit of policies.toml / agents.toml, waiting for confirmation. */
const isConfig = (a: PendingAsk) => a.action.startsWith("config.");

/** What a request would do, in a line a person can check. */
function describe(a: PendingAsk): string {
  const d = a.detail as Record<string, unknown>;
  switch (a.action) {
    case "session.kill":
      return `End the ${String(d.shell ?? "shell")} shell ${String(d.id ?? "").slice(-6)}${d.cwd ? ` in ${String(d.cwd)}` : ""}`;
    case "session.create":
      return `Start a ${String((d.shell as Record<string, unknown> | undefined)?.kind ?? "")} shell${d.project ? ` in project ${String(d.project)}` : ""}`;
    case "daemon.stop":
      return `Stop the Yolmira daemon (ends ${String(d.running_shells ?? "every")} running shell(s))`;
    case "project.register":
      return `Register project ${String(d.name)} at ${String(d.path)}`;
    case "project.remove":
      return `Forget project ${String(d.name)} (its folder is untouched)`;
    case "config.policies":
    case "config.agents":
      return `Confirm an edit of ${String(d.file)} that loosens: ${((d.loosenings as string[] | undefined) ?? []).join("; ")}`;
    default:
      return `${a.action} ${JSON.stringify(a.detail)}`;
  }
}

/**
 * Requests the policy holds until someone answers (ARCHITECTURE §6.3). Only this
 * window can answer without a code; the code shown lets a person approve from the
 * CLI (`yolmira approve <id> --code <code>`). Agents can do neither (decisions.md
 * 2026-10-09).
 */
export function ApprovalPanel() {
  const { asks, codes, answer } = useStore(
    useShallow((s) => ({ asks: s.asks, codes: s.askCodes, answer: s.answerAsk })),
  );
  if (asks.length === 0) return null;
  return (
    <section className="approvals" aria-labelledby="approvals-heading" data-testid="approvals">
      <h2 id="approvals-heading" className="approvals-title">
        {asks.length === 1 ? "A request waits for your approval" : `${asks.length} requests wait for your approval`}
      </h2>
      <ul>
        {asks.map((a) => (
          <li key={a.id} className="approval" data-testid="approval" data-ask-id={a.id} data-action={a.action}>
            <div className="approval-what">{describe(a)}</div>
            {isConfig(a) && (
              <pre className="approval-diff" data-testid="approval-diff">
                {String((a.detail as Record<string, unknown>).diff ?? "")}
              </pre>
            )}
            <div className="approval-meta">
              asked by {a.client}
              {a.project ? ` · project ${a.project}` : ""} · rule: {a.rule}
            </div>
            <div className="approval-code">
              CLI code <span className="mono" data-testid="approval-code">{codes[a.id] ?? "…"}</span>
            </div>
            <div className="approval-actions">
              <button type="button" className="btn" data-testid="approve-once" onClick={() => void answer(a.id, true, false)}>
                {isConfig(a) ? "Confirm the edit" : "Allow once"}
              </button>
              {!isConfig(a) && (
                <button type="button" className="btn btn-quiet" data-testid="approve-session" onClick={() => void answer(a.id, true, true)}>
                  Allow until the daemon stops
                </button>
              )}
              <button type="button" className="btn btn-quiet btn-danger" data-testid="deny" onClick={() => void answer(a.id, false, false)}>
                {isConfig(a) ? "Decline (keep the confirmed version)" : "Deny"}
              </button>
            </div>
          </li>
        ))}
      </ul>
    </section>
  );
}
