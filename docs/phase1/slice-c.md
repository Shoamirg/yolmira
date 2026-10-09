# Phase 1, Slice C: the Tauri UI attaches to sessions (findings)

- Date: 2026-10-05. Plan: `docs/phase1-plan.md` §1 Slice C.
- Code: `54f0dde` (UI, E2E harness, CI steps), `86e7be9` (harness call sites), `9fce2ca` (review fixes), `c7dafab` (test diagnostics, signer check).
- **Recorded runs** on `c7dafab`'s code, non-admin (Medium), ru-RU, WebView2 154.0.4258.53, Node 22.23.1: `npm run e2e` (from `app/`): **6 of 6 pass**, twice in a row, ~55 s per run. `daemon_down_banner_and_reconnect` alone: 12 runs, all pass (but see "Open": it failed twice on an earlier build).
- Workspace: `scripts/test.ps1` on `c7dafab`, exit 0, all 12 test binaries pass, 3 tests not run (same three as Slice B, each with its reason). Durations now printed per binary: `pty` 8.1 s, `sessions` 8.6 s, `lifecycle` 6.5 s, `ipc` 3.2 s, the rest under 1 s.
- App crate: `cargo fmt --check`, `cargo clippy --all-targets -D warnings` (debug **and** `--release`, which compiles different code: see the env-hook fix), `cargo test` 9 pass. Frontend: `tsc --noEmit` and `vite build` clean.
- Reviews before the fix commit: `rust-reviewer`, `react-reviewer`, `security-reviewer` (read-only). Every HIGH and MEDIUM finding was fixed in `9fce2ca` or turned out not to apply (reasons below); deferred LOWs are under "Open".

## Exit tests (`app/e2e/ui.test.mjs`)

Each test has its own daemon instance (`--instance`/`--home`), its own UI processes and its own WebView2 data folder. Assertions use only values the shell computes (`[42]`, `1042`, `[Windows_NT]`), status attributes, pids and exit codes; screens are read from xterm's DOM rows.

