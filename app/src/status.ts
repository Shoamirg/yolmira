// What a session row may claim. Decided at render time from facts, never from a flag
// that could be set out of order: a status is shown only from a list fetched in the
// current connection epoch, from the daemon process that is connected now.
import type { SessionInfo, SessionStatus } from "./api";

/** Connected, but no list from this daemon yet: nothing is claimed. */
export type ShownStatus = SessionStatus | "RECONNECTING";

/** The connection epoch and daemon pid a session list was fetched in. */
export type Snapshot = { epoch: number; pid: number };

export type StatusView = {
  connected: boolean;
  /** The pid of the daemon the UI is connected to now. */
  pid: number | null;
  /** Bumped on every connect, disconnect or daemon change. */
  epoch: number;
  snapshot: Snapshot | null;
};

export function snapshotIsCurrent(v: StatusView): boolean {
  return v.connected && v.snapshot !== null && v.snapshot.epoch === v.epoch && v.snapshot.pid === v.pid;
}

export function shownStatus(s: SessionInfo, v: StatusView): ShownStatus {
  if (!v.connected) return "UNKNOWN";
  return snapshotIsCurrent(v) ? s.status : "RECONNECTING";
}
