# Phase 1 plan: Windows shell (daemon, terminal, UI)

Status: **approved 2026-09-25** with the changes in §6.
Scope source: ARCHITECTURE §16.2 Phase 1. Exit criteria: *PTY integration suite green; UI closes and reopens while a local shell keeps running under `amirid`; the event log shows all actions.*

## 1. Slices

Four vertical slices, in order. Each ends in a working, tested, committed state. A slice starts only when the previous one's exit tests pass on a clean commit, and each slice's test run is recorded like a spike: commit, dirty flag, results.

### Slice A: daemon, IPC, CLI, event log (no UI)

Builds:
- **Workspace skeleton** with only the crates Phase 1 needs:
  - `amiri-core`: action types, policy types, no I/O;
  - `amiri-store`: SQLite, migrations, the `events` table and a `sessions` projection (a subset of §7.2);
  - `amiri-ipc`: protocol types;
  - `amirid`;
  - `amiri-cli`.

  Other crates from §4.2 are added in the phase that needs them.
- **`amirid` lifecycle** (§2 below): detached start, single instance, S1d Ctrl+C handling, elevation refusal, allowlisted child environment.
- **IPC:** newline-delimited JSON-RPC 2.0 over `\\.\pipe\amiri-<user SID>`.
  - The pipe has a DACL granting only the current user, and rejects remote clients (`PIPE_REJECT_REMOTE_CLIENTS`). The daemon creates it with `FILE_FLAG_FIRST_PIPE_INSTANCE`, so no other process can squat the name.
  - The first message must carry the per-boot token from `%LOCALAPPDATA%\AMIRI\run\token`, which has a user-only ACL.
  - A protocol version is exchanged in the handshake.
  - Requests and responses, plus subscriptions (event stream, PTY output).
- **Event log:** every state change is an event appended in the same transaction as its projection update. The DB uses WAL mode, and embedded numbered migrations run with a backup first.
- **CLI:** `amiri daemon start|stop|status`, `amiri events [--follow] [--kind …]`.

Exit tests (`crates/amirid/tests/lifecycle.rs`, `crates/amiri-store/tests/`):

| Test | Proves |
|---|---|
| `daemon_survives_launcher_job_close` | The launcher runs inside a kill-on-close job that **allows breakaway** (like the jobs terminals and IDEs use), starts the daemon, and then the job is closed. The daemon is still reachable 5 s later. |
| `restrictive_job_reports_manual_start` | The launcher runs inside a job that **forbids breakaway**: `ensure_daemon()` starts nothing and returns the manual-start error, with a non-empty `why` and `next_steps`. |
| `daemon_survives_launcher_console_close` | Launched from a ConPTY that is then closed, as in the S1 method: the daemon survives. |
| `second_start_connects_to_existing` | `amiri daemon start` twice gives one daemon PID. The second call reports "already running" and exits 0. |
| `concurrent_start_single_daemon` | 8 parallel `ensure_daemon()` calls produce exactly one daemon. The losers connect to it. |
| `pipe_dacl_is_current_user_only` | Reads back the pipe's security descriptor: exactly one allow ACE, for the current SID, and remote clients rejected. (We can't create a second Windows user without admin, so the ACL is checked by inspection.) |
| `ipc_rejects_missing_or_wrong_token` | Connection closed, and a `ipc.auth_failed` event recorded. |
| `ctrl_c_handling_s1d` | S1d's three checks ported: a child shell gets Ctrl+C; the daemon survives Ctrl+C in its own console (`--foreground`); console close shuts it down cleanly with exit 0. |
| `child_env_is_allowlisted` | The daemon starts with canary `AMIRI_TEST_SECRET=…` and a junk PATH entry. A child shell sees neither, and its PATH equals the registry PATH. |
| `elevation_check` | The decision function refuses an elevated token without `--allow-elevated` (token facts injected), and accepts the real Medium token. Nothing runs elevated. |
| `events_persist_across_restart` | Events written, daemon stopped and restarted: the same events come back in the same `seq` order. |
| `migrations_apply_and_backup`, `projection_rebuild_matches_live` | §15 Store rows. |

