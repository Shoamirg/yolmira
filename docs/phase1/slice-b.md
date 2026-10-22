# Phase 1, Slice B: terminal sessions from the CLI (findings)

- Date: 2026-09-25. Plan: `docs/phase1-plan.md` §1 Slice B.
- Code: commit `768bc57`. **Recorded run** on that clean commit, non-admin (Medium), ru-RU: `scripts/test.ps1`, exit 0, **all 12 test binaries pass; 3 tests not run** in this environment (listed below, each with its reason).
- Stability: after the fixes, `pty` (13 tests) and `sessions` (9 tests) ran **10 times in a row clean**.
- **CI** on `5226c24` (en-US, elevated runner): green, run `36150995203`. 76 run and passed; 7 not run, each listed with its reason (4 need breakaway, no WSL distro on the runner, soak, WSL-terminate).
- Reviews before the commit: `rust-reviewer` and `security-reviewer` (read-only). All HIGH and MEDIUM findings fixed in `768bc57`; deferred items under "Open".

## Exit tests (the PTY integration suite and session lifecycle)

| Plan test | Where | Result |
|---|---|---|
| `echo_round_trip_{cmd,powershell}` | `crates/yolmira/tests/pty.rs` | pass. Markers the shell computes (`YOLMIRA_%OS%`, `6*7`) |
| `ansi_color_semantics` | pty | pass. The foreground attribute before the marker is parsed and must be red (ConPTY re-encodes: 31/91/38;5;1/38;5;9/RGB) |
| `resize_reported_by_child` | pty | pass. `[Console]::WindowWidth/Height` = 100x30 after `session.resize` |
| `ctrl_c_interrupts_ping` | pty | pass. Also exercises the S1d flag clearing: the daemon was started with `CREATE_NEW_PROCESS_GROUP`, and its shells still get Ctrl+C |
| `exit_codes` | pty | pass. `exit 7` → notification, event and `shell list` all show 7 |
| `dsr_answered_without_client` | pty | pass. A shell created with nobody attached is usable 3 s later |
| `wsl_session` | pty | pass on this machine (default distro, `echo YOLMIRA_$((6*7))`). Not run where no distro is registered (read from `HKCU\…\Lxss`, locale-neutral) |
| `pwsh_session` | pty | **not run**: PowerShell 7 isn't installed (R7 stays open for `pwsh`) |
| `oem_codepage_output_decoded` | pty | pass. `echo Привет_%OS%` in cmd (OEM 866 on this machine) arrives as UTF-8 |
| `non_ascii_paths` → `non_ascii_cwd` | pty | pass. Working folder `…\Проект oʻzbek` |
| `soak_1h_output_flood` | pty | **not run** by default (`YOLMIRA_SOAK=<seconds>`; 3600 at the phase exit, once) |
| `session_survives_client_disconnect` | `crates/yolmira/tests/sessions.rs` | pass. Killed client, reattach: variable intact, scrollback replayed |
| `daemon_restart_reconciles_sessions` | sessions | pass. Daemon killed (`taskkill /F`); on restart RUNNING → `session.unknown` → `session.exited {reason: daemon_restart}`; never shown RUNNING |
| `wsl_shutdown_while_attached` | sessions | **not run**: `wsl --shutdown` would stop the user's distros too; the test only terminates a throwaway distro named in `YOLMIRA_TEST_WSL_DISTRO` (plan deviation, below) |
| `child_env_is_allowlisted` (daemon level, moved from Slice A) | sessions | pass. The launcher's environment carries `YOLMIRA_TEST_SECRET` and a junk PATH entry; a real cmd in the daemon sees neither |
| Added | sessions: `attach_after_more_output_than_one_frame` (>1 MiB replay), `attach_and_detach_are_recorded_without_keystrokes`, `clean_stop_records_session_exits`, `exited_sessions_reject_input_and_attach`, `cli_attach_round_trip` (the real `yolmira shell attach` inside a pseudoconsole: type, see output, Ctrl+] → exit 0, shell still RUNNING); unit tests for the DSR filter, detach detection, cwd/distro validation, elevated pwsh lookup | pass |

## Findings

1. **ConPTY's startup cursor request is its own handshake, not the application's.** ConPTY answers applications' cursor queries internally; the `ESC[6n` it emits at startup is for the terminal. Early design had "the daemon answers when nobody is attached, clients answer otherwise", which raced (double answers become keystrokes) and stalled when a client was slow to answer. Final design: the PTY layer always answers and strips it (including when split across reads). Clients never see it.
2. **ConPTY asks the outer terminal for win32-input-mode** (`ESC[?9001h`, also focus events `?1004h`). Passed through by `yolmira shell attach`, it switches Windows Terminal (or any ConPTY host) to encoding every key as `ESC[vk;sc;uc;kd;cs;rc_`. Consequences: the detach key is recognised in both encodings, and the CLI resets these modes (and alt screen, bracketed paste, mouse, cursor/keypad, scroll region, charset) when it exits, on every path.
3. **ConPTY's output doesn't reach EOF when the shell exits**, only when the pseudoconsole closes. The watcher closes it and waits for the reader to drain before reporting the exit, so the last lines of output aren't lost.
4. **Test flakiness found and fixed:** under parallel load `ping` sometimes started after the test's fixed 2.5 s wait, so Ctrl+C went to cmd instead. The test now waits until ping replies are visible (the address digits are locale-neutral). One earlier stress run showed three PTY failures of which only this one's message was captured; the 10 runs after the fix were clean.

