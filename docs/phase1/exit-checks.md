# Phase 1 exit checks

Order (decisions.md 2026-10-09): these run before admin PowerShell step 1 and Phase 2. Slice D acceptance still waits for one green CI run on the latest commit: CI first ran on 2026-10-22 (public repo, `e63f713`) and failed one test's precondition (below). Results are recorded here; the phase findings link this file.

| Check | Who | Status |
|---|---|---|
| Close UI / reopen / shell survives, fresh build | automated (E2E) | 9/9 at `39f6c17`; 9/9 after the fix (`2ccd493` to `e3920c5`, results below) |
| 1-hour soak, overload path (client fell behind) | automated | passed `39f6c17`; not the exit check |
| 1-hour soak with a client that keeps up (`soak_1h_output_flood`, `YOLMIRA_SOAK=3600`) | automated | **passed** 2026-10-22: 0 resyncs, daemon flat at ~11 MB |
| WSL terminate on a throwaway distro (`wsl_shutdown_while_attached`) | automated | 3/3 at `39f6c17`; **3/3 at `e63f713`** |
| Logoff while the daemon runs detached | the user, steps below | pending |
| Pipe-owner rejection with a second Windows account (H1) | the user, steps below | pending |

## Yolmira build (`39f6c17`)

- **Close UI / reopen / shell survives:** E2E 9/9 (47.7 s), every binary built after its newest source (`yolmira-app.exe` 14:37:29Z, `yolmira.exe`/`yolmirad.exe` 14:33:46Z; sources 14:36:17Z / 14:33:41Z).
- **WSL terminate:** `wsl_shutdown_while_attached` 3/3 on `amiri-throwaway` (one run after `wsl --terminate`), 0.5 to 1.0 s; the user's distros untouched.

### 1-hour soak, overload path

`soak_1h_output_flood` with `YOLMIRA_SOAK=3600`, 19:38:42 to 20:39:04: **passed** in 3619 s. The flood ran the full hour, the marker after it arrived, and the shell's exit status (7) arrived after it. Daemon (pid 924), sampled once a minute:

| | private | working set | handles |
|---|---|---|---|
| minute 1 | 166.9 MB | 133.3 MB | 215 |
| minutes 3 to 58 | 145.5 to 191.8 MB, mean 189.3 MB | 84.1 to 175.7 MB | 213 |
| after the flood | 116.0 MB | 79.3 MB | 213 |

Flat: no growth over the hour. But this is the **overload path**, not the exit check: the test client fell behind (below) and the daemon held its backlog at the per-connection cap the whole time. It is recorded as such; the exit check is the soak with a client that keeps up, after the fix.

### Why ~190 MB: the breakdown

`crates/yolmira/tests/memory.rs` (`YOLMIRA_MEMPROBE=<flood seconds>`) measures the daemon's private bytes at each stage, on its own daemon:

| Stage | debug (60 s flood) | release (30 s flood) |
|---|---|---|
| idle, no sessions | 7.9 MB | 7.9 MB |
| 1 PowerShell session, client attached | 8.7 MB | 8.5 MB |
| after the flood, client keeping up | 12.9 MB | 10.7 MB |
| 8 sessions | 14.5 MB | 12.6 MB |
| flood with a **slow client** (reads one notification per 20 ms) | **194.1 MB** | **181.9 MB** |
| slow client gone | 16.5 MB | 14.9 MB |

- **Debug build:** no; release behaves the same.
- **SQLite cache:** default (about 2 MB); no pragmas set.
- **Scrollback:** 2 MiB per session, as designed.
- **Soak output held in memory:** yes, but per slow client, not the hour's output. The soak's test client searched all text received so far after every chunk (`expect_within`), which grows with the output; it fell behind, and the daemon kept it fed with scrollback replays. A connection's outbox was capped at 256 *messages*, not bytes: up to 256 lines of 256 KiB replay chunks (~350 KB each in base64 JSON, ~512 KiB allocated after doubling) ≈ 128 MiB, plus the session's broadcast buffer (1024 chunks of up to 16 KiB ≈ 16 MiB), the ring and its snapshot (≈ 4 MiB) and the baseline (8 MB): ≈ 156 MB by arithmetic; the remaining 25 to 35 MB is not itemized (likely allocator overhead).
- Memory returns when the slow client leaves (16.5 MB), so it is bounded and flat, but large, and a real UI that falls behind could cost this per attached shell. Fixed in the output-budget commit (decisions.md 2026-10-09).

