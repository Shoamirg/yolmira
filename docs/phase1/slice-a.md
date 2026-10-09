# Phase 1, Slice A: daemon, named-pipe IPC, CLI, event log (findings)

- Date: 2026-09-25. Plan: `docs/phase1-plan.md` §1 Slice A, §2.
- Code: commits `52ae01b` (slice) and `9ccbd0d` (CI fixes). **Recorded run** on clean commit `9ccbd0d` (no tracked changes), non-admin (`S-1-16-8192`, Medium), culture ru-RU, console code page 65001 in the running shell: `scripts/test.ps1`, exit 0, **46 tests in 8 binaries, all run, all passed**.
- **CI** on `9ccbd0d` (GitHub-hosted `windows-latest`, en-US, elevated local Administrator): green, run `36138817727`. **42 run and passed, 4 not run** (need breakaway; see "CI").
- Reviews before the commit: `rust-reviewer` and `security-reviewer` agents (read-only). All HIGH and MEDIUM findings fixed in `52ae01b`; what was deferred is listed under "Open".

## Exit tests

| Plan test | Where | Result |
|---|---|---|
| `daemon_survives_launcher_job_close` | `crates/yolmira/tests/lifecycle.rs` | pass. Launcher in a kill-on-close job that allows breakaway; the job is closed and the daemon still answers 5 s later |
| `restrictive_job_reports_manual_start` | lifecycle | pass. Job forbids breakaway: launcher exits 1 with an actionable error (`why` + manual `yolmirad …` command), no daemon starts |
| `daemon_survives_launcher_console_close` | lifecycle | pass. Launched inside a ConPTY that is then closed |
| `second_start_connects_to_existing` | lifecycle | pass |
| `concurrent_start_single_daemon` | lifecycle | pass. 8 parallel starts give one pid and one `started`; a second `yolmirad` exits 4 (already running) |
| `ctrl_c_handling_s1d` | lifecycle | pass. Foreground daemon in a ConPTY survives Ctrl+C and keeps answering; console close gives exit 0 and `daemon.stopped {reason: console_close}` |
| `pipe_dacl_is_current_user_only` | `crates/yolmira/tests/ipc.rs` | pass. Read back from the live pipe: protected DACL, one allow ACE for the user's SID, label `(ML;;NWNR;;;ME)` |
| `ipc_rejects_missing_or_wrong_token` | ipc | pass. Wrong token and "no hello first" both get `unauthorized` and are recorded |
| `child_env_is_allowlisted` | `yolmira-ipc` `win::env` unit tests | pass at unit level: a canary variable doesn't reach the child environment; PATH comes from the registry; `SystemRoot`/`windir`/`ComSpec` come from Windows. **The daemon-level check (a real shell sees neither) moves to Slice B**, which is the first slice that starts shells |
| `elevation_check` | `yolmira` unit test | pass: the decision function. Real elevated runs happen only on CI, never locally |
| `events_persist_across_restart` | ipc | pass |
| `migrations_apply_and_backup`, `projection_rebuild_matches_live` | `yolmira-store` unit tests | pass. The backup is the pre-migration database (version 1, no `sessions` table) |
| Added after review | ipc: `overlong_hello_is_cut_off`, `failed_auth_events_are_rate_limited`, `subscribers_receive_new_events`; store: literal `_`/`%` in kind filters; `yolmira-ipc`: bounded framing, private-file ACL on replace, client-name rules; CLI: control characters never reach the terminal | pass |

## Findings

1. **cargo puts tests in a job that forbids breakaway.** Under `cargo test`, the test process is in a kill-on-close job without `BREAKAWAY_OK`; the same binary run directly is in no job. That's exactly the plan's "restrictive job" case, and the launcher correctly refused to escape it. Consequences:
   - The lifecycle tests run through `scripts/test.ps1`, which builds with cargo and runs the test binaries directly. Under plain `cargo test` they fail at once with a message pointing to the script. CI uses the script.
   - **Development:** `cargo run -- daemon start` and, in Slice C, `tauri dev` hit the same refusal. In development the daemon is started first from a normal terminal (the error says how). Product behaviour is unchanged (decisions.md, "Phase 1 plan": no WMI escape).
2. **ConPTY's cursor-position request (S1 finding 5) stalls anything run in a pseudoconsole** until someone answers it. The test harness answers it; Slice B's PTY layer must too.
3. **This machine's registry has no `ComSpec` in the machine environment** (like PowerShell missing from PATH, S0). The child environment falls back to `<Windows>\System32\cmd.exe`.
4. **Package layout:** `yolmirad` and `yolmira` ship from one package (`crates/yolmira`) because cargo exposes built binaries only to their own package's integration tests. Recorded in ARCHITECTURE §4.2.

