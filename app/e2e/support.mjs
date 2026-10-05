// E2E support: an isolated daemon instance per test, the real UI exe, and a W3C
// WebDriver client (Node's fetch) attached to the UI's WebView2 through its
// debugging port. The harness launches the UI itself, so it can close it normally,
// kill it, or start it from a console window it then closes. Assertions use only
// locale-neutral values: markers the shell computes, numbers, exit codes, pids.
// Screens are read from xterm's DOM rows: the text the user sees, no test hooks.
import { execFile, execFileSync, spawn } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
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
    };
    if (opts.viaConsole) {
      ui.consolePid = await openConsole(env, [APP_EXE, ...instance.args()]);
      ui.pid = await waitFor("the UI process under the console", () => childPid(ui.consolePid, "amiri-app.exe"));
    } else {
      ui.pid = spawn(APP_EXE, instance.args(), { env, stdio: "ignore" }).pid;
    }
    await waitFor("the UI's WebView2 debugging port", async () => (await http(`http://127.0.0.1:${ui.debugPort}/json/version`)).ok);
    const s = await driver.call("POST", "/session", {
      capabilities: { alwaysMatch: { browserName: "webview2", "ms:edgeOptions": { debuggerAddress: `127.0.0.1:${ui.debugPort}` } } },
    });
    ui.session = s.value?.sessionId;
    if (!ui.session) throw new Error(`WebDriver session failed: ${JSON.stringify(s).slice(0, 500)}`);
    await waitFor("the frontend", () => ui.exec("return !!document.querySelector('.sidebar')"));
    return ui;
  }

  constructor(driver) {
    this.driver = driver;
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

export function requireBuilt() {
  for (const exe of [APP_EXE, CLI_EXE, DAEMON_EXE]) {
    if (!existsSync(exe)) throw new Error(`${exe} is missing: run \`npm run e2e\` (it builds first)`);
  }
}
