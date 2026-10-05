import { create } from "zustand";
import {
  api,
  asAmiriError,
  type AmiriError,
  type DaemonState,
  type FeedClosed,
  type SessionExited,
  type SessionInfo,
  type SessionOutput,
  type ShellKind,
} from "./api";
import { snapshotIsCurrent, type Snapshot, type StatusView } from "./status";
import { DEFAULT_COLS, DEFAULT_ROWS, disposeTerminal, ensureTerminal, getTerminal, writeOutput } from "./terminals";

export type FeedState = "attaching" | "live" | "closed" | "exited";
/** `generation` identifies the current attachment; events from older ones are dropped. */
export type Tab = { id: string; feed: FeedState; note: string | null; generation: number };

type Store = {
  daemon: DaemonState;
  sessions: SessionInfo[];
  /** Bumped on every connect, disconnect or daemon change. */
  epoch: number;
  /** Where `sessions` came from; statuses show only while it matches (status.ts). */
  snapshot: Snapshot | null;
  tabs: Tab[];
  active: string | null;
  error: AmiriError | null;

  setDaemon: (d: DaemonState) => void;
  resume: () => Promise<void>;
  refreshSessions: () => Promise<void>;
  openNew: (kind: ShellKind) => Promise<void>;
  attach: (id: string, opts?: { focus: boolean }) => Promise<void>;
  closeTab: (id: string) => Promise<void>;
  kill: (id: string) => Promise<void>;
  startDaemon: () => Promise<void>;
  setActive: (id: string) => void;
  onOutput: (o: SessionOutput) => void;
  onExited: (e: SessionExited) => void;
  onFeedClosed: (f: FeedClosed) => void;
  showError: (e: unknown, what: string) => void;
  dismissError: () => void;
};

const OFFLINE: DaemonState = { connected: false, pid: null, version: null, instance: null, error: null };

/** The facts `shownStatus` decides from. */
export const statusView = (s: { daemon: DaemonState; epoch: number; snapshot: Snapshot | null }): StatusView => ({
  connected: s.daemon.connected,
  pid: s.daemon.pid,
  epoch: s.epoch,
  snapshot: s.snapshot,
});

let nextGeneration = 1;
/** The latest list request; an older one that returns later is dropped. */
let listSeq = 0;

const sameDaemon = (a: DaemonState, b: DaemonState) =>
  a.connected === b.connected && a.pid === b.pid && a.instance === b.instance;

