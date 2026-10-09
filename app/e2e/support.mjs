// E2E support: an isolated daemon instance per test, the real UI exe, and a W3C
// WebDriver client (Node's fetch) attached to the UI's WebView2 through its
// debugging port. The harness launches the UI itself, so it can close it normally,
// kill it, or start it from a console window it then closes. Assertions use only
// locale-neutral values: markers the shell computes, numbers, exit codes, pids.
// Screens are read from xterm's DOM rows: the text the user sees, no test hooks.
import { execFile, execFileSync, spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { randomUUID } from "node:crypto";

const HERE = dirname(fileURLToPath(import.meta.url));
export const ROOT = resolve(HERE, "..", "..");
export const APP_EXE = join(ROOT, "target", "debug", "amiri-app.exe");
export const CLI_EXE = join(ROOT, "target", "debug", "amiri.exe");
const DAEMON_EXE = join(ROOT, "target", "debug", "amirid.exe");
/** WebDriver's Enter key (W3C key code point U+E007). */
const KEY_ENTER = "\uE007";
/** Where off-screen test windows open: beyond any monitor, but not minimized. */
const OFFSCREEN = "-4000,-4000";
/** How much of a UI process's own output the diagnostics keep. */
const OUTPUT_TAIL = 4000;
/** Upper bound for one WebDriver or devtools HTTP request. */
const HTTP_TIMEOUT_MS = 30_000;
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Poll `fn` until it returns a truthy value; fail with `what` and the last value. */
export async function waitFor(what, fn, timeoutMs = 30_000, everyMs = 150) {
  const end = Date.now() + timeoutMs;
  let last;
  for (;;) {
    try {
      last = await fn();
      if (last) return last;
    } catch (e) {
      last = e;
    }
    if (Date.now() > end) throw new Error(`timed out after ${timeoutMs} ms waiting for ${what}; last: ${String(last).slice(0, 2000)}`);
    await sleep(everyMs);
  }
}

/** A port that was free a moment ago (another process could still take it first). */
export function freePort() {
  return new Promise((ok, fail) => {
    const s = createServer();
    s.once("error", fail);
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address();
      s.close(() => ok(port));
    });
  });
}

export function isAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

/** `taskkill /F` without `/T`: only that process, never its children (e.g. the daemon). */
export function killPid(pid) {
  execFileSync("taskkill.exe", ["/F", "/PID", String(pid)], { stdio: "ignore" });
}

const run = (file, args, opts = {}) =>
  new Promise((ok) => execFile(file, args, { windowsHide: true, timeout: 60_000, ...opts }, (err, stdout, stderr) => ok({ code: err ? (err.code ?? 1) : 0, stdout, stderr })));

const http = (url, init = {}) => fetch(url, { signal: AbortSignal.timeout(HTTP_TIMEOUT_MS), ...init });

/** Driver path from fetch-driver.ps1 (pinned, hash and signature checked), matched to WebView2. */
export async function driverExe() {
  const fetched = await run("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", join(HERE, "fetch-driver.ps1")], { timeout: 300_000 });
  if (fetched.code !== 0) throw new Error(`fetch-driver.ps1 failed:\n${fetched.stdout}\n${fetched.stderr}`);
  const exe = fetched.stdout.trim().split(/\r?\n/).pop();
  const webview = exe.split(/[\\/]/).at(-2);
  const v = await run(exe, ["--version"]);
  const driver = v.stdout.match(/(\d+\.\d+\.\d+\.\d+)/)?.[1];
  if (driver !== webview) throw new Error(`WebView2 is now ${webview}, msedgedriver is ${driver}: update the driver`);
  return exe;
}

/** Pid of the child of `parent` named `name`, via CIM (locale-neutral numbers only). */
async function childPid(parent, name) {
  const r = await run("powershell.exe", [
    "-NoProfile",
    "-Command",
    `(Get-CimInstance Win32_Process -Filter "ParentProcessId=${parent} and Name='${name}'" | Select-Object -First 1).ProcessId`,
  ]);
  const pid = Number.parseInt(r.stdout.trim(), 10);
  return Number.isInteger(pid) ? pid : null;
}

