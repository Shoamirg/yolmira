# AMIRI — Review decisions

Answers to ARCHITECTURE.md §17, recorded as `USER_DECISION`s.

| Date | Question | Decision | Effect |
|---|---|---|---|
| 2026-09-24 | §17 Q1 — phase order (§16.1) | **Approved.** Phase 0 spikes first; minimal Policy Engine and event log from Phase 1. | Product code waits for spike findings. |
| 2026-09-24 | §17 Q2 — AgentsCommander (§2.1) | **Reference and lift**, not fork. Its ConPTY/xterm layer is lifted with MIT notice if spike S5 finds it clean and separable. | §4 stands as written. |
| 2026-09-24 | §17 Q3 — AI-TV execution target | **Both** local (RTX 5070 Ti) and a Linux GPU server. | Local ConPTY/WSL first; Phase 3 (SSH) is on the critical path right after Phase 2. |
| 2026-09-24 | §17 Q4 — config format | **TOML** for AMIRI config (`machines.toml`, `policies.toml`, `agents.toml`, `.amiri/project.toml`); YAML only for Obsidian/SKILL.md frontmatter. | §7.1 file names change from `.yaml` to `.toml`; the unmaintained `serde_yaml` is no longer needed for config. |

| 2026-09-24 | S1 review — elevation | `amirid` refuses to start elevated without `--allow-elevated` (§6.5). Spikes S2+ run from a non-admin session. | Elevation check in daemon startup and `amiri doctor`. |
| 2026-09-24 | S1 review — locale | ru-RU and uz-UZ are permanent test configurations; assertions only on locale-neutral values (§15). | Test matrix. |
| 2026-09-24 | S1 review — reproducibility | Spike results record the git commit and dirty flag they ran against. | `git` block in each spike's `findings.json`. |
| 2026-09-24 | S2/S3 limits | Approved with limits: cheap model, small `--max-turns`, throwaway scratch folder (not this repo), minimal `--allowedTools`, non-admin session, trivial prompts plus one file-write to exercise the permission round-trip. Raw streams saved as fixtures. | Spend expected in cents. |
| 2026-09-24 | Spike order | S5 (AgentsCommander) next; S4 alongside against a Docker OpenSSH container or an sshd in WSL if no GPU server is available yet. | — |

