// The webview's only channel to AMIRI: Tauri commands and events (ARCHITECTURE §6.5).
// Shapes mirror crates/amiri-ipc/src/protocol.rs and app/src-tauri/src/daemon.rs.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type AmiriError = {
  what: string;
  where: string;
  why: string;
  attempted: string[];
  next_steps: string[];
};

export type DaemonState = {
  connected: boolean;
  pid: number | null;
  version: string | null;
  instance: string | null;
  error: AmiriError | null;
};

export type SessionStatus = "RUNNING" | "EXITED" | "UNKNOWN";

export type SessionInfo = {
  id: string;
  shell: string;
  cwd: string;
  pid: number | null;
  status: SessionStatus;
  exit_code: number | null;
  attached: number;
  cols: number;
  rows: number;
};

/** A session list and the pid of the daemon process that answered it. */
export type SessionSnapshot = { daemon_pid: number; sessions: SessionInfo[] };

export type ShellKind = "cmd" | "powershell" | "pwsh" | "wsl";

/**
 * Feed events carry the generation of the attachment they belong to. Output also
 * carries a flow-control credit, returned with `sessionAck` once xterm has written it.
 */
export type SessionOutput = { id: string; data: string; reset: boolean; generation: number; credit: number };
export type SessionExited = { id: string; exit_code: number | null; generation: number };
export type FeedClosed = { id: string; why: string; generation: number };
export type DaemonEvent = { seq: number; kind: string; session_id: string | null };

export const api = {
  daemonState: () => invoke<DaemonState>("daemon_state"),
  daemonStart: () => invoke<void>("daemon_start"),
  sessionList: () => invoke<SessionSnapshot>("session_list"),
  sessionCreate: (kind: ShellKind, cols: number, rows: number) =>
    invoke<SessionInfo>("session_create", { kind, cols, rows }),
  sessionAttach: (id: string, generation: number) => invoke<unknown>("session_attach", { id, generation }),
  sessionAck: (id: string, generation: number, credit: number) =>
    invoke<void>("session_ack", { id, generation, credit }),
  sessionDetach: (id: string) => invoke<void>("session_detach", { id }),
  sessionInput: (id: string, data: string) => invoke<void>("session_input", { id, data }),
  sessionResize: (id: string, cols: number, rows: number) =>
    invoke<void>("session_resize", { id, cols, rows }),
  sessionKill: (id: string) => invoke<void>("session_kill", { id }),
};

export type Handlers = {
  daemonState: (s: DaemonState) => void;
  daemonEvent: (e: DaemonEvent) => void;
  output: (o: SessionOutput) => void;
  exited: (e: SessionExited) => void;
  feedClosed: (f: FeedClosed) => void;
};

export async function subscribe(h: Handlers): Promise<UnlistenFn> {
  const offs = await Promise.all([
    listen<DaemonState>("daemon-state", (e) => h.daemonState(e.payload)),
    listen<DaemonEvent>("daemon-event", (e) => h.daemonEvent(e.payload)),
    listen<SessionOutput>("session-output", (e) => h.output(e.payload)),
    listen<SessionExited>("session-exited", (e) => h.exited(e.payload)),
    listen<FeedClosed>("session-feed-closed", (e) => h.feedClosed(e.payload)),
  ]);
  return () => offs.forEach((off) => off());
}

/** Commands reject with an AmiriError; anything else is wrapped into one. */
export function asAmiriError(e: unknown, what: string): AmiriError {
  if (e && typeof e === "object" && "why" in e && "what" in e) return e as AmiriError;
  return { what, where: "ui", why: String(e), attempted: [], next_steps: ["Run `amiri doctor`."] };
}