/** One isolated daemon instance (own pipe, home, database). */
export class Instance {
  constructor(label) {
    this.name = `e2e-${label}-${randomUUID().slice(0, 8)}`;
    this.home = mkdtempSync(join(tmpdir(), "amiri-e2e-"));
    // Its own policies.toml (never the user's): cleanup stops the daemon without
    // asking. The UI tests of the ASK prompt write their own.
    this.writePolicies('[global]\n"daemon.stop" = "allow"\n');
  }
  writePolicies(toml) {
    const dir = join(this.home, "config");
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, "policies.toml"), toml);
  }
  args() {
    return ["--instance", this.name, "--home", this.home];
  }
  async cli(...args) {
    const r = await run(CLI_EXE, [...this.args(), "--json", ...args]);
    let json = null;
    try {
      json = JSON.parse(r.stdout);
    } catch {
      /* not JSON (e.g. an error on stderr) */
    }
    return { ...r, json };
  }
  async daemonPid() {
    const r = await this.cli("daemon", "status");
    return r.json?.state === "running" ? r.json.status.pid : null;
  }
  async shells() {
    const r = await this.cli("shell", "list");
    return Array.isArray(r.json) ? r.json : [];
  }
  /** The last lines of every log in this instance's log folder (daemon and UI). */
  logTails(lines = 30) {
    const dir = join(this.home, "logs");
    let names = [];
    try {
      names = readdirSync(dir);
    } catch {
      return "(no log folder)";
    }
    return names
      .map((n) => {
        const text = readFileSync(join(dir, n), "utf8").split(/\r?\n/).filter(Boolean);
        return `--- ${n} (last ${lines} lines) ---\n${text.slice(-lines).join("\n")}`;
      })
      .join("\n");
  }
  /** Everything the instance's UIs logged (ui.log, all days). */
  uiLog() {
    const dir = join(this.home, "logs");
    try {
      return readdirSync(dir)
        .filter((n) => n.startsWith("ui.log"))
        .map((n) => readFileSync(join(dir, n), "utf8"))
        .join("");
    } catch {
      return "";
    }
  }
  async dispose() {
    await this.cli("daemon", "stop");
    rmSync(this.home, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
}

/** msedgedriver, shared by all UIs in a test file. */
export class Driver {
  static async start() {
    const d = new Driver();
    try {
      d.port = await freePort();
      d.proc = spawn(await driverExe(), [`--port=${d.port}`], { stdio: "ignore", windowsHide: true });
      await waitFor("msedgedriver", async () => (await d.call("GET", "/status")).value?.ready);
      return d;
    } catch (e) {
      d.stop();
      throw e;
    }
  }
  async call(method, path, body) {
    const r = await http(`http://127.0.0.1:${this.port}${path}`, {
      method,
      headers: { "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    return r.json();
  }
  stop() {
    this.proc?.kill();
  }
  /** Wait until msedgedriver has exited. */
  async stopped() {
    this.stop();
    if (this.proc?.pid) await waitFor("msedgedriver to exit", () => !isAlive(this.proc.pid), 15_000);
  }
}

/** One running UI process and a WebDriver session attached to its WebView2. */
export class Ui {
  /**
   * Launch and attach. `track` is called before anything is started, so a launch
   * that fails half-way is still cleaned up.
   * @param {{ viaConsole?: boolean }} opts viaConsole: start it from a new console
   *   window (PowerShell) so the test can close that console afterwards.
   */
  static async launch(driver, instance, track, opts = {}) {
    const ui = track(new Ui(driver));
    ui.debugPort = await freePort();
    ui.webviewData = mkdtempSync(join(tmpdir(), "amiri-e2e-wv-"));
    const env = {
      ...process.env,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${ui.debugPort}`,
      AMIRI_UI_WEBVIEW_DATA: ui.webviewData,
      // Windows open beyond every monitor (not minimized), so test runs don't pop up
      // on the desktop: same results and timings as on-screen (docs/phase1/slice-c.md).
      // AMIRI_E2E_ONSCREEN=1 shows them.
      ...(process.env.AMIRI_E2E_ONSCREEN === "1" ? {} : { AMIRI_UI_WINDOW_POSITION: OFFSCREEN }),
    };
    if (opts.viaConsole) {
      ui.consolePid = await openConsole(env, [APP_EXE, ...instance.args()]);
      ui.pid = await waitFor("the UI process under the console", () => childPid(ui.consolePid, "amiri-app.exe"));
    } else {
      // Keep its exit code and the tail of its output: a UI that disappears mid-test
      // must say why (a panic message goes to stderr even in a GUI program).
      const child = spawn(APP_EXE, instance.args(), { env, stdio: ["ignore", "pipe", "pipe"] });
      ui.pid = child.pid;
      ui.output = "";
      const keep = (d) => (ui.output = (ui.output + d.toString("utf8")).slice(-OUTPUT_TAIL));
      child.stdout.on("data", keep);
      child.stderr.on("data", keep);
      child.on("exit", (code, signal) => (ui.exit = { code, signal }));
    }
    await waitFor("the UI's WebView2 debugging port", async () => (await http(`http://127.0.0.1:${ui.debugPort}/json/version`)).ok);
    const s = await driver.call("POST", "/session", {
      capabilities: { alwaysMatch: { browserName: "webview2", "ms:edgeOptions": { debuggerAddress: `127.0.0.1:${ui.debugPort}` } } },
    });
    ui.session = s.value?.sessionId;
    if (!ui.session) throw new Error(`WebDriver session failed: ${JSON.stringify(s).slice(0, 500)}`);
    await waitFor("the frontend", () => ui.exec("return !!document.querySelector('.sidebar')"));
    // A window WebView2 considers hidden may be throttled: record what the page sees.
    const view = await ui.exec("return [document.visibilityState, window.screenX, window.screenY].join(' ')");
    console.log(`# UI pid ${ui.pid}: visibility, screenX, screenY = ${view}`);
    return ui;
  }

  constructor(driver) {
    this.driver = driver;
  }

  /** For failure reports: alive or not, how it exited, and its own output. */
  describe() {
    if (this.pid && isAlive(this.pid)) return `UI pid ${this.pid}: alive`;
    const how = this.exit
      ? `exit code ${this.exit.code} (0x${((this.exit.code ?? 0) >>> 0).toString(16)}), signal ${this.exit.signal}`
      : "exit not observed (started from a console)";
    return `UI pid ${this.pid}: gone, ${how}\n--- its stdout/stderr ---\n${this.output || "(none)"}`;
  }

  async wd(method, path, body) {
    const r = await this.driver.call(method, `/session/${this.session}${path}`, body);
    if (r.value && r.value.error) throw new Error(`WebDriver ${path}: ${r.value.error}: ${r.value.message}`);
    return r.value;
  }
  exec(script, ...args) {
    return this.wd("POST", "/execute/sync", { script, args });
  }
  async click(css) {
    const el = await this.wd("POST", "/element", { using: "css selector", value: css });
    await this.wd("POST", `/element/${Object.values(el)[0]}/click`, {});
  }
  /** Type into the visible terminal, then press Enter. */
  async typeLine(text) {
    const el = await this.wd("POST", "/element", { using: "css selector", value: ".pane:not([hidden]) .xterm-helper-textarea" });
    await this.wd("POST", `/element/${Object.values(el)[0]}/value`, { text: text + KEY_ENTER });
  }

  waitConnected() {
    return waitFor("the UI to connect to the daemon", () => this.exec("return document.querySelector('.daemon-chip')?.dataset.connected === 'true'"));
  }
  rows() {
    return this.exec("return [...document.querySelectorAll('[data-testid=session-row]')].map(r => ({ id: r.dataset.sessionId, status: r.dataset.sessionStatus }))");
  }
  tabFeed(id) {
    return this.exec("return document.querySelector(`[data-testid=tab][data-session-id=\"${arguments[0]}\"]`)?.dataset.feed ?? null", id);
  }
  bannerShown() {
    return this.exec("return !!document.querySelector('[data-testid=daemon-banner]')");
  }
  /** The session's visible rows as the user sees them, trimmed (xterm's DOM renderer). */
  async lines(id) {
    const text = await this.exec(
      "const rows = document.querySelector(`[data-testid=terminal][data-session-id=\"${arguments[0]}\"] .xterm-rows`); return rows ? [...rows.children].map(r => r.textContent) : []",
      id,
    );
    return text.map((l) => l.replaceAll("\u00A0", " ").trim());
  }
  /** How many visible rows of the session equal `line` (a replay must not repeat). */
  async countLine(id, line) {
    return (await this.lines(id)).filter((l) => l === line).length;
  }
  waitLine(id, line, timeoutMs) {
    return waitFor(`line ${JSON.stringify(line)} in session ${id}`, async () => (await this.lines(id)).includes(line) || false, timeoutMs);
  }
  /** cmd's prompt ends with '>' (the path before it may be localized, the '>' is not). */
  waitPrompt(id) {
    return waitFor(`a prompt in ${id}`, async () => (await this.lines(id)).some((l) => l.endsWith(">")));
  }
  /** Click "New cmd" and wait for the live tab; returns the session id. */
  async newCmd() {
    const before = (await this.rows()).map((r) => r.id);
    await this.click("[data-testid=new-cmd]");
    const id = await waitFor("a new live cmd tab", () =>
      this.exec("return [...document.querySelectorAll('[data-testid=tab][data-feed=live]')].map(t => t.dataset.sessionId).find(id => !arguments[0].includes(id)) ?? null", before),
    );
    await this.waitPrompt(id);
    return id;
  }
  async attachRow(id) {
    await this.click(`[data-testid=session-row][data-session-id="${id}"] [data-testid=attach]`);
    await waitFor(`tab ${id} live`, async () => (await this.tabFeed(id)) === "live");
  }

  async dropSession() {
    if (!this.session) return;
    await this.driver.call("DELETE", `/session/${this.session}`).catch(() => {});
    this.session = null;
  }
  /** Close the main window the way the user does (WM_CLOSE), and wait for exit. */
  async closeNormally() {
    await this.dropSession();
    await closeMainWindow(this.pid);
    await waitFor("the UI process to exit", () => !isAlive(this.pid), 15_000);
  }
  async kill() {
    await this.dropSession();
    if (isAlive(this.pid)) killPid(this.pid);
    await waitFor("the UI process to die", () => !isAlive(this.pid), 15_000);
  }
  /** Throws if the WebView2 data folder can't be removed: a leak must fail the run. */
  async dispose() {
    await this.dropSession();
    for (const pid of [this.pid, this.consolePid]) if (pid && isAlive(pid)) killPid(pid);
    if (this.webviewData) {
      // WebView2's browser processes leave after their host; retry until the folder frees.
      await waitFor("the WebView2 data folder to free", () => {
        rmSync(this.webviewData, { recursive: true, force: true });
        return !existsSync(this.webviewData);
      }, 15_000, 300);
    }
  }

  /**
   * Record every rendered state of the daemon chip and the session rows from now on
   * (a MutationObserver sees each React commit, so a brief wrong status can't slip
   * between two polls). Read it back with `recorded()`.
   */
  startRecording() {
    return this.exec(`
      const seen = (window.__amiriSeen = []);
      const snap = () => seen.push({
        connected: document.querySelector('.daemon-chip')?.dataset.connected === 'true',
        rows: [...document.querySelectorAll('[data-testid=session-row]')].map((r) => [r.dataset.sessionId, r.dataset.sessionStatus]),
      });
      snap();
      new MutationObserver(snap).observe(document.body, {
        subtree: true, childList: true, characterData: true, attributes: true,
        attributeFilter: ['data-session-status', 'data-connected'],
      });
    `);
  }
  recorded() {
    return this.exec("return window.__amiriSeen ?? []");
  }
}

/** WM_CLOSE to the process's main window (a console window for a console process). */
export async function closeMainWindow(pid) {
  const r = await run("powershell.exe", ["-NoProfile", "-Command", `if (-not (Get-Process -Id ${pid}).CloseMainWindow()) { exit 3 }`]);
  if (r.code !== 0) throw new Error(`CloseMainWindow(${pid}) failed (exit ${r.code}; 3 = no main window): ${r.stderr}`);
}

/**
 * Open a new console window running PowerShell, which runs `argv`; returns that
 * PowerShell's pid. Node's `detached` gives a child no console at all on Windows,
 * hence Start-Process; -EncodedCommand avoids a second layer of quoting.
 */
async function openConsole(env, argv) {
  const quoted = argv.map((a) => `'${a.replaceAll("'", "''")}'`);
  const inner = Buffer.from(`& ${quoted.join(" ")}`, "utf16le").toString("base64");
  const r = await run(
    "powershell.exe",
    ["-NoProfile", "-Command", `(Start-Process powershell.exe -ArgumentList '-NoProfile','-NoExit','-EncodedCommand','${inner}' -PassThru).Id`],
    { env },
  );
  const pid = Number.parseInt(r.stdout.trim(), 10);
  if (r.code !== 0 || !Number.isInteger(pid)) throw new Error(`opening a console failed (${r.code}): ${r.stdout} ${r.stderr}`);
  return pid;
}

/**
 * Processes this harness started that are still running: its msedgedriver (by port),
 * WebView2 hosts using a test data folder, and daemons or UIs of a test instance.
 * Matched on the command line only, never by name alone: the system runs its own
 * WebView2 processes (e.g. Windows Search), and the user may run AMIRI.
 */
export async function leftovers(driverPort) {
  const r = await run("powershell.exe", [
    "-NoProfile",
    "-Command",
    "Get-CimInstance Win32_Process -Filter \"Name='msedgedriver.exe' or Name='msedgewebview2.exe' or Name='amirid.exe' or Name='amiri-app.exe'\" | ForEach-Object { '{0}|{1}|{2}' -f $_.ProcessId, $_.Name, $_.CommandLine }",
  ]);
  if (r.code !== 0) throw new Error(`listing processes failed (${r.code}): ${r.stderr}`);
  const ours = (name, cmd) =>
    (name === "msedgedriver.exe" && driverPort !== undefined && cmd.includes(`--port=${driverPort}`)) ||
    (name === "msedgewebview2.exe" && cmd.includes("amiri-e2e-wv-")) ||
    ((name === "amirid.exe" || name === "amiri-app.exe") && /--instance\s+"?e2e-/.test(cmd));
  return r.stdout
    .split(/\r?\n/)
    .filter(Boolean)
    .map((line) => {
      const [pid, name, ...rest] = line.split("|");
      return { pid: Number.parseInt(pid, 10), name, cmd: rest.join("|") };
    })
    .filter((p) => Number.isInteger(p.pid) && ours(p.name, p.cmd));
}

/** Fail if anything the harness started outlives the cleanup (killing it first). */
export async function assertNoLeftovers(driverPort) {
  let left = [];
  try {
    await waitFor("test processes to exit", async () => (left = await leftovers(driverPort)).length === 0, 20_000, 500);
  } catch {
    for (const p of left) if (isAlive(p.pid)) killPid(p.pid);
    const list = left.map((p) => `${p.name} ${p.pid}: ${p.cmd.slice(0, 200)}`).join("\n  ");
    throw new Error(`${left.length} process(es) outlived the tests (now killed):\n  ${list}`);
  }
}

/** The sources each binary is built from. */
const CRATES = ["crates"];
const SOURCES = [
  [APP_EXE, ["app/src", "app/src-tauri/src", "app/index.html", ...CRATES]],
  [CLI_EXE, CRATES],
  [DAEMON_EXE, CRATES],
];

/** Newest modification time among `roots` (relative to the repository). */
function newestSource(roots) {
  roots = roots.map((r) => join(ROOT, r));
  const exts = /\.(rs|ts|tsx|css|html|toml|sql)$/;
  let newest = { time: 0, file: "" };
  const visit = (path) => {
    const st = statSync(path);
    if (st.isDirectory()) {
      for (const name of readdirSync(path)) if (name !== "target" && name !== "node_modules") visit(join(path, name));
    } else if (exts.test(path) && st.mtimeMs > newest.time) {
      newest = { time: st.mtimeMs, file: path };
    }
  };
  roots.forEach(visit);
  return newest;
}

/**
 * The binaries must exist and be newer than every source file: a run on stale
 * binaries tests old code and its results mean nothing (it happened once: npm's
 * ignore-scripts silently skipped the pre-test build).
 */
export function requireBuilt() {
  for (const [exe, roots] of SOURCES) {
    if (!existsSync(exe)) throw new Error(`${exe} is missing: run \`npm run e2e\` (it builds first)`);
    const built = statSync(exe).mtimeMs;
    const src = newestSource(roots);
    const name = exe.split(/[\\/]/).pop();
    console.log(`# ${name} built ${new Date(built).toISOString()}; newest source ${new Date(src.time).toISOString()}`);
    if (built < src.time) {
      throw new Error(`${exe} is older than ${src.file} (${new Date(src.time).toISOString()}): run \`npm run e2e\` (it builds first)`);
    }
  }
}