### Slice B: terminal sessions from the CLI

Builds:
- **`amiri-exec`:** local ConPTY through `portable-pty` 0.9.0 (S1).
  - Session kinds: `cmd`, `powershell` (5.1 by full path; S0 found it missing from PATH), `pwsh` if installed, `wsl` (`wsl.exe -d <distro>`).
  - Each session has a daemon-side scrollback ring (default 2 MiB, raw bytes) that is replayed on attach, so a client that reconnects rebuilds the screen.
  - The daemon answers ConPTY's DSR (`ESC[6n`) itself when no client is attached (S1 finding 5).
  - Several clients may view one shell; input from any of them goes to the PTY in arrival order. (The one-writer lock of D2 is for agent sessions, which come in Phase 2.)
- **CLI:** `amiri shell new --kind … [--cwd …]`, `amiri shell list`, `amiri shell attach <id>` (raw-mode terminal; a detach key detaches without killing), `amiri shell kill <id>`.
- **Events:** `session.created`, `session.attached`/`detached`, `session.resized`, `session.exited {code}`. Keystrokes are **not** logged, since they may contain secrets. Session output is not stored in the log either; the scrollback is memory only.
- **Reconciliation:** on daemon start, sessions recorded as RUNNING become `UNKNOWN`, then `EXITED (daemon restart)` (D3: local shells die with the daemon, S1).

Exit tests (`crates/amiri-exec/tests/pty.rs`, `crates/amirid/tests/sessions.rs`). Together they form the **PTY integration suite** (§15):

| Test | Notes |
|---|---|
| `echo_round_trip_{cmd,powershell}` | Asserts on a marker the shell computes (`%ERRORLEVEL%`-style arithmetic), never on a label. |
| `ansi_color_semantics` | Asserts that the colour *attribute* is set (parsed), not the bytes: ConPTY re-encodes them (S1). |
| `resize_reported_by_child` | `[Console]::WindowWidth` / `mode` digits only. |
| `ctrl_c_interrupts_ping` | `ping -t` interrupted; exit code checked. |
| `exit_codes` | `cmd /C exit 7` → 7. |
| `dsr_answered_without_client` | The shell doesn't hang with no client attached. |
| `wsl_session` | Runs `echo $((6*7))` in the default distro. Skipped with a recorded reason if no distro exists (R7). |
| `pwsh_session` | Skipped with a recorded reason if `pwsh` isn't installed (it isn't on this machine: R7 stays open for `pwsh`). |
| `session_survives_client_disconnect` | Client sets a shell variable, disconnects (killed, not a clean detach), a new client attaches: the variable is intact and the scrollback is replayed. |
| `daemon_restart_reconciles_sessions` | RUNNING → UNKNOWN → EXITED, with events. |
| `wsl_shutdown_while_attached` | `wsl --shutdown` during a WSL session: the session goes to EXITED and the daemon stays up (R7). Touches only a throwaway distro, never the user's. |
| `soak_1h_output_flood` | Gated by `AMIRI_SOAK=1`. Run once before the phase exit: no memory growth beyond the ring, no stall, no lost exit status (R7). |

### Slice C: the Tauri UI attaches to sessions

Builds:
- **`app/`:** Tauri 2 + React + TypeScript + Vite, with `@xterm/xterm` and the fit addon.
  - The Tauri Rust side is an IPC client of `amirid`. The webview talks only to Tauri commands and events, with no shell or filesystem plugins (§6.5).
  - The UI **never owns the daemon.** At start it calls the same `ensure_daemon()` as the CLI, and it never kills the daemon when it closes.
  - Screens: session list, terminal tabs (new, attach, detach, kill), and a daemon-disconnected banner with automatic reconnect.
  - No component library (§5.2).

