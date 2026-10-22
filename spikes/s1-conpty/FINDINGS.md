# Spike S1 — ConPTY via portable-pty: findings

- Date: 2026-09-24
- Machine: Windows 10.0.26200.9457 (Windows 11), ru-RU locale (OEM code page 866), process elevated (Administrator)
- Crate: `portable-pty` 0.9.0
- Raw evidence: `findings.json`: rerun on clean commit `62daad6` (no tracked changes), exit 0, 8/8 checks. The first runs were on an uncommitted tree and are superseded.
- Non-admin rerun (2026-09-24, Medium integrity, UAC re-enabled): clean commit `d764f5f`. `findings.json` = default run (7/8; Ctrl+C fails), `findings-ctrlc-restored.json` = same build with `AMIRI_S1_RESTORE_CTRL_C=1` (8/8). The elevated results from `62daad6` are in git history. See **Non-admin rerun** below.

## Observations

| Check | Result | Evidence |
|---|---|---|
| Echo round trip (`cmd.exe`) | Works, 27 ms typed-line → output | `echo_round_trip` |
| ANSI colour | Arrives, but **re-encoded by ConPTY**: `Write-Host -ForegroundColor Red` came out as `ESC[38;5;9m`, not `ESC[31m`/`ESC[91m`. ConPTY also emits its own OSC 0 title sequences and cursor moves. | `ansi_passthrough.raw_tail` |
| Resize | Works: after `resize(120×40)` the child reports `[Console]::WindowWidth` = 120 | `resize` |
| Ctrl+C (`\x03`) | Interrupts `ping -t`; prompt returns | `ctrl_c_interrupt` |
| Exit code | `cmd /C exit 7` → 7 via `try_wait` | `exit_code` |
| Child environment | PTY child gets the **registry** PATH (machine + user, 65 entries), not the parent's: the non-admin parent had 75 entries and the child still had 65. The elevated run only looked identical because that parent's PATH matched the registry. | `env_path` |
| **Host process killed** (≈ `amirid` crash) | PTY child (`ping.exe`) was alive, then **dead within 3 s** | `host_killed` |
| **Host closes pseudoconsole** (drop master) | Child **dead within 1 s** | `host_closes_pseudoconsole` |

## Consequences for the architecture

1. **§D3 row "Local Windows PTY — survives `amirid` crash: no" is now VERIFIED** (was `[UNVERIFIED]`). Local shells die with the process that owns the ConPTY. So the deferred `amiri-sessiond` (a minimal process that only owns ConPTYs) is the only way to let local shells survive a daemon crash or upgrade. Recommendation: keep it deferred for Phase 1 but design the `Transport`/`PtySession` boundary so `amiri-sessiond` can be slotted in without touching callers.
2. **Never assert on raw bytes.** ConPTY is a renderer, not a pipe: it re-encodes SGR colours, adds title and cursor sequences, and wraps at the PTY width. The PTY integration suite (§15) must assert on semantics (text appears, colour attribute is red) — or on files the child writes — not on byte-exact streams. This also reinforces D2: the orchestrator must never parse PTY output.
3. **Output is localized.** Tests and the environment probe must not match English labels from Windows tools (`mode con` printed `Столбцы:`). Use locale-neutral sources (`[Console]::WindowWidth`, exit codes, files).
4. **OEM code page.** `cmd` writes files in CP866 here; anything AMIRI reads back from a Windows shell needs explicit decoding, not `read_to_string`.
5. **ConPTY may send a DSR (`ESC[6n`) and wait.** The spike answers it in the reader thread; the product PTY layer must do the same (xterm.js does this in the UI, but the daemon needs its own answer when no UI is attached).
6. **This machine's PATH does not contain Windows PowerShell 5.1** (`powershell` did not resolve in the child or the parent). This is **unusual**: a default Windows 11 PATH contains `%SystemRoot%\System32\WindowsPowerShell\v1.0\`. Something (an installer, a PATH-cleanup tool, a manual edit) may have modified the system PATH; the user should check before relying on this machine as a reference environment. Either way, `amiri doctor` and shell profiles should resolve shells by absolute path (`%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`, `pwsh` via `where`) rather than trusting PATH.
7. **The spike ran elevated** (ConPTY titles read `Администратор:`), because the Claude Code session that ran it was elevated. Every agent command ran with admin rights. Follow-up decided in review: **`amirid` refuses to start elevated unless an explicit override flag is passed** (ARCHITECTURE §6.5), and `amiri doctor` reports elevation. Later spikes (S2+) must run from a non-admin session.

## Non-admin rerun

Rerun from a Medium-integrity session after UAC was re-enabled (S0 findings). Everything the elevated run observed still holds, except:

1. **Ctrl+C did not interrupt `ping -t` (every default run: 4 of 4, the last on `d764f5f`).** Cause confirmed by A/B on the same build: the spike process had inherited the console's *ignore Ctrl+C* flag from the session that launched it (a process started with `CREATE_NEW_PROCESS_GROUP` gets that flag, and children inherit it). The PTY children inherit it too, so ConPTY's `` → `CTRL_C_EVENT` is ignored. Calling `SetConsoleCtrlHandler(NULL, FALSE)` once at startup (`AMIRI_S1_RESTORE_CTRL_C=1`) made the check pass.
   → **`[DECISION proposed]`: `amirid` clears the inherited flag at startup** (`SetConsoleCtrlHandler(NULL, FALSE)`), because how the daemon is launched (Tauri, Task Scheduler, a terminal, a service wrapper) must not decide whether Ctrl+C works in user shells. `amiri doctor` can test it with this spike's `ping -t` check. Structured agents (pipes) are unaffected: AMIRI interrupts them through their protocol, not Ctrl+C.
2. **Resize check was brittle, not resize.** The child printed `120` both times; ConPTY ended the line with a cursor move (`ESC[8;1H`) instead of `
`, so waiting for `
` timed out. The check now reads the digits. Same lesson as consequence 2: don't wait for specific bytes from ConPTY.
3. **PATH comes from the registry** (table row above). Consequence: `amirid` must build each child's environment explicitly (registry PATH plus AMIRI's own additions) instead of assuming inheritance; which one portable-pty uses is an implementation detail that already differs from `std::process::Command`. The missing Windows PowerShell entry (consequence 6) is in the registry PATH itself, both machine and user.

## Locale handling (review check)

Assertions compare locale-neutral values only: `%OS%` (`Windows_NT` in every locale), `[Console]::WindowWidth` (a number), exit codes, and files the child writes. Two checks from the first run were rewritten this way; neither accepts Russian labels. The Ctrl+C check originally waited for `127.0.0.1:` in ping's localized reply, which happened to match the Russian format; it now uses a fixed wait instead. ru-RU (and uz-UZ) Windows become permanent test configurations (ARCHITECTURE §15).

## Not covered (follow-ups for S1b if needed)

- 1-hour soak with output flood (§15) — not run.
- `pwsh` 7 and WSL (`wsl.exe`) as PTY children.
- Whether a Job Object with `JOB_OBJECT_LIMIT_BREAKAWAY_OK` lets a child outlive the host (relevant only if `amiri-sessiond` is rejected).
