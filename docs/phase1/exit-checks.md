# Phase 1 exit checks

Order (decisions.md 2026-10-09): these run before admin PowerShell step 1 and Phase 2. Slice D acceptance still waits for one green CI run on the latest commit. Results are recorded here; the phase findings link this file.

| Check | Who | Status |
|---|---|---|
| Close UI / reopen / shell survives, fresh build | automated (E2E) | see below |
| 1-hour soak (`soak_1h_output_flood`, `AMIRI_SOAK=3600`) | automated | see below |
| WSL terminate on a throwaway distro (`wsl_shutdown_while_attached`) | automated | see below |
| Logoff while the daemon runs detached | the user, steps below | pending |
| Pipe-owner rejection with a second Windows account (H1) | the user, steps below | pending |

## Automated checks

### Close UI / reopen / shell survives, fresh build

`92c87b9`, the AMIRI crates rebuilt from scratch (`cargo clean -p amiri`, then `npm run e2e`; the harness prints each binary's build time against its newest source: `amirid.exe` 13:53:54Z, sources 13:37:58Z; `amiri-app.exe` 13:54:04Z, sources 13:40:11Z). **9/9 pass**, 51.0 s, windows off-screen, no leftover processes: `ui_close_reopen_shell_survives`, `ui_killed_shell_survives`, `ui_started_from_closed_terminal`, `two_uis_same_session`, `daemon_down_banner_and_reconnect`, `output_flood_flows_through`, `ui_approval_prompt`, `project_picker`, `config_edit_needs_confirmation`.

Note: a first `cargo clean -p amiri` printed nothing (output discarded) and the run used the 18:38 binaries from `f2273c4` (no Rust change since; also 9/9). The clean was repeated visibly ("Removed 13572 files") and the suite rerun; the result above is that rerun.

## Logoff while the daemon runs detached (plan §4, manual check 2)

Expected: the daemon notices the session ending and stops cleanly; the event log shows `daemon.stopped` with reason `session_end`.

1. Save your work in other programs; this signs you out.
2. In a normal (not elevated) terminal, in `C:\Users\User\Desktop\AMIRI`:
   ```
   .\target\debug\amiri.exe daemon start
   .\target\debug\amiri.exe shell new --kind cmd
   .\target\debug\amiri.exe daemon status
   ```
   Note the daemon's pid from `daemon status`.
3. Close the terminal (the daemon keeps running, detached).
4. Sign out: Start → your account picture → **Sign out** (not Lock, not Switch user).
5. Sign back in. Open a terminal in the repo:
   ```
   .\target\debug\amiri.exe events --kind daemon. --limit 1000
   .\target\debug\amiri.exe daemon status
   ```
6. Expected: the last `daemon.stopped` line has reason `session_end` and comes after the `daemon.started` with the pid you noted. `daemon status` says the daemon is not running (nothing starts it at logon in Phase 1).
7. Paste me the output of step 5.

## Pipe-owner rejection with a second Windows account (H1, plan §4, manual check 1)

Expected: your `amiri` refuses a pipe that another account created under your daemon's name, sends it nothing (no token), and no second daemon starts.

The script is `scripts/manual/impostor-pipe.ps1` (no elevation). Self-tested under one account on 2026-10-09: a same-user client is accepted and the script logged "131 bytes received" (a fake token), so the byte count works; under a second account it must log 0.

1. **Create a second local account** if you have none (needs your admin password in the Windows dialog; I don't run anything elevated): Settings → Accounts → Other users → Add account → "I don't have this person's sign-in information" → "Add a user without a Microsoft account". A standard user is enough. Optional: in that account set Region → Regional format to Uzbek (Latin) to note the uz-UZ behaviour too.
2. **As yourself**, in the repo:
   ```
   [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
   .\target\debug\amiri.exe daemon status
   Copy-Item scripts\manual\impostor-pipe.ps1 C:\Users\Public\
   ```
   Write down the SID (starts with `S-1-5-21-`). If the daemon runs, stop it: `.\target\debug\amiri.exe daemon stop` (stopping asks for approval by default: approve in the AMIRI window, or with the code it shows).
3. **Switch user** (Start → your picture → the other account; do NOT sign out, your session must stay). In the second account, open Windows PowerShell and run, with your SID:
   ```
   powershell -NoProfile -ExecutionPolicy Bypass -File C:\Users\Public\impostor-pipe.ps1 -Sid S-1-5-21-...
   ```
   It prints "impostor pipe ... created by S-1-5-21-...(the second account's SID)" and waits 15 minutes.
4. **Switch back to yourself** (leave the script running) and run in the repo:
   ```
   .\target\debug\amiri.exe daemon status
   .\target\debug\amiri.exe daemon start
   Get-Process amirid -ErrorAction SilentlyContinue | Select-Object Id, Path
   Get-Content C:\Users\Public\amiri-impostor.log
   ```
5. Expected:
   - both `amiri` commands fail with "the daemon's pipe is owned by another process: ...";
   - no `amirid` from `C:\Users\User\Desktop\AMIRI\target\debug` is running (test daemons from a running soak have their own pipe names and don't count);
   - every "connection N" line in the log says **0 bytes received**.
6. Switch to the second account, press Ctrl+C in its PowerShell (or wait 15 min), sign that account out. Paste me the output of step 4.
7. Afterwards: `Remove-Item C:\Users\Public\impostor-pipe.ps1, C:\Users\Public\amiri-impostor.log`. Delete the second account if you don't want to keep it.
