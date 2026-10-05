// Slice C exit tests (phase1-plan §1). Run with `npm run e2e` from app/ (builds first).
// Every test uses its own daemon instance and its own UI processes.
import { after, before, test } from "node:test";
import assert from "node:assert/strict";
import { Driver, Instance, Ui, isAlive, killPid, requireBuilt, sleep, waitFor } from "./support.mjs";

let driver;
const cleanups = [];
const track = (x) => (cleanups.push(x), x);

before(async () => {
  requireBuilt();
  driver = await Driver.start();
});

after(async () => {
  for (const c of cleanups.reverse()) await c.dispose().catch((e) => console.error("cleanup:", e));
  driver?.stop();
});

/** Open a cmd tab, set a variable in it, and return its id. */
async function shellWithMark(ui) {
  const id = await ui.newCmd();
  await ui.typeLine("set AMIRI_MARK=42");
  await ui.typeLine("echo [%AMIRI_MARK%]");
  await ui.waitLine(id, "[42]");
  return id;
}

/** Relaunch, attach to `id`, and check the replay and the variable. */
async function reattachAndCheck(instance, id) {
  const ui = track(await Ui.launch(driver, instance));
  await ui.waitConnected();
  await waitFor(`session ${id} listed RUNNING`, async () => (await ui.rows()).some((r) => r.id === id && r.status === "RUNNING"));
  await ui.attachRow(id);
  // The earlier output is visible again (replayed from the daemon's scrollback).
  await waitFor("replayed earlier output", async () => (await ui.lines(id)).some((l) => l.endsWith("set AMIRI_MARK=42")));
  await ui.typeLine("echo (%AMIRI_MARK%)");
  await ui.waitLine(id, "(42)");
}

test("ui_close_reopen_shell_survives", async () => {
  const instance = track(new Instance("close"));
  const ui = track(await Ui.launch(driver, instance));
  await ui.waitConnected();
  const id = await shellWithMark(ui);
  const daemon = await instance.daemonPid();

  await ui.closeNormally();

  assert.equal(await instance.daemonPid(), daemon, "the daemon outlives the window");
  assert.equal((await instance.shells()).find((s) => s.id === id)?.status, "RUNNING");
  await reattachAndCheck(instance, id);
});

test("ui_killed_shell_survives", async () => {
  const instance = track(new Instance("kill"));
  const ui = track(await Ui.launch(driver, instance));
  await ui.waitConnected();
  const id = await shellWithMark(ui);
  const daemon = await instance.daemonPid();

  await ui.kill();

  assert.equal(await instance.daemonPid(), daemon, "the daemon outlives a killed UI");
  assert.equal((await instance.shells()).find((s) => s.id === id)?.status, "RUNNING");
  await reattachAndCheck(instance, id);
});

test("ui_started_from_closed_terminal", async () => {
  const instance = track(new Instance("console"));
  assert.equal(await instance.daemonPid(), null, "no daemon before the UI starts one");
  const ui = track(await Ui.launch(driver, instance, { viaConsole: true }));
  await ui.waitConnected();
  const daemon = await instance.daemonPid();
  assert.ok(daemon, "the UI started the daemon");
  const id = await ui.newCmd();

  killPid(ui.consolePid); // the console's only process: its window closes
  await waitFor("the console to close", () => !isAlive(ui.consolePid));
  await sleep(2000);

  assert.ok(isAlive(ui.pid), "the UI survives its console closing");
  assert.equal(await instance.daemonPid(), daemon, "the daemon survives the console closing");
  await ui.typeLine("echo [%OS%]");
  await ui.waitLine(id, "[Windows_NT]");
});

test("two_uis_same_session", async () => {
  const instance = track(new Instance("two"));
  const a = track(await Ui.launch(driver, instance));
  await a.waitConnected();
  const id = await a.newCmd();
  const b = track(await Ui.launch(driver, instance));
  await b.waitConnected();
  await b.attachRow(id);

  // `set /a` prints its result: the number appears only if the shell computed it.
  await a.typeLine("set /a 6*7+1000");
  await a.waitLine(id, "1042");
  await b.waitLine(id, "1042");
  await b.typeLine("set /a 7*7+2000");
  await b.waitLine(id, "2049");
  await a.waitLine(id, "2049");
});

test("daemon_down_banner_and_reconnect", async () => {
  const instance = track(new Instance("down"));
  const ui = track(await Ui.launch(driver, instance));
  await ui.waitConnected();
  const id = await ui.newCmd();
  const first = await instance.daemonPid();
  assert.equal(await ui.bannerShown(), false);

  killPid(first);
  await waitFor("the banner", () => ui.bannerShown());
  // While the daemon is down, nothing is known: every row is UNKNOWN, never RUNNING.
  const seen = new Set();
  for (let i = 0; i < 10; i++) {
    (await ui.rows()).forEach((r) => seen.add(r.status));
    await sleep(100);
  }
  assert.deepEqual([...seen], ["UNKNOWN"]);

  const started = await instance.cli("daemon", "start");
  assert.equal(started.json?.state, "started", started.stderr);
  const second = await instance.daemonPid();
  while (await ui.bannerShown()) {
    (await ui.rows()).forEach((r) => assert.notEqual(r.status, "RUNNING", "no RUNNING before the reconnect"));
    await sleep(100);
  }
  await waitFor("the chip to show the new daemon", () => ui.exec("return document.querySelector('.daemon-chip')?.textContent ?? ''").then((t) => t.includes(`pid ${second}`)));
  // The daemon restart ended the old shell (D3); the UI says so instead of RUNNING.
  await waitFor("the old shell listed EXITED", async () => (await ui.rows()).find((r) => r.id === id)?.status === "EXITED");
  const fresh = await ui.newCmd();
  await ui.typeLine("echo [%OS%]");
  await ui.waitLine(fresh, "[Windows_NT]");
});