| 2026-09-24 | §17 Q2 revised after S5 | **Reference only** (supersedes "lift"). AgentsCommander's PTY layer is ~51k lines coupled to Tauri and its product model, and lacks DSR and locale handling. Its Job Object pattern, resize gate and argument adapters are reimplemented, not copied. **Condition:** any AMIRI code that closely follows AC's source gets an attribution entry (MIT notice) in `THIRD_PARTY_NOTICES.md`. Its build was not run and won't be: npm lifecycle hooks execute arbitrary code. | `spikes/s5-agentscommander/FINDINGS.md` |
| 2026-09-24 | Agent containment | **Approved:** `amirid` holds agent processes in a kill-on-close Job Object. Scope and startup reconciliation as in ARCHITECTURE §9.6. | S3 must confirm that it kills Codex's grandchildren. |
| 2026-09-24 | Spike prerequisites | Each spike's findings record the integrity level line from `whoami /groups \| findstr S-1-16-8192` (Medium = not elevated; match the SID, since the label is localized). The env probe runs in the same non-admin window. | — |
| 2026-09-24 | S4 SSH target | Disposable: a throwaway WSL distro with the distro's own `openssh-server` on localhost only (preferred), or a Docker image pulled **by digest** with the digest recorded. A fresh key pair generated only for S4; real keys never used. | — |
| 2026-09-24 | UAC | The dev machine had UAC off (`EnableLUA=0`); re-enabled. `amiri doctor` fails on `EnableLUA=0` (ARCHITECTURE §6.5). | `spikes/s0-environment/FINDINGS.md` |
| 2026-09-24 | Ctrl+C in shells (S1 non-admin) | **Approved:** `amirid` clears the inherited ignore-Ctrl+C flag at startup (`SetConsoleCtrlHandler(NULL, FALSE)`) so shells under it get Ctrl+C. **And** it installs its own console handler that ignores Ctrl+C/Ctrl+Break and shuts down cleanly on console close/logoff/shutdown. A spike check covers both: a child shell is interrupted by Ctrl+C, and `amirid` survives one. | S1d |
| 2026-09-24 | Child environment | **Allowlist:** each child's environment is built from fresh Windows values (registry machine + user, system variables) plus deliberate additions. Nothing from `amirid`'s own environment is passed through implicitly. | Fixes the stale PATH; keeps daemon secrets out of agents. |
| 2026-09-24 | S2 go | **Approved** within the S2/S3 limits, including the check that `claude` exits when its stdin closes. | — |
| 2026-09-24 | S4 image | **Approved:** official Ubuntu WSL image, SHA-256 **verified** against Ubuntu's published `SHA256SUMS` (and its GPG signature if published). Distro `amiri-s4-sshd`, key-only auth, `127.0.0.1` only, non-default port. Removed with `wsl --unregister` at the end, teardown recorded. | — |
| 2026-09-24 | Destructive git | No `git checkout -- <path>`, `git reset`, `git clean` or other change-discarding git command in this repo without asking the user first (the same rule AMIRI's Policy Engine will enforce). | — |
| 2026-09-24 | Codex | User installs and logs in to Codex themselves; S3 runs last and records whether Codex works natively on Windows or needs WSL. | — |
| 2026-09-25 | tmux status bar (S4) | **Approved:** AMIRI turns the status bar off in the tmux sessions it creates (`tmux set -t <s> status off`), so the pane size equals the PTY size AMIRI renders. Rendering tmux's status line instead is not pursued. | ARCHITECTURE §10.5; `spikes/s4-russh/FINDINGS.md` |
| 2026-09-25 | Provider accounts (hard rule) | Agents always run through their official CLIs with their own login. AMIRI **never** extracts or reuses CLI login tokens, **never** auto-rotates accounts to get around usage limits, and **never** uses another person's account. Account profiles chosen per project by the user are allowed, for the user's own accounts only. **Reasoning:** sharing accounts and getting around usage limits break the providers' terms and put every account involved at risk of suspension. AMIRI would become a tool people get banned for using. Running the official CLI with its own login is the intended use, and AMIRI's design already keeps provider tokens inside the CLIs (§6.4). The reviewer cited public reports of suspensions for these practices; not independently verified here, and the rule doesn't depend on them. | ARCHITECTURE §6.4 |
| 2026-09-25 | Usage limits and Claude→Codex failover | **Approved.** On a usage limit the task is `PAUSED: usage_limit` (with the reset time if reported). Per-project policy `provider_failover`: **ASK** (default; choose wait-and-auto-resume, API-key billing, or hand over) or **ALLOW** (hand over automatically; the user sets it for their own projects). Handover: git snapshot, recorded diff and in-progress plan step, handoff packet; the fallback agent first verifies the working state (diff + tests). If the fallback agent was the task's reviewer, lost review independence is recorded and a review by the original agent is queued for its reset, or the user is asked. Both limited: paused until the first reset, no ping-pong. The fallback agent keeps the task until it completes; normal roles return at the next task. One project rules source, sent in every packet (CLAUDE.md vs AGENTS.md). Depends on S3, which tests the Codex pickup from a packet. | ARCHITECTURE §8.3, §8.4, §16.2 (S3) |
| 2026-09-25 | Failover gate | Until a real limit-hit event has been captured and turned into a fixture, `provider_failover: ALLOW` behaves as ASK. The first real limit is captured opportunistically from the raw stream logs during normal use; never provoked (it would waste usage). The three earlier deviations (reports not recorded as fact, detection marked unverified, per-account mechanism checked in S3/Phase 2) are agreed. | ARCHITECTURE §8.4 |
| 2026-09-25 | §17 Q11 — Compute | **Accepted, Phase 5b** (right after the orchestrator), with spike S6 (SkyPilot / dstack / Ray on own machines) at its start, not in Phase 0. A minimal version (inventory, jobs in remote tmux, logs back) after Phase 3 only if the user asks for remote training sooner. | ARCHITECTURE §10.9, §16.2 |

## Still open

- Q10 `amiri-sessiond` — deferred (§17 Q10)

- Q5 default `plan_approval` (recommended: ASK)
- Q6 reviewer must be a different provider, or only a different session
- Q7 minimum trust bar for third-party crates (§5.3)
- Q8 AMIRI license (recommended: MIT/Apache-2.0 dual)
- Q9 code signing source
