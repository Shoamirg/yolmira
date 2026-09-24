# S0 — environment baseline (non-admin session)

- Date: 2026-09-24
- Source: `scripts/amiri-env-probe.ps1`, output `amiri-env.json` (kept out of git: it contains local paths). This file keeps only what the spikes and the design need.

## Integrity level and UAC

- Earlier sessions (S1, S1b) ran **elevated** because UAC was disabled on this machine (`EnableLUA=0`). With UAC off, an administrator account gets its full token in every process, so no non-elevated session existed.
- The user set `EnableLUA=1` and rebooted. Now:
  - `whoami /groups | findstr S-1-16` → one line with SID `S-1-16-8192` (Medium mandatory level). The label text is localized (ru-RU), so only the SID counts.
  - `EnableLUA` = `1`.
- Consequence: `amiri doctor` checks `EnableLUA` as well as the token (ARCHITECTURE §6.5).

## Probe summary

| Item | Result | Spike impact |
|---|---|---|
| OS | Windows 11 Pro, 10.0.26200, AMD64, stock edition | — |
| Windows PowerShell | 5.1; **PowerShell 7 not installed** | Scripts target 5.1 |
| `powershell` on PATH | **Missing in the Claude Code session** (found by full path `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`) | Spawners use the full path, never bare `powershell` |
| ConPTY, WebView2, MSVC build tools | present | — |
| rustc / cargo | 1.94.1 | — |
| `cargo tauri` | not installed | Needed in Phase 1, not for spikes |
| node / npm | 22.23.1 / 11.10.0 | — |
| `claude` | 2.1.281 | S2 can run |
| `codex` | **not installed**; `OPENAI_API_KEY` unset | S3 blocked until the user installs and logs in |
| `ANTHROPIC_API_KEY` | unset | Claude uses its own login (§6.4) |
| git / ssh | 2.53.0 / OpenSSH for Windows 9.5p2 | — |
| WSL | 2.7.14, kernel 6.18; distros: Ubuntu-22.04 (running, default), Ubuntu (stopped), docker-desktop | WSL/tmux check runs in Ubuntu-22.04; S4 uses a fresh distro (below) |
| Docker | 29.1.2, Desktop running | Fallback S4 target |
| GPU | RTX 5070 Ti, 16 GB | AI-TV local target (decisions Q3) |

## S4 target rule

The throwaway distro is built from a clean official image, **never** by `wsl --export`/`--import` of the user's Ubuntu-22.04 (that would copy the home folder, SSH keys and saved logins). The image source URL and SHA-256 are recorded in S4's findings.
