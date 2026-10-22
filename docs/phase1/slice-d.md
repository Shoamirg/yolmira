# Phase 1, Slice D: policy engine, project registry, `yolmira doctor` (findings)

- Dates: 2026-10-05 to 2026-10-09. Plan: `docs/phase1-plan.md` §1 Slice D, plus the reviewer's additions of 2026-10-05 and 2026-10-09 (`docs/decisions.md`).
- Code: `8a3af4e` (policy core) … `cd74cb4` (project picker), all pushed. Main commits: `359592f` projects store, `e82a027` process peer helpers, `ffe7144` policy gate + approvals, `02a48d7` method coverage test, `2bdcfb6`/`65c3ff0` identity, `8681d14` approval panel, `771a0a2` doctor v1, `cd74cb4` project picker.
- **Recorded runs** on `cd74cb4`, non-admin (Medium), ru-RU:
  - `scripts/test.ps1`: exit 0, **all 14 test binaries pass**; 3 tests not run, each with its reason (unchanged since Slice B: 1 h soak, `pwsh` not installed (R7), WSL shutdown needs a throwaway distro). Durations: `lifecycle` 5.6 s, `pty` 4.0 s, `sessions` 3.7 s, `methods` 1.5 s, `doctor` 0.8 s, `ipc` 0.8 s, `policy` 0.8 s, the rest under 0.2 s.
  - `npm run e2e` (builds first; windows off-screen): **8/8 pass**, 42.2 s, WebView2 154.0.4258.62.
  - `npm test` (UI unit tests): 5/5. App crate: 13 unit tests; fmt, clippy (debug and release) clean.
- **CI: not yet green.** GitHub refused to start the job for `359592f` (run 37928451747: "recent account payments have failed or your spending limit needs to be increased"). Per the reviewer, **Slice D isn't accepted until CI has run green once**; the user is fixing billing.

## Exit tests

| Plan test | Where | Result |
|---|---|---|
| `policy_table` (unit) | `crates/yolmira-core/src/policy/tests.rs` | pass, 16 tests: built-in defaults, unknown → ASK, project over global, exact over wildcard, longest wildcard, a project can't loosen a global DENY but may tighten, `system.*` never ALLOW (an ALLOW becomes ASK; DENY stays), destructive git ASK under a wildcard ALLOW unless an exact per-action rule, invalid files rejected with a reason, whole-segment matching |
| `ask_round_trip` | `crates/yolmira/tests/policy.rs` | pass. A kill blocks in ASK on its own connection; the UI (a test client the daemon trusts in debug builds) allows it, it runs; a second one is denied, the caller gets `POLICY_DENIED` and the shell keeps running. Events in order: `policy.ask` → `policy.answered` → `policy.decision` → `session.kill_requested` |
| `every_mutating_method_emits_event` | `crates/yolmira/tests/methods.rs` | pass. Walks the protocol `METHODS` table: every mutating method is called and must record its declared event; `session.input` must carry its unlogged reason; every read method is served; a mutating method without a recipe fails the test |
| `doctor_detects_faults` | `crates/yolmira/tests/doctor.rs` + `crates/yolmira/src/cli/doctor/tests.rs` | pass. Real: a shell left RUNNING by a daemon killed with `taskkill /F` (`sessions_stale` warn), a corrupted database copy (`database` fail, exit 1), a fake agent CLI outside its tested range (`agent_fake` warn); doctor changes nothing (no daemon start). Injected: `EnableLUA=0` (`uac` fail), corrupt/unreadable DB, impostor pipe, no WebView2, running elevated. Every problem has a why and next steps |
| `project_shell_starts_in_repo` | `crates/yolmira/tests/policy.rs`; E2E `project_picker` | pass. Shell cwd is the registered folder `…\Проект oʻzbek`; an unknown project is an error with a next step. In the UI: register the folder, pick it, `cd` prints it |
| Added (reviewer, 2026-10-09) | `policy.rs`: `cli_approval_needs_the_code`, `agent_job_process_cannot_approve`, `only_the_ui_gets_codes` | pass. See "Approvals and agents" |
| Added | `ipc.rs`: `events_name_person_and_install`; identity unit tests | pass. Both ids stable across a daemon restart; a second instance that imports the person file has the same `person_id` and its own `install_id` |
| Added | E2E `ui_approval_prompt` | pass. Deny in the panel keeps the shell; the panel's code works for `yolmira approve`; Allow once ends it; answers logged with `via` ui/code/ui |

