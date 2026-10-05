// One xterm per attached session, kept outside React so switching tabs never loses
// a screen. Output can arrive before the pane is mounted; xterm buffers it.
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";

type Entry = {
  term: Terminal;
  fit: FitAddon;
  /** The tail of this session's input chain; it never rejects (see queueInput). */
  inputTail: Promise<void>;
};

const entries = new Map<string, Entry>();

/** Paste chunk size in UTF-16 units, under the UI command's 64 KiB input limit. */
const INPUT_CHUNK = 16 * 1024;
/** RIS: a full terminal reset, ordered with the writes around it. */
const RESET = "\x1bc";

export const DEFAULT_COLS = 120;
export const DEFAULT_ROWS = 30;

function decodeBase64(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export function ensureTerminal(id: string): Entry {
  const found = entries.get(id);
  if (found) return found;
  const term = new Terminal({
    cols: DEFAULT_COLS,
    rows: DEFAULT_ROWS,
    cursorBlink: true,
    scrollback: 5000,
    fontFamily: '"Cascadia Mono", "Cascadia Code", Consolas, monospace',
    fontSize: 14,
    theme: {
      background: "#111412",
      foreground: "#e6e1d6",
      cursor: "#e8a33d",
      selectionBackground: "#e8a33d44",
    },
    // Hyperlinks printed by programs (OSC 8) are never opened: the webview shows only
    // the app's own pages (security review, Slice C).
    linkHandler: { activate: () => {}, allowNonHttpProtocols: false },
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  const entry = { term, fit, inputTail: Promise.resolve() };
  entries.set(id, entry);
  return entry;
}

export function getTerminal(id: string): Entry | undefined {
  return entries.get(id);
}

export function writeOutput(id: string, data: string, reset: boolean): void {
  const e = entries.get(id);
  if (!e) return;
  if (reset) e.term.write(RESET);
  e.term.write(decodeBase64(data));
}

export function disposeTerminal(id: string): void {
  entries.get(id)?.term.dispose();
  entries.delete(id);
}

/** Split without cutting a surrogate pair in half. */
function chunks(data: string): string[] {
  const out: string[] = [];
  let i = 0;
  while (i < data.length) {
    let end = Math.min(i + INPUT_CHUNK, data.length);
    const last = data.charCodeAt(end - 1);
    if (end < data.length && last >= 0xd800 && last <= 0xdbff) end -= 1;
    out.push(data.slice(i, end));
    i = end;
  }
  return out;
}

/**
 * Send input strictly in order, one command at a time per session. A failed send
 * rejects only its own promise; later input still goes out.
 */
export function queueInput(id: string, data: string, send: (chunk: string) => Promise<void>): Promise<void> {
  const e = entries.get(id);
  if (!e) return Promise.resolve();
  const mine = e.inputTail.then(async () => {
    for (const c of chunks(data)) await send(c);
  });
  e.inputTail = mine.catch(() => {});
  return mine;
}
