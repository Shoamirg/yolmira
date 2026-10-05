// Slice C exit tests (phase1-plan §1). Run with `npm run e2e` from app/ (builds first).
// Every test uses its own daemon instance and its own UI processes.
import { after, before, test } from "node:test";
import assert from "node:assert/strict";
import { Driver, Instance, Ui, assertNoLeftovers, isAlive, killPid, requireBuilt, sleep, waitFor } from "./support.mjs";

let driver;
const cleanups = [];
/** What the running test created, for its failure report. */
let mine = [];
const track = (x) => (cleanups.push(x), mine.push(x), x);

/**
 * A test whose failure also reports, for everything it started: whether each UI
 * process is still alive, and the tails of each instance's daemon and UI logs.
 */
function e2e(name, body) {
  test(name, async () => {
    mine = [];
    try {
      await body();
    } catch (e) {
      const facts = mine.map((x) =>
        x instanceof Instance ? `instance ${x.name}:\n${x.logTails()}` : `UI pid ${x.pid}: ${x.pid && isAlive(x.pid) ? "alive" : "gone"}`,
      );
      e.message += `\n=== diagnostics ===\n${facts.join("\n")}`;
      throw e;
    }
  });
}

before(async () => {
  requireBuilt();
  driver = await Driver.start();
});

after(async () => {
  const failures = [];
  for (const c of cleanups.reverse()) await c.dispose().catch((e) => failures.push(e));
  await driver?.stopped().catch((e) => failures.push(e));
  // Nothing the tests started may outlive them: driver, WebView2 hosts, daemons, UIs.
  await assertNoLeftovers(driver?.port).catch((e) => failures.push(e));
  if (failures.length) throw new AggregateError(failures, `${failures.length} cleanup step(s) failed`);
});

/** Open a cmd tab, set a variable in it, and return its id. */
async function shellWithMark(ui) {
  const id = await ui.newCmd();
  await ui.typeLine("set AMIRI_MARK=42");
  await ui.typeLine("echo [%AMIRI_MARK%]");
  await ui.waitLine(id, "[42]");
  assert.equal(await ui.countLine(id, "[42]"), 1);
  return id;
}

/** Relaunch, attach to `id`, and check the replay and the variable. */
async function reattachAndCheck(instance, id) {
  const ui = await Ui.launch(driver, instance, track);
  await ui.waitConnected();
  await waitFor(`session ${id} listed RUNNING`, async () => (await ui.rows()).some((r) => r.id === id && r.status === "RUNNING"));
  await ui.attachRow(id);
  // The earlier output is visible again (replayed from the daemon's scrollback).
  await waitFor("replayed earlier output", async () => (await ui.lines(id)).some((l) => l.endsWith("set AMIRI_MARK=42")));
  // Replayed once: a second feed or a missed reset would show it twice.
  assert.equal((await ui.lines(id)).filter((l) => l.endsWith("set AMIRI_MARK=42")).length, 1);
  assert.equal(await ui.countLine(id, "[42]"), 1);
  await ui.typeLine("echo (%AMIRI_MARK%)");
  await ui.waitLine(id, "(42)");
}

e2e("ui_close_reopen_shell_survives", async () => {
  const instance = track(new Instance("close"));
  const ui = await Ui.launch(driver, instance, track);
  await ui.waitConnected();
  const id = await shellWithMark(ui);
  const daemon = await instance.daemonPid();

  await ui.closeNormally();

  assert.equal(await instance.daemonPid(), daemon, "the daemon outlives the window");
  assert.equal((await instance.shells()).find((s) => s.id === id)?.status, "RUNNING");
  await reattachAndCheck(instance, id);
});

e2e("ui_killed_shell_survives", async () => {
  const instance = track(new Instance("kill"));
  const ui = await Ui.launch(driver, instance, track);
  await ui.waitConnected();
  const id = await shellWithMark(ui);
  const daemon = await instance.daemonPid();

  await ui.kill();

  assert.equal(await instance.daemonPid(), daemon, "the daemon outlives a killed UI");
  assert.equal((await instance.shells()).find((s) => s.id === id)?.status, "RUNNING");
  await reattachAndCheck(instance, id);
});