## Review findings and what was done

| Source | Severity | Finding | Done |
|---|---|---|---|
| security | HIGH | Client sent the token to whoever owned the pipe name (machine-global, predictable) | Client checks the server process's user SID and integrity (`GetNamedPipeServerProcessId`) before sending anything; mismatch is a distinct `Impostor` error, never "not running" |
| security | HIGH | An `--allow-elevated` daemon was usable from Medium integrity | Pipe and token file get a High mandatory label when elevated |
| security | MEDIUM | Low-integrity processes could open the pipe read-only and flood the log | Medium label with no-read-up/no-write-up; ≤16 unauthenticated connections; `ipc.auth_failed` limited to one per reason per 10 s with a suppressed count |
| both | MEDIUM | Unbounded line length | 4 KiB hello, 1 MiB after, both ends; client names limited to 64 printable ASCII |
| rust | HIGH | Broadcast after releasing the store lock: subscribers could get seq 6 before 5 and drop 5 | Broadcast under the lock |
| rust | HIGH | `stop` then an immediate `start` could time out (old daemon still held the instance) | `stop` returns only after the process exits; a new daemon waits up to 5 s for the instance |
| rust | MEDIUM | Follow clients kept the connection alive after disconnect; failed instance creation could spin; probe turned any error into "not running"; blocking SQLite on runtime workers; swallowed registry and exit-code errors | Subscription aborted on disconnect (one per connection); next instance created before serving, with retry; probe reports everything except absent/closing; paged reads on the blocking pool; errors logged or returned |
| both | LOW | `CREATE_ALWAYS` kept an existing token file's ACL; DLL search order; `MutexGuard` was `Send`; LIKE wildcards; JSON-RPC parse errors used id 0; functions over 50 lines | Delete + `CREATE_NEW` (reparse points not followed); `SetDefaultDllDirectories`; guard is `!Send`; exact prefix match; `"id": null`; split |

## Open (deferred, with owner)

- **Profile folders in the child environment** (`USERPROFILE`, `APPDATA`, `LOCALAPPDATA`) still come from the launcher's environment; they should come from `SHGetKnownFolderPath`. Slice B, before shells start.
- **Impostor check has no automated test:** it needs a second Windows account to own the pipe name. The code path is small; covered by review only. Candidate for the Phase 10 Windows Sandbox tests.
- **Per-session mutex vs global pipe:** the same user in a second logon session gets a startup failure rather than "already running". Rare; revisit with the Phase 10 installer.
- **Logoff/shutdown through the hidden window:** manual check at the phase exit (planned).
- **Two unexplained slow runs.** During the pre-commit stress runs, one lifecycle run took 213 s and one IPC run 351 s; both passed. They didn't recur in 50+ later runs (including 20 back-to-back after adding diagnostics). The harness now bounds every CLI call at 60 s and fails with the command and the daemon's log tail, so a recurrence is caught with evidence instead of passing slowly.
- **`cargo deny` / `cargo audit`:** Phase 9 gate; to be run once manually at the phase exit.

## CI

- **The runner's own job object forbids breakaway** (first CI run, `36137606194`, failed on this): every step runs inside it, so no test process on CI can detach a daemon. Decision (2026-09-25, user): CI runs everything it can; nothing escapes any job.
  - Test daemons start through the detaching launcher when possible, otherwise directly inside the job.
  - The 4 detach tests (`daemon_survives_launcher_job_close`, `daemon_survives_launcher_console_close`, `second_start_connects_to_existing`, `concurrent_start_single_daemon`) run **locally only**. On CI the workflow sets `YOLMIRA_CI_NO_BREAKAWAY=1`; they then print `NOT RUN ON CI` (written past the test harness's output capture) and `scripts/test.ps1` lists them. libtest still counts them as passed, which is why the explicit list exists. Without the flag, a restrictive job fails them.
  - `restrictive_job_reports_manual_start` does run on CI (it tests the refusal).
- **SDDL renders well-known SIDs as aliases.** On the runner the user is the local Administrator, shown as `LA` instead of `S-1-5-…`, and Windows stores `GA` as `FA` on file objects. The owner-only SDDL now uses `FA`, and tests compare ACEs after Windows' own rendering (`SecurityDescriptor::to_sddl`, `sddl_aces`).
- The runner is elevated, so CI exercises `--allow-elevated` and the High-labelled pipe and token file; locally everything runs at Medium.