| Plan test | Result |
|---|---|
| `ui_close_reopen_shell_survives` | pass. Window closed with `WM_CLOSE`; daemon pid unchanged, shell RUNNING; relaunch + attach: the earlier `set AMIRI_MARK=42` line is replayed **exactly once**, and `echo (%AMIRI_MARK%)` gives `(42)` |
| `ui_killed_shell_survives` | pass. Same after `taskkill /F` of the UI (without `/T`: the daemon is not the UI's child anyway) |
| `ui_started_from_closed_terminal` | pass. UI started from a new PowerShell console; the console is killed; the UI stays alive (polled for 3 s), the daemon it started keeps its pid, and the shell still answers |
| `two_uis_same_session` | pass. Input typed in either window shows in both, once each |
| `daemon_down_banner_and_reconnect` | pass. A MutationObserver records **every rendered state** from the kill on: while disconnected every row is UNKNOWN, and the old shell is never rendered RUNNING again (not even for one frame at the reconnect). Banner, tab `closed` → `exited`, chip shows the new pid, old shell EXITED (D3), a new shell works |
| Added: `output_flood_flows_through` | pass. 150 000 lines (~2.5 MB, ~3.3 MB as base64 events) through cmd; the last line arrives and the shell still answers. More than the 1 MiB flow-control window, so it fails if acknowledgements break |

## Findings

1. **Each WebView2 instance needs its own data folder to get its own debugging port.** Two UIs sharing the default folder share one browser process, so the second `--remote-debugging-port` is ignored. Debug builds read `AMIRI_UI_WEBVIEW_DATA` for this; release builds don't (below).
2. **WebView2 takes configuration from `WEBVIEW2_*` environment variables**: browser arguments (`--remote-debugging-port`, unauthenticated: anyone on the machine could drive the session commands through it), the browser executable folder, the user-data folder. The E2E harness depends on that in debug builds. Release builds now remove every `WEBVIEW2_*` variable (any case) first thing in `main`, before the webview or any other thread exists.
3. **The daemon already copes with a slow attachment** (`session_handlers.rs`: a lagging attachment gets a reset plus a scrollback snapshot, at most 2 MiB). So the UI can simply stop reading a feed: each output event carries a credit; the feed task waits on a 1 MiB semaphore; the frontend returns credit (`session_ack`) from xterm's `write` callback. Without this, a flood queued events in the webview without bound, and xterm drops data past ~50 MB with only a console warning.
4. **Attach replay arrives before the pane has its real size** when a tab is reattached in the background (hidden panes aren't fitted), so cursor-addressed output in the replay is laid out at 120x30 and then reflowed. Not fixed (see "Open").
5. **`node:test` has no default per-test timeout.** One hung wait would hang the run; the e2e script now passes `--test-timeout=600000` (the flood test allows 5 min for its last line).
6. **A half-finished launch leaked WebView2 data folders** (three from the first, pre-crash runs at 18:43–18:48, removed). `Ui.launch` now registers the UI for cleanup before starting anything, and a folder that can't be removed fails the run.

## Review findings and what was done

| Source | Severity | Finding | Done |
|---|---|---|---|
| react | HIGH | Arrow keys on the tab bar: focus jumped into the terminal after one step, and the next arrow key reached the shell | The pane focuses the terminal only when focus isn't in the tab bar; Enter on a tab moves into its terminal |
| rust | HIGH | Control calls queued behind a hung one reused the dead connection after it was dropped: each could wait another 30 s | After taking the lock, a call checks its connection is still current, else fails at once |
| rust | MEDIUM | Input order across concurrent invokes not guaranteed | **Not applicable:** `queueInput` (terminals.ts) already sends one input at a time per session, in order |
| rust | MEDIUM | A stale `Notify` permit could tear down a fresh, healthy connection | The broken signal belongs to each connection (`Link`) |
| rust + react | MEDIUM | No backpressure from the webview | Flow control (finding 3) |
| rust | MEDIUM | Failed or hung attach left a placeholder (the session silently had no feed); attach not atomic with a disconnect | Connect + attach bounded (15 s); placeholder removed on failure; the link is read under the feeds lock and re-checked before the feed task starts |
| react | MEDIUM | A list fetched before a reconnect could be applied after it (stale RUNNING); a daemon change without a visible disconnect wasn't treated as one | Connection epoch (bumped on any change of connected/pid/instance) and a list sequence number; results from an older epoch or request are dropped |
| react | MEDIUM | The startup state snapshot could overwrite a newer state event | The snapshot applies only if no state event has arrived |
| react | MEDIUM | Keystrokes sent to panes that aren't live, each failure replacing the error panel | Input only goes to a live feed; the pane note says why otherwise |
| react | MEDIUM | Focus lost when a tab is detached | Focus moves to the newly active tab |
| react | MEDIUM | Attach/Kill buttons without context for screen readers | `aria-label` names the shell |
| react | MEDIUM | The UNKNOWN check sampled every 100 ms and stopped exactly when a stale RUNNING would show; unbounded wait loop; replay checks only for presence | MutationObserver recording (above); `waitFor`; lines counted exactly once |
| security | MEDIUM | Remote-debugging and data-folder hooks live in release builds | Finding 2 |
| rust | LOW | Ended feeds stayed in the map | A feed removes its own entry when it ends |
| rust | LOW | Two concurrent `ensure_daemon` calls | **Not applicable:** serialized by the named start mutex (`launch.rs`) |
| react | LOW | `.tab` wrappers inside the tablist; pane notes not announced | `role="presentation"`; `role="status"` |
| react | LOW | Detaching a tab while offline showed an error | No detach call while disconnected (there is no feed) |
| react | LOW | Invisible characters in the harness; no per-test timeout; fixed sleep; cleanup failure only logged | ``/` ` escapes; `--test-timeout`; polled liveness; cleanup failures fail the run |
| security | INFO | Driver signer checked by substring | Anchored to the `O=` RDN |

## After acceptance (2026-10-05 to 2026-10-09)

The reviewer accepted Slice C with conditions (decisions.md 2026-10-05); all are met (.., pushed). Further findings while doing so:

1. **E2E runs used stale binaries.**  sets  (supply chain), which also makes npm skip  scripts:  never ran its  build. Runs since Slice C tested whatever binaries a manual build had left; results from 2026-10-06 () and the first 2026-10-09 run don't count. Fixed (, ): the  script builds explicitly, and the harness fails if any binary is older than its own sources, printing each build time.
2. **The UIs that "crashed" mid-run were closed by hand**, twice (the user confirmed both; exit code 0, window closed). An empty  hid it: the stale UI build still used a buffered writer whose last lines are lost when Tauri ends the process. Now (): unbuffered log;  on every exit the UI sees (window closed, exit request, startup failure); panics logged; a run marker (pid + process start time) makes the next start report a UI that was killed or crashed. E2E asserts both after a close and after .
3. **Off-screen test windows.** Windows ignores an off-screen position given at window creation (the first attempt opened on-screen); moving the built window works (). Comparison on the same build (), WebView2 154.0.4258.62:

| Test | On-screen | Off-screen (-4000,-4000) |
|---|---|---|
| ui_close_reopen_shell_survives | ok, 2.71 s | ok, 2.59 s |
| ui_killed_shell_survives | ok, 2.20 s | ok, 2.01 s |
| ui_started_from_closed_terminal | ok, 4.81 s | ok, 4.85 s |
| two_uis_same_session | ok, 1.61 s | ok, 1.58 s |
| daemon_down_banner_and_reconnect | ok, 2.73 s | ok, 2.67 s |
| output_flood_flows_through | ok, 15.73 s | ok, 15.58 s |
| Whole run | 6/6, 36.8 s | 6/6, 36.3 s |

   Every page reported  off-screen too: WebView2 doesn't treat these windows as hidden. Off-screen is now the default;  shows the windows. The console window of  still appears briefly.
4. **WebView2 updated itself** (154.0.4258.53 → .62) between runs; the harness stopped with the planned message, and the matching driver was pinned (: zip SHA-256 recorded, signature Valid, O=Microsoft Corporation).
5. **CI did not run** for  (run 37928451747): GitHub refused to start the job over the account's billing ("recent account payments have failed or your spending limit needs to be increased"). Not a code failure; rerun once billing is fixed.

## Plan deviations

- **No `tauri-driver`.** The harness runs WebView2's `msedgedriver` itself and attaches to each UI through its debugging port (`ms:edgeOptions.debuggerAddress`). `tauri-driver` launches the app itself, but these tests must launch the UI in specific ways (from a console they then close; killed; two at once) and close it with `WM_CLOSE`. `tauri-driver` was never installed; its row in the plan's dependency table (§5) can go. The driver is pinned per WebView2 version with the zip's SHA-256 and an Authenticode check, and a version mismatch fails with *"WebView2 is now X, msedgedriver is Y: update the driver"* as planned.
- **E2E tests run locally only.** msedgedriver must match the machine's WebView2 exactly, and a hosted runner image doesn't pin WebView2. CI builds the frontend (`npm ci` with lifecycle scripts off, typecheck, build) and runs fmt/clippy/unit tests on the app crate.

## Open

- **`daemon_down_banner_and_reconnect` failed twice** (the full run on `9fce2ca`'s first build, and once more alone on the same binary): exactly one rendered frame, right after the reconnect, showed the dead shell RUNNING; the next frame (1 ms later) showed EXITED. The daemon itself lists the shell as EXITED immediately after a restart (checked with the CLI), and the store trace on a rebuilt binary showed the intended order (connected with no fresh list → UNKNOWN → EXITED). After rebuilding from the same source: 14 runs, no failure. Cause not found. The test now prints the full rendered trail on failure, so the next occurrence says which state produced it.
- **`pty` (Slice B) failed once in 11 runs**: in `scripts/test.ps1` right after an E2E run, the binary hung ~140 s and failed; the failing test's output wasn't kept. 8 runs of the binary alone and two more full runs were clean. Not reproduced; the next `test.ps1` failure should keep the full log.
- Replay at the default size for background reattaches (finding 4): fit hidden panes before attaching, or send the real size with the attach.
- Kill has no confirmation step; the Detach button sits inside the tablist; xterm keeps Tab, so keyboard users can't leave a terminal with Tab (needs a documented escape key).
- Security LOWs: test that xterm's automatic replies (DA/DSR/DECRQSS, `CSI 21 t`) can't echo attacker-controlled text into an (eventually elevated) shell; cap simultaneous feeds; `style-src 'unsafe-inline'` stays (xterm and React inline styles); add non-blocking `npm audit` / `cargo audit` steps to CI.
- No "connecting" state while the first `ensure_daemon` runs (up to 10 s): the banner shows "disconnected" meanwhile.
- `supervise` and the feeds are never cancelled; fine while the app exits with its last window, revisit if a tray icon keeps it alive.
- `scripts/test.ps1` now prints durations, but two binaries are both named `amiri` (the lib's and the bin's unit tests).

## CI

- **Green**: run `37330124720` on `9acaed9` (the first with the UI steps: Node 22.23.1 pinned, `npm ci` + typecheck + build, app crate fmt/clippy/tests), 9 min 23 s in total against ~6 min before. 7 tests not run, each with its reason (unchanged from Slice B).
- **CI test durations** (named per binary now): `pty` 55.1 s (Slice B: 70.3 s; locally 8.1 s), `sessions` 20.8 s (31.1 s), `lifecycle` 2.6 s, `ipc` 1.3 s, the rest under 1 s.