### The output-budget fix

Reviewer, 2026-10-09: done before Phase 1 closes. Code: `crates/yolmira/src/daemon/{outbox.rs, slots.rs}` and `session_handlers::forward`.

- **Per-connection byte budgets** replace the 256-message cap. Shell output: 8 MiB per connection. Responses and events: 256 KiB per connection, and senders wait for room. Only the connection's own tasks wait; the shared event broadcast never waits on a client.
- **Daemon-wide output budget:** 64 MiB across connections. It counts each connection's queued output beyond its first 512 KiB, so slow clients filling it can't get a client that keeps up refused.
- **A slow client:** when it is over budget (or the broadcast lagged), its queued output for that shell is dropped and it is marked behind. Nothing more is queued for it until a whole snapshot fits. Then it gets one fresh scrollback snapshot (`reset`), with a new subscription taken atomically, so there is no gap and no repeat. At most one snapshot is pending per client and shell; copies are never queued. The attach replay takes the same path. At most 4 snapshots are copied and encoded at once, daemon-wide.
- **The line being written** counts against the budgets until its write completes.
- **Stalled clients:** a client stuck for 30 s is disconnected and an `ipc.client_stalled` event is logged. Stuck means one line not taken in that time, or a response or event waiting that long for room.
- **Connections:** 128 for everyone; at most 16 per process; 48 more reserved for the verified UI (which may use 48 per process: a window plus one per attached shell). Refusals are logged as `ipc.auth_failed` with reason `too_many_connections` or `too_many_connections_from_process`.
- **Per-shell broadcast buffer:** 1024 → 256 chunks. Forwarders no longer wait on clients, so they lag only when starved.
- **Test helper:** `expect_within` searches only new text (it was quadratic). `Shell::text` keeps the latest 8 to 16 MiB. `Shell::resets` counts resyncs.

**True worst case of the daemon's memory** with these limits: 176 connections (128 + 48 reserved), 32 live shells (`MAX_LIVE_SESSIONS`), every client misbehaving at once:

| Item | Bound | Inside a budget? |
|---|---|---|
| Queued shell output | 64 MiB shared + 176 × 512 KiB floors = 152 MiB | yes |
| Queued responses and events | 176 × 256 KiB = 44 MiB (plus at most one larger line each; real lines are small) | yes |
| Snapshots being copied and encoded | 4 × (2 MiB + ~2.8 MB) ≈ 19 MiB | bounded by the permits |
| Per-shell broadcast buffers | 32 × 256 × 16 KiB = 128 MiB | **no**: held per shell whether or not anyone reads |
| Scrollback rings | 32 × 2 MiB = 64 MiB | **no**: by design |
| Request lines being read | 176 × 1 MiB (`MAX_LINE`) = 176 MiB, only while clients send huge requests | **no** |
| Baseline | ~8 MB | |
| **Total** | **≈ 590 MiB**, plus allocator overhead | |

In normal use (clients that keep up) the daemon stays in the tens of MB (memory probe above). The largest terms outside any budget are the request-line limit and the broadcast buffers. Follow-ups, not now: a smaller request-line limit for everything except `session.input`; binary or raw framing for output instead of base64 in JSON (base64 is what turns a 256 KiB chunk into ~350 KB).

**Results** (`2ccd493` to `e3920c5`):