## What was built

- **Policy gate** (`crates/yolmira/src/daemon/policy.rs`). `session.create`, `session.kill`, `daemon.stop`, `project.register`, `project.remove` are checked. `policies.toml` (`%APPDATA%\Yolmira`, a named instance's own `config\`) is read on every check; a file that doesn't parse makes everything ASK. Defaults: kill and stop ASK (the reviewer's kill confirmation), the rest ALLOW. ASK holds the request until `policy.answer` (once, or until the daemon stops) or 600 s (= deny). Every verdict is a `policy.decision` event.
- **Projects**: `yolmira project add|list|remove`, `yolmira shell new --project`, `project.*` methods, a projection rebuilt from `project.registered/removed`; sessions record their project, and its rules apply to them.
- **Identity** (decisions.md 2026-10-09, team mode): `person_id` (per person, `identity.toml`, importable on another machine with `yolmira identity import`), `install_id` (per machine, automatic, local folder), `agent_id` (from Phase 2) on every event and approval. Migrations 4 and 5.
- **`yolmira doctor` v1**: daemon (+ impostor pipe), pipe ACL, database (read-only: integrity, schema, pending migration), stale sessions, elevation, UAC, agent CLIs (data: built-in `agents.toml` with `claude` tested 2.1.281 and `codex` informational, extended by the user's `agents.toml`), PowerShell, WebView2.
- **UI**: approval panel (Allow once / until the daemon stops / Deny, the CLI code shown), project picker; gated calls use their own connection so a pending approval never blocks typing.

## Approvals and agents (reviewer, 2026-10-09)

Agents will run as the user, with a shell, and can read the pipe token. So the token alone must not let a process approve:

- **(a) Not from an agent job.** The daemon identifies each connection's client process (`GetNamedPipeClientProcessId`; the process handle is held from connect, so a reused pid can't stand in). `policy.answer` and `policy.code` from any process inside an Yolmira agent job (kill-on-close, no breakaway, ARCHITECTURE §9.6) are refused and logged as `policy.answer_refused {reason: agent_job, pid, image}`. An unidentifiable process counts as inside.
- **(b) A one-time code shown only in the UI.** Each request gets 8 random digits. Only the genuine UI (`yolmira-app.exe` beside `yolmirad.exe`, outside agent jobs) can read it (`policy.code`) or answer without it; any other client needs it. It is never written to an event. Three wrong codes deny the request.
- **Test of the reviewer's case:** a process the daemon starts inside a real agent job runs `yolmira approve <id> --code <the right code>`: refused, logged with its pid, the request still waits (`agent_job_process_cannot_approve`).
- **Residual risks:**
  - **R14** (ARCHITECTURE §16.4): any same-user process can drive the UI (send input, read window text), so an agent could read the code or click Allow without escaping its job. Candidate fix: agents at low integrity (UIPI; the pipe rejects them); spike **S7** at the start of Phase 2, since Claude Code may not work at low integrity.
  - An agent that escapes its job (e.g. WMI process creation) passes (a); it still needs the code that only the UI shows (and R14 then applies).
  - Approving needs the Yolmira window: the CLI alone can't approve without its code. Intended, but `yolmira daemon stop` (ASK by default) now needs the window open.

## Config files agents could edit (reviewer, 2026-10-09)

Agents run as the user and can edit `policies.toml` and `agents.toml`, which would bypass every approval (e.g. `"daemon.stop" = "allow"`). Now (`d04c7e4`):

- **The confirmed text** of each file is kept in the event log (`policy.config_accepted`, full content). An instance's first start (and the guard's first start on an existing install) confirms the files as found: trust on first use, logged with that reason.
- **Every change is logged with a line diff** (`policy.config_changed {file, diff, loosenings, applied}`). The daemon looks at both files on every policy check and every 2 s.
- **Tightening applies at once** (logged, then confirmed with reason "an edit that only tightens").
- **Loosening waits for a person.** Compared by behaviour, not text: every action the rules can reach (known actions, every pattern either version names, a representative action under each wildcard), globally and in every project either names; any decision that gets less strict is a loosening. For `agents.toml`: a dropped agent, a wider or removed tested range, a changed command, a warning demoted. The edit opens a confirmation in the approval machinery: the UI shows the file, what it loosens and the diff, with the one-time code; agent-job processes can't answer; three wrong codes decline it. Until confirmed, each decision is the stricter of the confirmed and the edited rules, so the edit's tightening parts still apply. A declined text isn't asked again while the file keeps it.
- **Fails closed:** an edit that doesn't parse applies the confirmed policy with at least ASK everywhere; for `agents.toml`, the confirmed list.
- **`yolmira doctor`** uses the confirmed agent list and reports unconfirmed edits (`config_policies`, `config_agents`).
- **Tests:** `agent_edit_allowing_daemon_stop_still_asks` (a process in an agent job overwrites policies.toml to allow `daemon.stop`: the edit is logged with its diff and not applied, stopping still asks, the agent can't confirm its own edit even with the right code, a person's confirmation applies it); `tightening_edit_applies_at_once`; E2E `config_edit_needs_confirmation` (the window shows the diff; a kill still asks until the edit is confirmed); unit tests for the comparison, the diff, both files' rules.
- **Residual:** a same-user process could also edit the event log (the database) and so the confirmed text. Agents at low integrity (S7, R14) can write neither file nor database.

## Findings

1. **The daemon's allowlisted environment held.** A test variable naming the trusted test UI reached the daemon only when the test started it directly (cargo, in-job); a detached daemon (the launcher's path) got the allowlist (§9.6) and never saw it. Debug builds now read the test UI's path from a file in the instance's own config folder.
2. **ASK blocks only its own connection** (requests on one connection are sequential). Clients that must stay responsive send gated calls on a separate connection: the UI does, and the CLI watches for its own `policy.ask` on a second connection to prompt for the code. A client gives a gated call up to 630 s (`ASK_LIMIT`), not the usual 30 s.
3. **Claude Code updated itself to 2.1.294** (tested: 2.1.281). Doctor reports it as a warning with the R4 note: risk R4 observed live.
4. **WebView2 updated itself** between runs (154.0.4258.53 → .62); the E2E harness stopped with the planned message until the new driver was pinned (zip SHA-256, signature checked).
5. **Off-screen test windows** must be moved after creation (Windows ignores an off-screen creation position); on/off-screen results and timings matched (see `slice-c.md`), so off-screen is the default.
6. **Tooling:** npm's `ignore-scripts` silently skipped the E2E build (stale binaries; fixed and guarded, `slice-c.md`); and shell-quoted Markdown once ran its backtick spans as commands (no harm; docs are now written through files).

