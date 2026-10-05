import { memo, useEffect, useRef } from "react";
import "@xterm/xterm/css/xterm.css";
import { api } from "../api";
import { useStore } from "../store";
import { ensureTerminal, queueInput } from "../terminals";

const RESIZE_DEBOUNCE_MS = 120;

type Props = {
  id: string;
  visible: boolean;
};

/** Hosts one session's xterm. Hidden panes stay mounted, so their screens survive tab switches. */
export const TerminalPane = memo(function TerminalPane({ id, visible }: Props) {
  const host = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = host.current;
    if (!el) return;
    const { term, fit } = ensureTerminal(id);
    if (!term.element) term.open(el);
    else el.appendChild(term.element);

    const showError = useStore.getState().showError;
    const onData = term.onData((d) => {
      // Only a live feed takes input; otherwise the pane's note says why it doesn't.
      if (useStore.getState().tabs.find((t) => t.id === id)?.feed !== "live") return;
      queueInput(id, d, (c) => api.sessionInput(id, c)).catch((e) => showError(e, "Send input to the shell"));
    });
    let timer: number | undefined;
    const onResize = term.onResize(({ cols, rows }) => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        const tab = useStore.getState().tabs.find((t) => t.id === id);
        if (tab?.feed === "live") api.sessionResize(id, cols, rows).catch((e) => showError(e, "Resize the shell"));
      }, RESIZE_DEBOUNCE_MS);
    });
    const observer = new ResizeObserver(() => {
      if (el.offsetWidth > 0 && el.offsetHeight > 0) fit.fit();
    });
    observer.observe(el);
    return () => {
      window.clearTimeout(timer);
      observer.disconnect();
      onData.dispose();
      onResize.dispose();
    };
  }, [id]);

  useEffect(() => {
    if (!visible) return;
    const t = ensureTerminal(id);
    t.fit.fit();
    // Not while the user moves through the tab bar with the arrow keys: those would
    // reach the shell. Enter on a tab moves focus into its terminal.
    if (!document.activeElement?.closest('[role="tablist"]')) t.term.focus();
  }, [id, visible]);

  return <div className="terminal-host" data-testid="terminal" data-session-id={id} ref={host} />;
});