- **Unit tests:** `outbox` (9 tests: exact accounting, all-or-nothing, the in-flight line, order kept when one session's output is dropped, control backpressure, waking on room, the per-connection floor, stalls, close) and `slots` (3 tests).
- **Integration tests** (`output_budget.rs`), all pass in parallel runs:

  | Test | connection high water | daemon high water | resyncs |
  |---|---|---|---|
  | `slow_client_stays_within_its_budget` | 8.0 of 8.0 MiB | 7.5 of 64.0 MiB | 117 to 129 |
  | `many_slow_clients_stay_within_the_daemon_budget` (12 slow clients on 3 shells, plus a fast one) | 7.9 to 8.0 of 8.0 MiB | 53.5 to 64.0 of 64.0 MiB | 510; the fast client 0 |
  | `fast_client_on_the_same_shell_is_unaffected` | 8.0 of 8.0 MiB | 7.5 of 64.0 MiB | the fast client 0 |
  | `resynced_client_output_is_correct_and_gap_free` | | | the slow client's text after its last snapshot is exactly the end of the fast client's |

- **Connection tests** (`connections.rs`), 3/3 several times:
  - a never-reading client is disconnected after 30 s as "not reading" and logged as `ipc.client_stalled`; meanwhile the UI got its approval prompt in 0.9 to 1.2 ms and answered it, before and after the disconnect;
  - one process opening 128 connections gets 16, and the UI still connects;
  - 8 processes fill all 128 slots, a 9th gets none, and the UI still gets in through its reserve.
- **A client slower than its shell is resynced repeatedly:** about 8 times a second for the 20 ms-per-read test client. Each resync waits until that client has drained room for a whole snapshot, so the rate is paced by the client's own reading, and memory stays within the budgets. **Corrected 2026-10-23:** not paced by the client; a resync loop (below, "Open: resync loop").
- **Whole suite:** `scripts/test.ps1` (17 test binaries) passed in 3 of 4 runs after the fix. E2E 9/9 (46.6 s). UI unit tests 5/5. App crate 13/13. clippy and fmt clean.

**Open: one daemon crash, cause unknown.** In 1 of 4 full-suite runs, `a_never_reading_client_is_disconnected_and_the_ui_still_answers_prompts` failed: about 22 s into the test its daemon was no longer reported as running. The helper then started a new `yolmirad` on the same home, which crashed during startup:
- exception `0xC0000409` at offset `0x6c80d1` inside `yolmirad.exe` (Windows Application log, 20:48:49);
- the process had started at 20:48:49.407.

On Windows, Rust aborts with that code, for example after a stack overflow or a panic during a panic. No trace survived: a detached daemon's stderr went nowhere, its log writer is non-blocking, and the test deleted its home. Not reproduced in 3 direct runs of that binary, 3 parallel runs of the budget tests, or 3 more full-suite runs. A second `yolmirad` on a live instance exits cleanly ("another yolmirad already serves this instance"), so that path isn't it.

Since `e3920c5`, a detached daemon's stderr and panics (with backtrace) go to `logs\yolmirad-stderr.log`, written synchronously (test `crash_log_records_a_panic`), and a failing test keeps its home and prints the path. If it happens again, the reason will be on disk.

## After the output-budget fix (2026-10-22)

Run at `e63f713`, whose code is that of `1a4e51d` (the history rewrite changed only Markdown, `.gitleaksignore` and old fixture lines). The test binaries were those built on 2026-10-09 from that code; `cargo test --no-run` rebuilt nothing.

### 1-hour soak with a client that keeps up

`pty.exe soak_1h_output_flood --nocapture` with `YOLMIRA_SOAK=3600`, started 16:03: **passed** in 3600.6 s. The flood ran the full hour, the markers after it arrived, and the shell's exit status (7) arrived after them. The test's own report:

- **The client kept up:** 0 resyncs beyond the attach replay; its connection's queued output peaked at 121 KiB of the 8 MiB budget; the daemon-wide budget was never used (high water 0).
- **Daemon** (pid 27644), sampled once a minute by a separate script:

  | | private | working set | handles |
  |---|---|---|---|
  | minute 1 (startup) | 5.4 MB | 14.7 MB | 106 |
  | minutes 2 to 60 (59 samples) | 10.7 to 11.3 MB, mean 10.8 MB | 24.7 to 25.0 MB | 210 to 212 |
  | last sample | 10.8 MB | 25.0 MB | 210 |

Flat for the hour, and about 18× less than the overload-path soak's ~190 MB. An earlier run the same day (through `scripts/test.ps1`, before the history rewrite, the same code) also passed in 3600.7 s, but the harness hid the passing test's report and no samples were taken; it is not counted.

### WSL terminate

`wsl_shutdown_while_attached` **3/3** (0.5 to 0.7 s): once warm, twice after `wsl --terminate amiri-throwaway`. The throwaway distro had to be rebuilt first: its disk had been in a temporary folder that was cleaned, and the first attempt failed on WSL's `MountDisk/ERROR_FILE_NOT_FOUND`, before any Yolmira code ran. Rebuilt from the same Alpine 3.24.2 minirootfs (SHA-256 `c5ca053c…90388677` matched the published checksum and the 2026-10-09 record; GPG signature good, Alpine's key `0482 D840 22F5 2DF1 C4E7 CD43 293A CD09 07D9 495A`), now kept in `%LOCALAPPDATA%\amiri-throwaway`. The user's distros were untouched.