Exit tests (`app/e2e/`, WebDriver through WebView2's `msedgedriver`; not `tauri-driver`, see decisions.md 2026-10-05):

| Test | Proves |
|---|---|
| `ui_close_reopen_shell_survives` | **The main exit criterion.** Launch the UI, open a `cmd` tab, `set AMIRI_MARK=42`, close the window normally. Relaunch the UI and attach: `echo %AMIRI_MARK%` gives `42`, and the earlier output is visible (replayed). |
| `ui_killed_shell_survives` | Same, but the UI process is killed (`taskkill /F`). |
| `ui_started_from_closed_terminal` | Start the UI from a console, close the console: the UI's shells keep working (covers the daemon's launch path through the UI). |
| `two_uis_same_session` | Two windows attached to one shell both see output. |
| `daemon_down_banner_and_reconnect` | Kill the daemon: the banner shows, sessions show UNKNOWN (never RUNNING), and after restart the UI reconnects. |

### Slice D: policy engine, project registry, `amiri doctor`

Builds:
- **Minimal policy engine (§6.3):**
  - Typed actions for what Phase 1 does: `session.create`, `session.kill`, `daemon.stop`, `project.register`, `project.remove`.
  - **System-level changes are always ASK** (reviewer, 2026-09-25, after the ComSpec/PATH repair was run elevated with only the UAC click as a check): `system.elevate` (starting anything elevated) and `system.env.write` / `system.registry.write`. The prompt shows the exact commands before any UAC dialog appears; a narrower rule can't loosen it to ALLOW.
  - Rules in `%APPDATA%\AMIRI\policies.toml` (TOML, decisions Q4); scopes resolve project → global. Unknown actions → ASK.
  - ASK blocks the request until a client answers: a prompt in the UI, `amiri approve <id>` or an interactive prompt in the CLI. Approval scope is once or this session.
  - Every decision is a `policy.decision` event. The destructive git classifications are in the table already (tested), even though git arrives in Phase 8.
- **Project registry and selection:** `amiri project add <name> <path>`, `list`, `remove`, and choosing a project so that new shells start in its folder. The UI gets a project picker.
- **`amiri doctor` v1** (table output, `--json`):
  - daemon reachable and its version;
  - pipe ACL;
  - DB `integrity_check` and migration level;
  - elevation and `EnableLUA`, by SID and not by localized label;
  - stale RUNNING sessions;
  - `claude` found, and its version against the tested range (R4: 2.1.281 tested, 2.1.282 installed means a warning);
  - `codex` found or not (informational);
  - PowerShell path; WebView2 present.

Exit tests:

| Test | Proves |
|---|---|
| `policy_table` (unit) | Scope precedence, unknown → ASK, a narrower scope can't loosen a global DENY, destructive hard-classification. |
| `ask_round_trip` | Client 1's request blocks in ASK; client 2 approves; the action runs. Deny path too. All of it as events. |
| `every_mutating_method_emits_event` | Enumerates the IPC method list. Each method marked mutating is called and must produce its event, and a new method not listed in the table fails the test. **This is the automated check for "the event log shows all actions".** |
| `doctor_detects_faults` | Injected faults: stale RUNNING row, corrupted DB copy, `claude` outside the tested range, `EnableLUA=0` (registry read injected). Each is reported with `why` and `next_steps` (§14). |
| `project_shell_starts_in_repo` | Shell cwd equals the registered path, including a non-ASCII path (`C:\…\Проект`). |

## 2. Daemon lifecycle

- **Who starts it:** whichever client comes first, CLI or UI, through one shared `ensure_daemon()` in `amiri-ipc`. It connects to the pipe; if nothing answers, it takes a named start lock, checks again, and launches `amirid`. Then it waits up to 10 s for the pipe and reports a §14-shaped error if the daemon doesn't come up.
- **Detached:**
  - `CreateProcessW` with `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB`, stdio to null, cwd `%LOCALAPPDATA%\AMIRI`, and the allowlisted environment (§9.6).
  - The daemon is not a child of the UI's console and not in the UI's job.
  - If the launcher's job forbids breakaway, `CreateProcessW` fails with access denied. AMIRI then **does not** try another way out of the job: starting processes through WMI to escape a job is a known malware technique that security tools flag, and it would worsen R12. `ensure_daemon()` returns a §14-shaped error, and the CLI and UI show it: the daemon can't be started from inside this restricted environment, plus the command to start it from a new terminal opened from the Start menu. This case is rare: launching from the Start menu or Explorer doesn't put the UI in a restrictive job. If it turns out to matter, the installer (Phase 10) adds a per-user on-demand scheduled task that starts the daemon.
