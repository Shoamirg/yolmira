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

## Still open

- Q10 `amiri-sessiond` — deferred (§17 Q10)

- Q5 default `plan_approval` (recommended: ASK)
- Q6 reviewer must be a different provider, or only a different session
- Q7 minimum trust bar for third-party crates (§5.3)
- Q8 AMIRI license (recommended: MIT/Apache-2.0 dual)
- Q9 code signing source