### CI (first run, public repository)

Run `39764484176` on `e63f713` (GitHub-hosted `windows-latest`, 2 vCPU): every test binary passed except `output_budget`, where `many_slow_clients_stay_within_the_daemon_budget` failed its precondition, not a budget: connection high water 5.4 of 8.0 MiB, daemon high water 58.2 of 64.0 MiB, **0 resyncs**. On the runner the three floods wrote too little for 12 slow clients to overrun a budget, so the test proved nothing and said so (its message wrongly reads "fell behind"). Locally the same test resyncs about 510 times. Fix proposed: slower slow clients in that test, and a correct message.

The second run (`39775486129`, on `805cb8a`) failed the same test differently: the slow clients did fall behind (26 resyncs, daemon high water 64.0 of 64.0 MiB) but the **fast client was resynced** once, which the test forbids. So the proposed fix above was wrong and is withdrawn: it would have hidden this. Not reproduced locally in 3 runs pinned to 2 cores, nor in 3 pinned to 1 core.

### Open: resync loop (found 2026-10-23)

Diagnostics added (`6b92e51`, `cfda73d`; no behaviour change): the daemon logs each attachment's first fall behind at info (later ones at debug) with its cause, its connection's queued bytes and the daemon-wide budget in use, and `debug.output_stats` counts the causes. When the fast client is resynced, the test's failure message now includes those lines, so the next CI failure shows why.

Local results (`output_budget`, all 4 tests pass):

| Test | resyncs | refused by a budget | lagged the shell | of those, before passing on anything after a snapshot |
|---|---|---|---|---|
| `slow_client_stays_within_its_budget` | 228 | 1 | 227 | 227 |
| `fast_client_on_the_same_shell_is_unaffected` | 232 | 1 | 231 | 231 |
| `many_slow_clients_stay_within_the_daemon_budget` | 830 | 12 | 818 | 818 |

A client falls behind on a budget once; after that, **every** resync is followed at once by a broadcast lag. `catch_up` takes the snapshot and a new subscription atomically, then encodes the 2 MiB snapshot; while it encodes, a flooding shell writes more than the per-shell broadcast holds (256 *chunks*, not bytes, and the chunks are small), so the new subscription has overflowed before the forwarder reads it. While the shell stays busy, a client that fell behind once gets only back-to-back snapshots, never live output, and each costs a ~2.8 MB encode (about 830 in 15 s in the 12-client test, ~2.3 GB encoded). Likely, not yet confirmed: on the 2-vCPU runner that CPU starves the fast client's forwarder past 256 chunks, which resynced it.

Fix options, for review (not built): encode the snapshot while the forwarder keeps the new subscription's chunks (within the budgets) and queue them after it; size the broadcast window in bytes, or merge small PTY reads into larger chunks, rather than counting chunks. The old 1024-chunk buffer only made the window larger.

## Earlier automated runs (old name, superseded)