- **Single instance per Windows user:** a named mutex `Local\AMIRI-amirid-<SID>`, held for the daemon's lifetime, plus the pipe's `FILE_FLAG_FIRST_PIPE_INSTANCE`. A second `amirid` exits with a distinct exit code. Several logon sessions of the same user (RDP plus console) share the pipe name, and the pipe flag still keeps it to one daemon. Tested in slice A.
- **Ctrl+C from the first commit (S1d):** clear the inherited ignore flag (`SetConsoleCtrlHandler(NULL, FALSE)`) and install AMIRI's handler (ignore Ctrl+C and Ctrl+Break, clean shutdown on close, logoff and shutdown). A detached daemon has no console, so the handler only fires in `--foreground` dev mode. The flag clearing matters in both modes, because children inherit it.
- **Logoff and shutdown when detached:** without a console, Windows sends no console control events. The daemon creates a hidden top-level window to receive `WM_QUERYENDSESSION`/`WM_ENDSESSION`, and shuts down cleanly (flush the log, mark sessions). `[UNVERIFIED; a manual check at the phase exit (log off and on, and the event log shows a clean shutdown). An automated logoff test would need a second account.]`
- **Elevation:** refuses to start elevated without `--allow-elevated` (§6.5).
- **Stopping:** `amiri daemon stop` asks for policy approval, since it kills every shell, then shuts down cleanly. The daemon keeps running when the last client disconnects; that's the point. Starting at logon is not in Phase 1.

## 3. Not in Phase 1

Agents (Claude or Codex adapters, handoff packets, the failover of §8.4); AMIRI's MCP server; SSH and remote machines; WSL as an execution *transport* (WSL appears only as a terminal kind); Obsidian and knowledge; skills; git integration; the task state machine and orchestrator; Compute; Credential Manager or any secret storage (Phase 1 has no secrets); `amiri-sessiond`; starting at logon; the installer, packaging and code signing (R12); UI polish or a component library; `cargo deny`/`audit` gates (Phase 9; run once manually at the phase exit and report the result).

## 4. Exit criteria → automated tests

| §16.2 exit criterion | Test(s) |
|---|---|
| PTY integration suite green | Slice B table, including the soak run (`AMIRI_SOAK=1`, once) |
| UI closes and reopens while a local shell keeps running under `amirid` | `ui_close_reopen_shell_survives`, `ui_killed_shell_survives`, `ui_started_from_closed_terminal`; underneath them `daemon_survives_launcher_job_close` and `session_survives_client_disconnect` |
| The event log shows all actions | `every_mutating_method_emits_event`, `ask_round_trip`, `events_persist_across_restart` |
| (§15) locale as a permanent test dimension | See below |

**Locale.** All assertions use locale-neutral values (numbers, exit codes, markers the shell computes, files); never tool labels.
- **ru-RU (CP866):** the whole suite runs on this machine, which is ru-RU with CP866 natively. Two tests exist only for locale: `oem_codepage_output_decoded` (a `cmd` child writes Cyrillic in CP866; the UI and CLI show it correctly) and `non_ascii_paths` (project, cwd and DB path under `…\Проект`).
- **en-US:** the GitHub-hosted Windows runner (§6).
- **uz-UZ:** a Phase 1 gap, recorded in the phase findings and not claimed. Covered in Phase 10's Windows Sandbox clean-install test.