## Plan deviations

- **Approval scope "this session"** is "until the daemon stops" (Phase 1 has no tasks or agent sessions to scope to).
- **Debug-only test hooks** (release builds have neither): the trusted test UI file (`config\debug-trusted-ui.txt`) and `debug.spawn_in_agent_job` (not in the `METHODS` table, so the method-coverage test doesn't see it).
- **`yolmira approve`** needs the code shown in the UI (reviewer's requirement (b)); the plan's "interactive prompt in the CLI" asks for that code.
- **Elevation** is read from the token's elevation flag (`TokenElevation`): locale-neutral like a SID check, and what `yolmirad` itself uses.
- **Doctor's agent list** is data (`agents.toml`), as the reviewer asked; Gemini/Qwen stay out (their proposal is undecided).

## Open

- **CI green on the latest commit** (billing; not a rerun of the old run on `359592f`), then Slice D acceptance. After acceptance: the Phase 1 exit checks (decisions.md 2026-10-09) before anything else.
- A request whose client disconnects isn't cancelled: it waits until answered or the 600 s limit. Harmless (the decision is logged), but the panel shows it meanwhile.
- `yolmira doctor` doesn't yet report identity (person/install) or `policies.toml` validity; the daemon reports an invalid file by asking for everything.
- The UI doesn't show who (person/agent) asked; Phase 1 has one person and no agents.
- Low-integrity agents (S7, R14) at the start of Phase 2.
- Kill confirmation lives in the policy (`session.kill` ASK by default); a UI-only dialog wasn't added.