## Review findings and what was done

| Source | Severity | Finding | Done |
|---|---|---|---|
| both | HIGH | 2 MiB scrollback replay in one attach reply exceeded the 1 MiB frame: a busy session could never be attached again | Replay/resync as ≤256 KiB `session.output` notifications (first with `reset`); test with >1 MiB |
| rust | HIGH | A shell exiting before it was in the session map left a zombie (pty never closed) | Exit handling closes the pty in the PTY layer; `create` removes a session that exited before insertion |
| rust | HIGH | `?` inside the attach loop skipped restoring the user's terminal (e.g. typing `exit`) | Restore in the console guard's `Drop`; input/resize errors end the attach cleanly |
| security | MEDIUM | Elevated daemon took `pwsh.exe` from user-writable PATH folders (EoP) | `pwsh` from Program Files (known folder) first; PATH only when not elevated; test with a planted `pwsh.exe` |
| both | MEDIUM | Blocking pipe writes / process creation / SQLite on async workers; DSR reply written under the output lock | `spawn_blocking` for create/list/input/resize/kill; DSR handled in the PTY reader, no lock held |
| rust | MEDIUM | Exit reported before the last output; double DSR answers with two clients; split DSR missed | Drain before exit; DSR stripped and answered once in the PTY layer with carry-over |
| rust | MEDIUM | `ReadFile` on console input can return 0 without EOF (non-ASCII, UTF-8 CP) | `ReadConsoleW` + own UTF-8 encoding, surrogate pairs across reads |
| both | MEDIUM | No session limit; poisoned map lock silently dropped sessions | 32 live shells; poison-tolerant locks |
| security | LOW | cwd unvalidated; distro unvalidated; program folders from the caller; cwd exe lookup | Absolute drive/UNC folders only (no `\\?\`, `\\.\`); distro `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`; `ProgramFiles*`/`ProgramData` from known folders; `NoDefaultCurrentDirectoryInExePath=1` |
| both | LOW | `session.list` unbounded; `kill` after exit; stale attachment handles; base64 errors became empty output; functions over 50 lines | Running + 200 most recent ended; `kill`/`input`/`attach` refuse exited sessions; finished handles pruned; decode errors end the attach with a message; `attach` and `create` split |

## Where the scrollback lives

- The 2 MiB ring per session is **daemon memory only**: never written to the event log, the database, the daemon's log file or any other file (`session.input` is unlogged; no log line carries output or keystrokes). It is dropped when the session ends: on exit the session leaves the live map, and the memory is freed once the last attachment's handle is gone. A daemon restart loses it (sessions are reconciled to EXITED, not replayed).
- Not done: the freed memory isn't zeroed, so it could survive in a crash dump or the page file until reused. Acceptable for Phase 1; if scrollback is ever persisted (e.g. replay after a daemon restart), it gets the event log's redaction and retention rules first.

## Plan deviations

- `wsl_shutdown_while_attached` uses `wsl --terminate <throwaway distro>`, never `wsl --shutdown` (which would stop the user's running distros, including `docker-desktop`), and runs only when `YOLMIRA_TEST_WSL_DISTRO` names one.
- Detach records `reason: "disconnect"` rather than `"detach"` (the CLI drops its output connection instead of sending `session.detach`). Accepted for Phase 1.

## Open

- `pwsh` and the 1 h soak: run at the phase exit (PowerShell 7 must be installed for `pwsh_session`; otherwise R7 stays open for pwsh).
- `wsl_shutdown_while_attached`: at the phase exit, reusing the S4 approach: a throwaway distro imported from the verified Ubuntu image, unregistered afterwards; never `wsl --shutdown`.
- After `kill_all`'s 2 s shutdown wait, a shell that hasn't exited may record `session.exited` after `daemon.stopped`.
- Two orphaned processes from 2026-09-24 (a `cmd.exe` started 17:26 and a `conhost.exe` 18:27, parents gone) are probably left from the Phase 0 lifetime spikes. Not touched; gone when checked on 2026-09-26.

## CI

- **First two runs failed on the four Windows PowerShell tests** (`768bc57`, `0d255b2`); one diagnostic run in between passed. The full output showed the command typed and accepted (`\r` after the line), the prompt already there, and then no output for 30 s. Cause: the first command in a fresh Windows PowerShell on a new image builds its module-analysis cache, which is slow; locally the cache is always warm. PSReadLine is 2.0.0 on both (checked; printed by the CI facts step).
- Fixes: tests wait for the unlocalized `PS …>` prompt before typing (up to 120 s); a command round trip allows 90 s instead of 30; CI runs a PowerShell warm-up step first. With these the suite passes; the PTY binary still takes ~70 s on the runner against ~8 s locally, so the warm-up helps less than hoped and the 90 s allowance is what carries it.
- A real hang still fails (each wait is bounded and shows 2,000 characters of output on failure).
- **CI test durations** (recorded per slice from now on, so a jump stands out). Run `36150995203` on `5226c24`, per test binary as reported by the harness: `pty` 70.3 s (locally ~8 s), `sessions` 31.1 s, two others 2.6 s and 1.3 s, the rest under 1 s (the harness output doesn't name the binaries; `scripts/test.ps1` will print names from Slice C on). Slice A had no per-binary record; its baseline is this run.