**Manual checks at the phase exit** (no automated test possible on one account; results recorded in the Phase 1 findings):
1. **Pipe-owner rejection (H1)** with a second Windows account (the same one serves the uz-UZ check). While the first user's daemon is stopped, the second account creates `\\.\pipe\amiri-<first user's SID>` (a small PowerShell `NamedPipeServerStream` with an Everyone DACL). The first user runs `amiri daemon status` and `amiri daemon start`. Expected: both fail with "the daemon's pipe is owned by another process", **no token is sent** (the second account's server logs no bytes received), and no second daemon starts.
2. **Logoff/shutdown when detached:** start the daemon, sign out, sign back in. Expected: the event log shows `daemon.stopped {reason: session_end}`.
3. **Detach tests** (`daemon_survives_launcher_*`, `second_start_*`, `concurrent_start_*`): not run on CI (the runner's job forbids breakaway); the local `scripts/test.ps1` run on the exit commit covers them.

## 5. New dependencies

`Cargo.lock` and `app/package-lock.json` are committed. Each dependency gets a row in `docs/dependencies.md` in the slice that first uses it, with the locked version. These versions are current stable as of 2026-09-25:

| Dependency | Version | Slice | Note |
|---|---|---|---|
| `tokio` | 1.53.1 | A | Named pipes via `tokio::net::windows::named_pipe` (no extra IPC crate) |
| `windows-sys` | 0.61.2 | A | Security descriptors, mutex, job objects, `SetConsoleCtrlHandler`, token queries |
| `serde` / `serde_json` | 1.0.229 / 1.0.151 | A | |
| `rusqlite` (`bundled`) | 0.40.2 | A | |
| `uuid` (v7) | 1.26.1 | A | |
| `thiserror` | 2.0.21 | A | |
| `tracing` / `-subscriber` / `-appender` | 0.1.44 / 0.3.23 / 0.2.5 | A | |
| `clap` | 4.6.7 | A | CLI |
| `toml` | 1.1.6 | D | `policies.toml` |
| `tempfile` (dev) | 3.27.0 | A | |
| `portable-pty` | 0.9.0 | B | Same version S1 tested |
| `tauri` / `tauri-build` | 2.11.6 / 2.6.3 | C | Stable 2.x. Tauri 3 is in alpha and not used |
| `@tauri-apps/cli` | 2.11.5 | C | **Installed as an `app/` devDependency** (`npm ci`, run with `npx tauri`), not a global `cargo install`, so the version is locked with the project |
| `@tauri-apps/api` | 2.11.1 | C | |
| `react` / `react-dom` | 19.3.0 | C | |
| `vite` / `@vitejs/plugin-react` | 8.3.1 / 6.1.1 | C | |
| `typescript` | 5.9.3 | C | Not 7.0 (new native compiler, first major); revisit later |
| `@xterm/xterm` / `@xterm/addon-fit` | 6.0.0 / 0.11.0 | C | |
| `zustand` | 5.0.15 | C | |
| `msedgedriver` (tool) | must match WebView2 (154.0.4258.53 at Slice C) | C | Downloaded from Microsoft by `app/e2e/fetch-driver.ps1`; version, zip SHA-256 and signature checked. Drives the E2E tests directly (no `tauri-driver`) |

Downloads for slice C are pinned, with SHA-256 recorded. WebView2 updates itself, so the E2E harness first compares the installed WebView2 version with `msedgedriver --version`. On a mismatch it fails with *"WebView2 is now X, msedgedriver is Y: update the driver"*, not with a WebDriver error.

## 6. Review decisions (2026-09-25)

- **Approved,** with the WMI fallback dropped (§2).
- **CI:** a private GitHub repo with a GitHub-hosted `windows-latest` runner, which gives the en-US pass; local runs cover ru-RU; no self-hosted runner. Before the first push: a `gitleaks` scan of the full history, and a list of personal details in the findings files (username, machine name, paths) for the user to review.
- **Slice C downloads:** approved, pinned, with checksums recorded and the version-mismatch check above.
- **uz-UZ:** a Phase 1 gap. It joins the Phase 10 clean-install test in Windows Sandbox.