export const useStore = create<Store>((set, get) => {
  const fail = (what: string) => (e: unknown) => get().showError(e, what);
  const tab = (id: string) => get().tabs.find((t) => t.id === id);
  const updateTab = (id: string, patch: Partial<Tab>) =>
    set((s) => ({ tabs: s.tabs.map((t) => (t.id === id ? { ...t, ...patch } : t)) }));
  /** The event belongs to the tab's current attachment. */
  const current = (id: string, generation: number) => tab(id)?.generation === generation;

  return {
    daemon: OFFLINE,
    sessions: [],
    epoch: 0,
    snapshot: null,
    tabs: [],
    active: null,
    error: null,

    // A different daemon (even without a disconnect in between) counts as one: its
    // feeds are gone and its list must be reloaded before anything shows RUNNING.
    setDaemon: (d) => {
      const prev = get().daemon;
      if (sameDaemon(prev, d)) return set({ daemon: d });
      set((s) => ({
        daemon: d,
        epoch: s.epoch + 1,
        tabs: prev.connected
          ? s.tabs.map((t) => (t.feed === "exited" ? t : { ...t, feed: "closed", note: "daemon unreachable" }))
          : s.tabs,
      }));
      if (d.connected) void get().resume();
    },

    // Reconnected: reload the list, then reattach the tabs whose shell still runs
    // (the replay restores their screens). A daemon restart ends local shells (D3).
    resume: async () => {
      const epoch = get().epoch;
      await get().refreshSessions();
      if (get().epoch !== epoch || !snapshotIsCurrent(statusView(get()))) return;
      const running = new Set(get().sessions.filter((s) => s.status === "RUNNING").map((s) => s.id));
      for (const t of get().tabs.filter((t) => t.feed === "closed")) {
        if (running.has(t.id)) void get().attach(t.id, { focus: false });
        else updateTab(t.id, { feed: "exited", note: "the shell ended while the daemon was unreachable" });
      }
    },

    refreshSessions: async () => {
      if (!get().daemon.connected) return;
      const epoch = get().epoch;
      const seq = ++listSeq;
      try {
        const { daemon_pid, sessions } = await api.sessionList();
        // Out-of-date answers are dropped here; and should one get through, it still
        // can't show a status unless its epoch and daemon pid match (status.ts).
        if (seq === listSeq && epoch === get().epoch) set({ sessions, snapshot: { epoch, pid: daemon_pid } });
      } catch (e) {
        fail("List shells")(e);
      }
    },

    openNew: async (kind) => {
      try {
        const info = await api.sessionCreate(kind, DEFAULT_COLS, DEFAULT_ROWS);
        set((s) => ({ sessions: [info, ...s.sessions.filter((x) => x.id !== info.id)] }));
        await get().attach(info.id);
      } catch (e) {
        fail("Open a shell")(e);
      }
    },

    attach: async (id, opts = { focus: true }) => {
      const generation = nextGeneration++;
      ensureTerminal(id);
      set((s) => ({
        tabs: tab(id)
          ? s.tabs.map((t) => (t.id === id ? { ...t, feed: "attaching", note: null, generation } : t))
          : [...s.tabs, { id, feed: "attaching", note: null, generation }],
        active: opts.focus || !s.active ? id : s.active,
      }));
      try {
        await api.sessionAttach(id, generation);
      } catch (e) {
        // A newer attach, a detach or a disconnect replaced this one: not an error.
        if (!current(id, generation) || tab(id)?.feed !== "attaching") return;
        updateTab(id, { feed: "closed", note: "attach failed" });
        fail("Attach to the shell")(e);
        return;
      }
      // Events may have arrived first (the shell exited, the feed closed): keep those.
      if (!current(id, generation) || tab(id)?.feed !== "attaching") return;
      updateTab(id, { feed: "live" });
      const t = getTerminal(id)?.term;
      if (t) await api.sessionResize(id, t.cols, t.rows).catch(fail("Resize the shell"));
    },

    // Closing a tab only detaches: the shell keeps running in the daemon. The tab goes
    // first, so an attach still in flight finds itself replaced and backs out.
    closeTab: async (id) => {
      set((s) => {
        const tabs = s.tabs.filter((t) => t.id !== id);
        const active = s.active === id ? (tabs[tabs.length - 1]?.id ?? null) : s.active;
        return { tabs, active };
      });
      disposeTerminal(id);
      // While the daemon is down there is no feed to detach.
      if (get().daemon.connected) await api.sessionDetach(id).catch(fail("Detach from the shell"));
    },

    kill: async (id) => {
      await api.sessionKill(id).catch(fail("Close the shell"));
    },

    startDaemon: async () => {
      await api.daemonStart().catch(fail("Start the AMIRI daemon"));
    },

    setActive: (id) => set({ active: id }),

    onOutput: (o) => {
      if (!current(o.id, o.generation)) return;
      // An ack can only fail when the feed is already gone; nothing is waiting for it then.
      writeOutput(o.id, o.data, o.reset, () => void api.sessionAck(o.id, o.generation, o.credit).catch(() => {}));
    },
    onExited: (e) => {
      if (current(e.id, e.generation)) updateTab(e.id, { feed: "exited", note: `exited with code ${e.exit_code ?? "?"}` });
    },
    onFeedClosed: (f) => {
      const t = tab(f.id);
      if (t && t.generation === f.generation && (t.feed === "live" || t.feed === "attaching")) {
        updateTab(f.id, { feed: "closed", note: f.why });
      }
    },

    showError: (e, what) => set({ error: asAmiriError(e, what) }),
    dismissError: () => set({ error: null }),
  };
});
