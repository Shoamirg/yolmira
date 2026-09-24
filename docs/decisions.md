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

## Still open

- Q10 `amiri-sessiond` — deferred (§17 Q10)

- Q5 default `plan_approval` (recommended: ASK)
- Q6 reviewer must be a different provider, or only a different session
- Q7 minimum trust bar for third-party crates (§5.3)
- Q8 AMIRI license (recommended: MIT/Apache-2.0 dual)
- Q9 code signing source