e2e("ui_started_from_closed_terminal", async () => {
  const instance = track(new Instance("console"));
  assert.equal(await instance.daemonPid(), null, "no daemon before the UI starts one");
  const ui = await Ui.launch(driver, instance, track, { viaConsole: true });
  await ui.waitConnected();
  const daemon = await instance.daemonPid();
  assert.ok(daemon, "the UI started the daemon");
  const id = await ui.newCmd();

  killPid(ui.consolePid); // the console's only process: its window closes
  await waitFor("the console to close", () => !isAlive(ui.consolePid));
  // A UI tied to the console would go within moments; watch it for 3 s.
  for (let i = 0; i < 20; i++) {
    assert.ok(isAlive(ui.pid), "the UI survives its console closing");
    await sleep(150);
  }
  assert.equal(await instance.daemonPid(), daemon, "the daemon survives the console closing");
  await ui.typeLine("echo [%OS%]");
  await ui.waitLine(id, "[Windows_NT]");
});

e2e("two_uis_same_session", async () => {
  const instance = track(new Instance("two"));
  const a = await Ui.launch(driver, instance, track);
  await a.waitConnected();
  const id = await a.newCmd();
  const b = await Ui.launch(driver, instance, track);
  await b.waitConnected();
  await b.attachRow(id);

  // `set /a` prints its result: the number appears only if the shell computed it.
  await a.typeLine("set /a 6*7+1000");
  await a.waitLine(id, "1042");
  await b.waitLine(id, "1042");
  await b.typeLine("set /a 7*7+2000");
  await b.waitLine(id, "2049");
  await a.waitLine(id, "2049");
  for (const ui of [a, b]) {
    assert.equal(await ui.countLine(id, "1042"), 1, "each window shows the output once");
    assert.equal(await ui.countLine(id, "2049"), 1, "each window shows the output once");
  }
});

e2e("daemon_down_banner_and_reconnect", async () => {
  const instance = track(new Instance("down"));
  const ui = await Ui.launch(driver, instance, track);
  await ui.waitConnected();
  const id = await ui.newCmd();
  const first = await instance.daemonPid();
  assert.equal(await ui.bannerShown(), false);
  await ui.startRecording();

  killPid(first);
  await waitFor("the banner", () => ui.bannerShown());
  await waitFor("the tab to show its feed closed", async () => (await ui.tabFeed(id)) === "closed");

  const started = await instance.cli("daemon", "start");
  assert.equal(started.json?.state, "started", started.stderr);
  const second = await instance.daemonPid();
  await waitFor("the banner to clear", async () => !(await ui.bannerShown()));
  await waitFor("the chip to show the new daemon", () => ui.exec("return document.querySelector('.daemon-chip')?.textContent ?? ''").then((t) => t.includes(`pid ${second}`)));
  // The daemon restart ended the old shell (D3); the UI says so instead of RUNNING.
  await waitFor("the old shell listed EXITED", async () => (await ui.rows()).find((r) => r.id === id)?.status === "EXITED");
  await waitFor("the old tab marked exited", async () => (await ui.tabFeed(id)) === "exited");

  // Every state the UI rendered from the kill on: while disconnected every row is
  // UNKNOWN; once reconnected the dead shell is RECONNECTING until the new daemon's
  // list arrives, then EXITED. It is never shown RUNNING again, not even for one frame.
  const seen = await ui.recorded();
  const fromKill = seen.slice(seen.findIndex((r) => !r.connected));
  assert.ok(fromKill.length > 0 && !fromKill[0].connected, "the disconnect was rendered");
  const trail = fromKill.map((r) => `${r.connected ? "up" : "down"}:${r.rows.map(([sid, st]) => `${sid.slice(-4)}=${st}`).join(",")}`).join(" | ");
  for (const r of fromKill) {
    if (!r.connected) assert.ok(r.rows.every(([, status]) => status === "UNKNOWN"), trail);
    const mine = r.rows.find(([sid]) => sid === id)?.[1];
    assert.ok(["UNKNOWN", "RECONNECTING", "EXITED"].includes(mine), trail);
  }

  const fresh = await ui.newCmd();
  await ui.typeLine("echo [%OS%]");
  await ui.waitLine(fresh, "[Windows_NT]");
});

// Flow control: more output than the 1 MiB window must arrive in full, which needs
// the terminal's acknowledgements, and the shell must stay responsive after it.
e2e("output_flood_flows_through", async () => {
  const instance = track(new Instance("flood"));
  const ui = await Ui.launch(driver, instance, track);
  await ui.waitConnected();
  const id = await ui.newCmd();
  // About 2.5 MB of output (3.3 MB as base64 events).
  await ui.typeLine("for /L %i in (1,1,150000) do @echo flood-line-%i");
  await ui.waitLine(id, "flood-line-150000", 300_000);
  await ui.typeLine("echo [%OS%]");
  await ui.waitLine(id, "[Windows_NT]");
});