**Superseded by the rename (decisions.md 2026-10-09, AMIRI → Yolmira).** The results below were run on the old-name build and are kept as a record. The exit checks are rerun on the renamed build, and those results are the ones that count. Also from the old build: `wsl_shutdown_while_attached` passed 3 times on the throwaway distro `amiri-throwaway` (Alpine 3.24.2 minirootfs, SHA-256 `c5ca053c…90388677`, GPG signature by Alpine's key `0482 D840 22F5 2DF1 C4E7 CD43 293A CD09 07D9 495A` checked), once warm and twice from a stopped distro. A soak was stopped after 12 min for the rename: by then the daemon's private memory was flat at about 190 MB from minute 2 on, with 213 to 215 handles.

### Close UI / reopen / shell survives, fresh build

`dea6b49`, the Yolmira crates rebuilt from scratch (`cargo clean -p yolmira`, then `npm run e2e`; the harness prints each binary's build time against its newest source: `yolmirad.exe` 13:53:54Z, sources 13:37:58Z; `yolmira-app.exe` 13:54:04Z, sources 13:40:11Z). **9/9 pass**, 51.0 s, windows off-screen, no leftover processes: `ui_close_reopen_shell_survives`, `ui_killed_shell_survives`, `ui_started_from_closed_terminal`, `two_uis_same_session`, `daemon_down_banner_and_reconnect`, `output_flood_flows_through`, `ui_approval_prompt`, `project_picker`, `config_edit_needs_confirmation`.

Note: a first `cargo clean -p yolmira` printed nothing (output discarded) and the run used the 18:38 binaries from `98f017a` (no Rust change since; also 9/9). The clean was repeated visibly ("Removed 13572 files") and the suite rerun; the result above is that rerun.

## Logoff while the daemon runs detached (plan §4, manual check 2)

Expected: the daemon notices the session ending and stops cleanly; the event log shows `daemon.stopped` with reason `session_end`.

1. Save your work in other programs; this signs you out.
2. In a normal (not elevated) terminal, in `C:\Users\User\Desktop\AMIRI`:
   ```
   .\target\debug\yolmira.exe daemon start
   .\target\debug\yolmira.exe shell new --kind cmd
   .\target\debug\yolmira.exe daemon status
   ```
   Note the daemon's pid from `daemon status`.
3. Close the terminal (the daemon keeps running, detached).
4. Sign out: Start → your account picture → **Sign out** (not Lock, not Switch user).
5. Sign back in. Open a terminal in the repo:
   ```
   .\target\debug\yolmira.exe events --kind daemon. --limit 1000
   .\target\debug\yolmira.exe daemon status
   ```
6. Expected: the last `daemon.stopped` line has reason `session_end` and comes after the `daemon.started` with the pid you noted. `daemon status` says the daemon is not running (nothing starts it at logon in Phase 1).
7. Paste me the output of step 5.

## Pipe-owner rejection with a second Windows account (H1, plan §4, manual check 1)

Expected: your `yolmira` refuses a pipe that another account created under your daemon's name, sends it nothing (no token), and no second daemon starts.

The script is `scripts/manual/impostor-pipe.ps1` (no elevation). Self-tested under one account on 2026-10-09: a same-user client is accepted and the script logged "131 bytes received" (a fake token), so the byte count works; under a second account it must log 0.

1. **Create a second local account** if you have none (needs your admin password in the Windows dialog; I don't run anything elevated): Settings → Accounts → Other users → Add account → "I don't have this person's sign-in information" → "Add a user without a Microsoft account". A standard user is enough. Optional: in that account set Region → Regional format to Uzbek (Latin) to note the uz-UZ behaviour too.
2. **As yourself**, in the repo:
   ```
   [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
   .\target\debug\yolmira.exe daemon status
   Copy-Item scripts\manual\impostor-pipe.ps1 C:\Users\Public\
   ```
   Write down the SID (starts with `S-1-5-21-`). If the daemon runs, stop it: `.\target\debug\yolmira.exe daemon stop` (stopping asks for approval by default: approve in the Yolmira window, or with the code it shows).
3. **Switch user** (Start → your picture → the other account; do NOT sign out, your session must stay). In the second account, open Windows PowerShell and run, with your SID:
   ```
   powershell -NoProfile -ExecutionPolicy Bypass -File C:\Users\Public\impostor-pipe.ps1 -Sid S-1-5-21-...
   ```
   It prints "impostor pipe ... created by S-1-5-21-...(the second account's SID)" and waits 15 minutes.
4. **Switch back to yourself** (leave the script running) and run in the repo:
   ```
   .\target\debug\yolmira.exe daemon status
   .\target\debug\yolmira.exe daemon start
   Get-Process yolmirad -ErrorAction SilentlyContinue | Select-Object Id, Path
   Get-Content C:\Users\Public\yolmira-impostor.log
   ```
5. Expected:
   - both `yolmira` commands fail with "the daemon's pipe is owned by another process: ...";
   - no `yolmirad` from `C:\Users\User\Desktop\AMIRI\target\debug` is running (test daemons from a running soak have their own pipe names and don't count);
   - every "connection N" line in the log says **0 bytes received**.
6. Switch to the second account, press Ctrl+C in its PowerShell (or wait 15 min), sign that account out. Paste me the output of step 4.
7. Afterwards: `Remove-Item C:\Users\Public\impostor-pipe.ps1, C:\Users\Public\yolmira-impostor.log`. Delete the second account if you don't want to keep it.
