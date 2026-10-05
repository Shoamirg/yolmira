// `npm test`: Node runs this TypeScript directly (type stripping).
import { test } from "node:test";
import assert from "node:assert/strict";
import type { SessionInfo } from "../src/api.ts";
import { shownStatus, type StatusView } from "../src/status.ts";

const running: SessionInfo = {
  id: "01a10c90-8c41-7156-8fde-e6d2e85083eb",
  shell: "cmd",
  cwd: "C:\\",
  pid: 1234,
  status: "RUNNING",
  exit_code: null,
  attached: 0,
  cols: 120,
  rows: 30,
};
const live: StatusView = { connected: true, pid: 100, epoch: 3, snapshot: { epoch: 3, pid: 100 } };

test("a list from this epoch and this daemon shows real states", () => {
  assert.equal(shownStatus(running, live), "RUNNING");
  assert.equal(shownStatus({ ...running, status: "EXITED" }, live), "EXITED");
});

test("disconnected: every session is UNKNOWN, whatever the list says", () => {
  assert.equal(shownStatus(running, { ...live, connected: false }), "UNKNOWN");
  assert.equal(shownStatus(running, { ...live, connected: false, pid: null, snapshot: null }), "UNKNOWN");
});

test("connected without a list yet: RECONNECTING", () => {
  assert.equal(shownStatus(running, { ...live, snapshot: null }), "RECONNECTING");
});

test("a list from an earlier epoch never shows RUNNING", () => {
  assert.equal(shownStatus(running, { ...live, epoch: 4 }), "RECONNECTING");
});

test("a list from another daemon process never shows RUNNING, even in the same epoch", () => {
  // e.g. a list applied out of order after a restart the epoch logic missed.
  assert.equal(shownStatus(running, { ...live, pid: 200 }), "RECONNECTING");
});
