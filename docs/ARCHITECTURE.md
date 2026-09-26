# AMIRI — Architecture

Status: **DRAFT FOR REVIEW** — no implementation until approved.
Date: 2026-09-24

## 0. How to read this document

This document applies AMIRI's own evidence rules to itself. Statements are tagged:

| Tag | Meaning |
|---|---|
| `[DOC]` | Taken from official vendor documentation. Still re-check against the installed version before relying on it. |
| `[README]` | Claimed by a third-party project's README. Not independently tested. |
| `[UNVERIFIED]` | Belief or recollection; must be confirmed by a spike before code depends on it. |
| `[DECISION]` | Architectural choice made here, open to review. |
| `[OPEN]` | Question the reviewer must answer. |

Phase 0 spikes verified some statements; those cite their findings (`spikes/*/FINDINGS.md`). What the spikes did not settle is listed as a named risk in §16.4. Every performance or reliability property not backed by a finding is a target, not a fact.

---

## 1. Environment baseline

**Status: NOT YET PROBED.** Task 28 items 1–9 require inspecting the actual machine. That was not possible when this draft was written. Run `amiri-env-probe.ps1` (read-only, prints no secrets) and attach `amiri-env.json` to the review.

The probe checks: OS build and ConPTY support (Windows 10 1809+), Rust/Cargo/rustup, Node/npm/pnpm, Tauri CLI, WebView2 runtime, MSVC build tools, Claude Code and its MCP config (server names only), Codex CLI and its MCP config, API-key presence (boolean only), Git/gh, OpenSSH client and ssh-agent service, SSH key file names, known_hosts count, whether `~/.ssh/config` uses ProxyJump, WSL and distros, Docker, Obsidian install and registered vaults (and whether the Local REST API plugin is present), Python/uv, `nvidia-smi`, and the repository state.

User-reported facts (from earlier conversations, not probed): primary dev machine is Windows 11 with an RTX 5070 Ti (16 GB) and CUDA 12.8; AI-TV is the first target project.

Build prerequisites AMIRI itself will need `[DOC-level common knowledge, confirm with probe]`: Rust stable (MSVC toolchain), MSVC C++ build tools, WebView2 runtime (preinstalled on Windows 11), Node LTS for the UI build.

---

## 2. Existing projects evaluated

The field has moved fast; there are now many overlapping tools. The ones that matter for AMIRI's decisions:

| Project | What it is | Stack | License | Verdict |
|---|---|---|---|---|
| **mblua/AgentsCommander** | Runs Claude Code, Codex, OpenCode etc. as real PTY processes, coordinated through Markdown files; Windows is the primary platform `[README]` | Rust + Tauri 2, SolidJS, xterm.js, portable-pty, axum, tokio `[README]` | MIT | **Closest existing match. Study closely; selectively reuse (with attribution) its ConPTY/xterm plumbing. Do not fork wholesale** — see §2.1. |
| **tan-zhuo/AgentMux** | Desktop control plane for agents on remote hosts; agents live in tmux so they survive client disconnect; on Windows treats WSL and native Windows as two hosts, with a small detached session daemon for native sessions; state in SQLite `[README]` | Desktop app (verify) | verify | **Design reference for SSH, persistence, and the Windows session daemon.** Its model (app attaches to sessions it doesn't own) is adopted in §4. |
| **awslabs/cli-agent-orchestrator (CAO)** | Supervisor delegates to provider CLIs in isolated tmux sessions via a local server; HTTP API + PTY WebSocket `[README]` | Python | Apache-2.0 `[README]` | **Reference for provider-adapter design** (it supports ~12 CLIs). Not reusable directly: tmux-centric, Python runtime. |
| **markuswondrak/AgentMux** | Deterministic state machine (PM→Architect→Plan→Code→Review) driving CLIs in tmux; agents coordinate only through files and never talk directly `[README]` | Python + tmux | verify | **Conceptual match for AMIRI's orchestrator** (explicit state, artifacts, no free-running conversation). Ideas only. |
| **mikecubed/Hydra** (fork of PrimeLocus/Hydra) | Event-sourced HTTP daemon with task queue, routing, and multi-round deliberation across Claude/Codex/Gemini/Copilot CLIs `[README]` | Node 24 | verify | **Reference for routing and deliberation modes.** Not reusable: would add a Node runtime to the shipped app. |
| rencryptofish/hydra | Rust TUI managing agents in tmux sessions `[README]` | Rust | verify | Minor reference for session-status detection. |
| **itzikiusa/otto_os** | macOS Tauri app; pipeline ending in a "Proof Pack" and human approval before any PR `[README]`. Its README states the code is largely AI-generated and unaudited. | Tauri + Rust + Svelte | verify | **Ideas only** (Proof Pack ≈ AMIRI evidence bundle). Do not import code. |
| Draek2077/otto-code | Multi-provider agent UI with a provider-neutral event stream and orchestration graph `[README]` | verify | verify | Reference for the normalized event stream idea. |
| **adelost/agentmux** | Claude+Codex in tmux panes; durable per-pane FIFO prompt queue with delivery receipts; reads the agents' structured JSONL history instead of scraping the screen; `doctor` command enumerating silent failure modes `[README]` | Node | verify | **Adopt two ideas:** delivery receipts for prompts, and a `doctor` diagnostics command. |
| **jaysidd / RBJGlobal claudelink** | MCP server letting several Claude Code (and other MCP-capable) instances message each other through SQLite `[README]` | Node | verify | **Subsumed** by AMIRI's own MCP server (§9). Confirms the pattern works. |
| guzus/claude-link | Telegram bridge for Claude Code | — | — | Out of scope (unrelated despite the name). |
| Obsidian MCP servers (obsidian-mcp, MCPVault, trsdn/obsidian-mcp, Rust `obsidian-mcp` crate, cyanheads obsidian-mcp-server, Local REST API plugin) | Two camps: filesystem-direct (no plugin, works with Obsidian closed) vs. REST-plugin-mediated (Obsidian must run) `[README]`. The Local REST API plugin reportedly serves MCP itself since v5.0 `[UNVERIFIED, not load-bearing: AMIRI uses the filesystem]`. | mixed | mixed | **Filesystem camp chosen** (§11). External servers remain registrable as optional MCP tools. |
| **codex-codes** (Rust crate) | Typed Rust bindings for the Codex app-server JSON-RPC protocol: thread/turn lifecycle, approvals, streaming notifications `[README]` | Rust | verify | **Candidate dependency** for the Codex adapter, pinned and wrapped. Fallback: own thin client. |

**Security note.** At least one repository found during research (a lookalike "AgentMux" whose README directs Windows users to download a zip from inside a `tests/` folder) matches a known malware-lure pattern. AMIRI's dependency policy (§5.3) exists because of this ecosystem: many near-identical repos, low stars, vibe-coded code.

### 2.1 Why not fork AgentsCommander

It matches the stack (Rust, Tauri 2, ConPTY, Windows-first) and is MIT-licensed, so it is the strongest fork candidate. Reasons against forking wholesale `[DECISION, revisit at Phase 1 gate]`:

1. **Coordination model differs.** AgentsCommander coordinates agents through Markdown message files that the agents themselves read and write. AMIRI's spec requires an orchestrator-owned state machine, typed evidence, and policy-gated handoffs. Retrofitting that into a file-messaging core is a rewrite of its heart.
2. **UI framework differs** (SolidJS vs. required React).
3. **Size and bus factor.** ~4,000 commits, ~640 open issues, one primary maintainer `[observed on GitHub page]`. Forking means owning all of it.
4. **Enforcement stance.** Its README states room isolation is instructions to agents, not an enforced sandbox. AMIRI needs policy enforced at the points it controls.

**Phase 1 gate action:** build and run AgentsCommander locally for a day. If its PTY/terminal layer is clean and separable, lift it (with MIT notice). If the reviewer disagrees with points 1–4, fork instead and adapt §4.

---

## 3. Core architectural decisions

### D1. Daemon/UI split `[DECISION]`
AMIRI is two processes:

- **`amirid`** — Rust daemon. Owns PTYs, SSH connections, agent processes, SQLite, the event log, the policy engine, and the MCP server.
- **`AMIRI.exe`** — Tauri 2 app (React UI). A client of `amirid`; it launches the daemon if it isn't running.

Reason: closing or crashing the UI must not kill running agents (requirements 8, 13, 17). This follows AgentMux's "the app attaches to sessions it doesn't own" model. IPC is JSON-RPC over a Windows named pipe (`\\.\pipe\amiri-<user SID>`) with an ACL restricting it to the current user.

There is also a thin **`amiri` CLI** over the same IPC, used for scripting, tests, and headless diagnostics (`amiri doctor`).

### D2. Two channels per agent: structured control + human terminal `[DECISION]`
The orchestrator **never drives an agent by screen-scraping a TUI.** Both target agents expose structured protocols:

- **Claude Code:** `claude -p --input-format stream-json --output-format stream-json --verbose`, with `--session-id` / `--resume` for persistence and `--permission-prompt-tool` to route permission requests to AMIRI `[VERIFIED by S2: message schema, multi-turn, resume, interrupt, and permission round-trips over both stdio and an MCP tool; spikes/s2-claude-stream/FINDINGS.md]`.
- **Codex:** `codex app-server`, newline-delimited JSON-RPC over stdio: `thread/start`, `turn/start`, `turn/interrupt`, `turn/steer`, streamed item notifications, and server-initiated approval requests `[README-level via codex-codes and third-party guides; described as experimental. Deferred to S3 at the start of Phase 4: risk R1, §16.4]`.

The **PTY channel** is for humans: open the same session interactively (`claude --resume <id>` in a real ConPTY). A session is locked to one channel at a time so the orchestrator and a human never type into the same session at once.

Consequences: task completion, tool calls, costs, and approvals come from typed events, not regex over ANSI output. PTY mode stays fully real (requirement 6).

### D3. Honest persistence model `[DECISION]`

| Where | Survives UI close | Survives `amirid` crash | Survives network drop | Mechanism |
|---|---|---|---|---|
| Local Windows PTY | yes | **no** `[VERIFIED by spike S1: child dies ≤3 s after host kill, ≤1 s after pseudoconsole close — spikes/s1-conpty/FINDINGS.md]` | n/a | ConPTY owned by `amirid` |
| Local structured agent | yes | process: no, **enforced** by a kill-on-close Job Object (S1b showed a child that ignores stdin EOF survives as an orphan otherwise); **conversation: yes** | n/a | Agent's own session store + `--resume` / thread resume |
| WSL | yes | yes | n/a | tmux inside WSL |
| Remote Linux | yes | yes | yes | tmux on the remote + append-only journal (§10.5) |

When `amirid` restarts, it **reconciles**: every session previously RUNNING becomes UNKNOWN until re-observed. The UI never shows RUNNING based on stale state (requirement 8).

A later option is a separate tiny `amiri-sessiond` that owns only the ConPTYs so a daemon upgrade doesn't kill local shells, as AgentMux does. Deferred until the ConPTY spike shows it's needed.

### D4. SQLite event log as the source of truth `[DECISION]`
All state changes are appended as events; tables for tasks, sessions, etc. are projections rebuilt from events. This yields a debuggable history for free (requirement 18). It's event-sourcing-lite: no CQRS framework, just an append-only table plus projection updates in the same transaction.

### D5. Obsidian through the filesystem `[DECISION]`
AMIRI reads and writes the vault as Markdown directly (works with Obsidian closed, no plugin, no API key) and exposes it to agents through AMIRI's MCP server. This keeps a single enforcement point for write permissions. External Obsidian MCP servers can still be registered as optional tools.

### D6. AMIRI is an MCP server to its agents `[DECISION]`
Agents get AMIRI capabilities (task state, knowledge, skills, evidence recording, approval requests) as MCP tools. AMIRI generates a per-session MCP config granting only the tools the task needs (least privilege, requirement 22). AMIRI is also an MCP client to external servers listed in its tool registry.

### D7. Git through the CLI `[DECISION]`
Use `git` itself, parsing `--porcelain=v2` output. It respects the user's hooks, credential helpers, LFS, and config exactly. A Git library would add a large dependency and subtly different semantics. Revisit only if status polling proves too slow on large repos (measure first).

### D8. Policy is enforced at three layers `[DECISION]` — see §6
AMIRI cannot intercept what happens inside an agent's own tool loop. It enforces policy by (a) configuring each agent's native permission system, (b) answering the agent's permission and approval requests through the policy engine, and (c) gating everything AMIRI itself does. This limit is stated plainly so nobody assumes AMIRI is a sandbox.

---

## 4. Architecture

### 4.1 Process view

```text
┌──────────────────────── Windows host ────────────────────────┐
│                                                               │
│  AMIRI.exe (Tauri 2 + React + xterm.js)     amiri (CLI)       │
│        │  JSON-RPC over named pipe (user-ACL)   │             │
│        └──────────────────┬─────────────────────┘             │
│                           ▼                                   │
│  amirid (Rust daemon)                                         │
│   ├─ Core: Task Engine · Orchestrator · Event Bus ·           │
│   │        Policy Engine · State Manager                      │
│   ├─ Agents: ClaudeAdapter · CodexAdapter · (future)          │
│   ├─ Exec transports: Local(ConPTY) · WSL · SSH · (Docker)    │
│   ├─ Knowledge: Vault · Skills · Project/Session memory       │
│   ├─ Dev: Git(CLI) · Tests · Benchmarks · Artifacts           │
│   ├─ MCP: server (to agents) · client (external) · registry   │
│   └─ Store: SQLite (WAL) · artifact files                     │
│        │              │                  │                    │
│  claude -p (stdio)  codex app-server   ConPTY shells          │
│                                                               │
└───────────────┬───────────────────────────────────────────────┘
                │ SSH (russh): exec / PTY / SFTP channels
                ▼
     Remote Linux: tmux sessions · agent CLIs · journals
```

### 4.2 Cargo workspace

```text
amiri/
├── crates/
│   ├── amiri-core        domain types, task state machine, event bus, policy engine (no I/O)
│   ├── amiri-store       SQLite, migrations, projections, redaction
│   ├── amiri-exec        Transport + Pty traits; Local(ConPTY), WSL
│   ├── amiri-ssh         SSH transport, connection manager, known_hosts, SFTP
│   ├── amiri-agents      Agent trait; claude, codex adapters; normalized events
│   ├── amiri-knowledge   vault access, FTS index, skills, memory
│   ├── amiri-git         git CLI wrapper, porcelain v2 parser, snapshots
│   ├── amiri-mcp         MCP server + client + tool registry
│   ├── amiri-ipc         daemon protocol types shared with UI and CLI
│   ├── amirid            daemon binary
│   └── amiri-cli         CLI binary (incl. `amiri doctor`)
├── app/                  Tauri 2 shell + React/TS UI
├── fixtures/             recorded agent streams, test repos, vault samples
└── docs/                 this document, ADRs, dependency register
```

**As built (Phase 1, Slice A):** `amirid` and `amiri-cli` ship from one package, `crates/amiri`, with two binaries (`src/bin/amirid.rs`, `src/bin/amiri.rs`) and the code in `daemon/` and `cli/` modules. Cargo exposes built binaries only to the integration tests of their own package, and the lifecycle tests launch both. Crates are added in the phase that needs them.

Rule: `amiri-core` depends on nothing that does I/O. Everything provider-, transport-, or storage-specific sits behind a trait so it can be replaced without touching core (mission statement).

### 4.3 Key traits (sketch, not final)

```rust
/// Where commands run. SSH is one implementation; agents never see which.
#[async_trait]
pub trait Transport: Send + Sync {
    fn id(&self) -> &MachineId;
    async fn status(&self) -> ConnectionState;            // CONNECTED/DISCONNECTED/RECONNECTING/UNKNOWN
    async fn exec(&self, cmd: &CommandSpec) -> Result<ExecResult, AmiriError>;
    async fn spawn_pty(&self, spec: &PtySpec) -> Result<Box<dyn PtySession>, AmiriError>;
    async fn spawn_stdio(&self, spec: &CommandSpec) -> Result<Box<dyn StdioProcess>, AmiriError>;
    async fn put_file(&self, local: &Path, remote: &str) -> Result<(), AmiriError>;
    async fn get_file(&self, remote: &str, local: &Path) -> Result<(), AmiriError>;
    fn supports_detached(&self) -> bool;                  // tmux available?
}

pub trait PtySession: Send {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()>;
    fn output(&mut self) -> BoxStream<'static, PtyChunk>;  // raw bytes, ANSI intact
    fn signal(&mut self, sig: PtySignal) -> io::Result<()>; // Interrupt(Ctrl+C), Eof(Ctrl+D), Terminate
    fn exit_status(&self) -> ProcessState;                   // RUNNING/EXITED(code)/UNKNOWN
}
```

The agent trait is in §9.

---

## 5. Dependency decisions

### 5.1 Rust

| Crate | Purpose | Why | Rejected alternative |
|---|---|---|---|
| `tokio` | async runtime | De facto standard; required by russh, rmcp, Tauri | async-std (ecosystem mismatch) |
| `tauri` 2 | desktop shell | Spec requirement; small bundles, native WebView2 | Electron (size, second runtime) |
| `portable-pty` | ConPTY + Unix PTY | Maintained with WezTerm; used by AgentsCommander `[README]` | raw `windows` crate ConPTY (reinventing); winpty (legacy) |
| `russh` + `russh-sftp` | SSH client, PTY, exec, SFTP | Pure Rust, async, no OpenSSL/libssh2 C build on Windows `[VERIFIED by S4: Windows ssh-agent pipe, host-key pinning, PTY, SFTP, exec; agent forwarding not tested: risk R9, §16.4]` | `ssh2` (libssh2 C dep, blocking); shelling out to `ssh.exe` (kept as fallback transport for exotic `~/.ssh/config`, §10.6) |
| `rusqlite` (`bundled`) | SQLite | Bundled SQLite includes FTS5 (used for vault search); synchronous API is fine behind a dedicated DB thread | `sqlx` (compile-time checks need a live DB; heavier) |
| `rmcp` | MCP server + client | Official Rust MCP SDK `[UNVERIFIED: pin version and confirm stdio server support at first use: risk R10, §16.4]` | hand-rolled JSON-RPC (violates "don't invent APIs") |
| `codex-codes` | Codex app-server client | Typed protocol bindings `[README]`; wrapped so it can be replaced | own client (fallback if crate lags CLI versions) |
| `keyring` | Windows Credential Manager | OS secure storage for passwords/passphrases (requirement 7, 22) | DPAPI directly (more code, same result) |
| `serde`, `serde_json` | serialization | standard | — |
| YAML crate | config + frontmatter | `[OPEN]` `serde_yaml` is archived/unmaintained; choose a maintained fork after checking | TOML for config (spec examples are YAML; frontmatter must be YAML anyway) |
| `notify` | vault file watching | cross-platform, standard | polling |
| `tracing`, `tracing-subscriber`, `tracing-appender` | structured logs | standard; JSON logs for diagnostics | `log` (unstructured) |
| `uuid` (v7) | IDs | time-ordered, sortable IDs | ULID crate (equivalent; one fewer concept) |
| `thiserror` | error types | typed errors feed §14 diagnostics | `anyhow` in libraries (loses structure) |
| `regex`, `ignore` | search, gitignore-aware walking | from ripgrep's ecosystem | shelling out to `rg` (optional accelerator only) |

### 5.2 Frontend

React + TypeScript + Vite; `@xterm/xterm` + fit/web-links addons (the de facto browser terminal, also used by AgentsCommander and VS Code); `@tauri-apps/api`. State: a small store (Zustand) plus the daemon's event stream. No component library in Phase 1 ("functional before beautiful").

### 5.3 Dependency policy
- Every dependency gets a row in `docs/dependencies.md`: purpose, reason, alternatives, license, pinned version, date reviewed.
- `cargo deny` (licenses, advisories, duplicate versions) and `cargo audit` in CI; `npm audit` for the UI.
- No dependency from a repository with fewer than N maintainers/stars without an explicit reviewer sign-off `[OPEN: pick N]`. Vendoring is preferred over depending on low-trust crates.
- Agent CLIs (Claude Code, Codex) are **not** bundled; AMIRI detects them and reports versions. Adapters declare a tested version range and warn outside it.

---

## 6. Security model

### 6.1 Threats considered
1. **Prompt injection from project content** (README, issue text, web pages, vault notes) steering an agent into destructive or exfiltrating actions.
2. **A misbehaving agent** exceeding its task (deleting files, pushing, touching production).
3. **Secret leakage** into prompts, logs, event history, exported sessions, or vault notes.
4. **SSH risks:** unknown host MITM, key theft, uncontrolled remote commands.
5. **Local attackers:** another user or process on the machine talking to `amirid`'s pipe.
6. **Supply chain:** malicious or abandoned dependencies (§5.3).

### 6.2 What AMIRI can and cannot enforce

| Layer | What AMIRI controls | Mechanism |
|---|---|---|
| **A. Agent configuration** | What the agent is allowed to try | Per-session generated config: Claude `--allowedTools`/`--disallowedTools`/permission mode and scoped `--mcp-config`; Codex `approvalPolicy` + `sandbox` on `thread/start` |
| **B. Agent permission requests** | Actions the agent asks about | Claude `--permission-prompt-tool stdio` (`control_request {subtype: can_use_tool}` on the session's own pipe, S2); Codex `requestApproval` server requests → both go to the Policy Engine → ALLOW / ASK (UI prompt) / DENY |
| **C. AMIRI's own actions** | Everything AMIRI executes (git, SSH, file writes, vault writes, tests) | Policy Engine check before every action; recorded as events |

**Cannot enforce:** anything an agent does within permissions it already holds. An agent granted unrestricted shell can do whatever the OS user can. AMIRI is not a sandbox. For risky work, the project profile should prefer a WSL, container, or remote execution target. The UI states this when a task is granted broad shell access. `--dangerously-skip-permissions` and equivalents are **DENY by default** and can be enabled only per project with an explicit, logged user decision.

### 6.3 Policy engine
- Actions are typed (`git.push`, `fs.delete`, `ssh.exec`, `deploy.production`, `db.migrate`, `credential.*`, `agent.tool.<name>`, …). Unknown action types default to **ASK**, never ALLOW.
- Rules resolve most-specific-first: task → project → machine → global. A narrower scope may tighten but not loosen a global DENY.
- Every decision (including automatic ALLOWs) is an event: `policy.decision {action, rule, result, actor}`.
- ASK blocks the task in `AWAITING_APPROVAL` (§8). Approvals can be scoped: once, for this task, or for this session. Never "forever" from a prompt; permanent changes go through Settings.
- `git reset --hard`, `git clean`, `git push --force`, and deletion of untracked files are hard-classified as destructive; they are ASK even if a broad rule says ALLOW, unless the user sets an explicit per-action override.

### 6.4 Secrets
- AMIRI stores **references** to secrets, never values: key file paths, Credential Manager target names.
- SSH: prefer the Windows OpenSSH agent; fall back to reading the key file at connect time, with the passphrase from Credential Manager. Private key bytes never enter the DB, the event log, or any agent's environment.
- Provider auth belongs to the CLIs (Claude Code login, Codex login). AMIRI does not read or copy their tokens and only reports whether the probe found them.
- **Provider accounts: hard rule** `[DECISION]` (decisions.md, 2026-09-25). Agents always run through their official CLIs with their own login. AMIRI **never** extracts, copies or reuses CLI login tokens, **never** rotates accounts automatically to get around a usage limit, and **never** uses another person's account. A usage limit is handled by pausing, API-key billing or provider failover (§8.4), never by switching to a fresh account.
- **Account profiles per project** (allowed, the user's own accounts only). The user picks which of *their* accounts a project uses, e.g. a subscription login for everyday work and an API key for a client project. A profile is a reference: a separate CLI config directory holding that CLI's own login, or a Credential Manager target for an API key. It's added to the child's allowlisted environment (§9.6) as a deliberate addition. The profile is chosen per project by the user and never changed by AMIRI to escape a limit. `[UNVERIFIED: the per-profile mechanism for each CLI (config-dir variable for Claude Code, `CODEX_HOME` for Codex): risk R5 (Claude, Phase 2) and R1 (Codex, S3), §16.4]`
- A **redaction filter** runs on everything written to the event log, artifacts, and exports: known secret patterns plus exact-match against values fetched from Credential Manager in this process lifetime.
- Session export requires a redaction review screen.

### 6.5 Local IPC
Named pipe ACL: current user SID only. Each client authenticates with a per-boot random token stored in a user-only file. The Tauri webview gets only the IPC commands it needs (Tauri 2 capability files), with no shell or filesystem plugin access from the frontend.

**Elevation** `[DECISION, from S1 review]`: `amirid` **refuses to start in an elevated (Administrator) token** unless launched with an explicit override flag (`--allow-elevated`), which is logged as an event and shown as a persistent banner in the UI. Reason: every agent, shell and git command inherits the daemon's token, so an elevated daemon silently removes the UAC boundary from all agent actions. `amiri doctor` reports elevation either way.

**UAC disabled** `[DECISION, from the non-admin rerun]`: `amiri doctor` also reads `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System\EnableLUA`. `0` means UAC is off: an administrator account then gets its full token in every process, so there is no non-elevated session to run in and the elevation refusal above fires on every start. Doctor reports it as a failure with the fix (set `EnableLUA=1` and reboot) instead of just "elevated". The dev machine had UAC off, which is why S1 and S1b first ran elevated.

---

## 7. Data model

### 7.1 Storage locations

```text
%APPDATA%\AMIRI\             user-editable config (roams)
    machines.yaml            machine profiles (no secrets)
    policies.yaml            global policies
    agents.yaml              provider definitions and role defaults
%LOCALAPPDATA%\AMIRI\        machine-local state
    amiri.db                 SQLite (WAL mode)
    artifacts\<task_id>\     logs, diffs, benchmark raw output, stream captures
    logs\                    rolling JSON logs of AMIRI itself
<repo>\.amiri\project.yaml   portable, committable project profile
```

**Project profile split** `[DECISION]`: the spec's example profile mixes portable facts (rules, required Python/CUDA, preferred agents) with machine-local facts (`D:/Projects/AI-TV`). A committed file can't know its own absolute path on every machine. So `<repo>/.amiri/project.yaml` holds portable settings, and AMIRI's DB holds the registry `project name → repo path, vault paths, machine overrides`. "Continue AI-TV" resolves the name through the registry, then loads the in-repo profile.

### 7.2 SQLite schema (sketch)

```sql
-- append-only; source of truth
CREATE TABLE events (
  seq         INTEGER PRIMARY KEY,          -- global order
  id          TEXT NOT NULL UNIQUE,         -- uuid v7
  ts          TEXT NOT NULL,                -- RFC3339 UTC
  kind        TEXT NOT NULL,                -- e.g. agent.command.executed
  actor       TEXT NOT NULL,                -- user | amiri | agent:<id> | policy
  project_id  TEXT, task_id TEXT, session_id TEXT, machine_id TEXT,
  payload     TEXT NOT NULL,                -- JSON, post-redaction
  redacted    INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX events_task ON events(task_id, seq);

CREATE TABLE projects  (id TEXT PRIMARY KEY, name TEXT UNIQUE NOT NULL, repo_path TEXT NOT NULL,
                        vault_paths TEXT, profile_hash TEXT, created_at TEXT);
CREATE TABLE machines  (id TEXT PRIMARY KEY, name TEXT UNIQUE, kind TEXT CHECK(kind IN ('local','wsl','ssh','docker')),
                        config TEXT NOT NULL, last_state TEXT, last_seen_at TEXT);
CREATE TABLE sessions  (id TEXT PRIMARY KEY, task_id TEXT, agent_id TEXT, provider TEXT,
                        provider_session_ref TEXT,   -- Claude session id / Codex thread id
                        machine_id TEXT, cwd TEXT, channel TEXT CHECK(channel IN ('structured','pty')),
                        status TEXT, lock_holder TEXT, created_at TEXT, updated_at TEXT, archived INTEGER DEFAULT 0);
CREATE TABLE tasks     (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, objective TEXT NOT NULL,
                        constraints TEXT, state TEXT NOT NULL, execution_target TEXT,
                        base_commit TEXT, final_result TEXT, repair_attempts INTEGER DEFAULT 0,
                        created_at TEXT, updated_at TEXT);
CREATE TABLE task_roles(task_id TEXT, role TEXT, agent_id TEXT, PRIMARY KEY(task_id, role));
CREATE TABLE artifacts (id TEXT PRIMARY KEY, task_id TEXT, kind TEXT, path TEXT, sha256 TEXT, created_at TEXT);

-- knowledge claims and the evidence behind them (§13)
CREATE TABLE claims    (id TEXT PRIMARY KEY, task_id TEXT, project_id TEXT, text TEXT NOT NULL,
                        kind TEXT CHECK(kind IN ('FACT','OBSERVATION','AGENT_CLAIM','USER_DECISION','EXPERIMENT','BENCHMARK')),
                        status TEXT CHECK(status IN ('UNVERIFIED','VERIFIED','REFUTED','WAIVED')),
                        source_actor TEXT, created_at TEXT);
CREATE TABLE evidence  (id TEXT PRIMARY KEY, claim_id TEXT, exec_event_id TEXT NOT NULL, -- must point to an AMIRI-observed execution
                        summary TEXT, metrics TEXT, artifact_id TEXT, created_at TEXT);
CREATE TABLE decisions (id TEXT PRIMARY KEY, task_id TEXT, text TEXT, rationale TEXT, decided_by TEXT, created_at TEXT);
CREATE TABLE approvals (id TEXT PRIMARY KEY, task_id TEXT, action TEXT, detail TEXT,
                        result TEXT, scope TEXT, decided_by TEXT, requested_at TEXT, decided_at TEXT);
CREATE TABLE git_snapshots (id TEXT PRIMARY KEY, task_id TEXT, phase TEXT, commit_sha TEXT, branch TEXT,
                            dirty_summary TEXT, taken_at TEXT);
CREATE VIRTUAL TABLE vault_fts USING fts5(path, title, body, tags);  -- rebuildable cache, never authoritative
```

Migrations are numbered SQL files embedded in the binary. Each gets a test that applies it to a DB produced by the previous release's fixtures. The DB is backed up before any migration runs.

---

## 8. Task state machine

### 8.1 States

```text
CREATED → ANALYZING → PLANNING → PLAN_READY ──(approve)──→ EXECUTING → TESTING → REVIEWING → VERIFYING → DOCUMENTING → COMPLETED
                                     │  (reject → PLANNING)       │           │           │            │
                                     │                            ▼           ▼           ▼            ▼
                                     │                          FAILED ──→ DIAGNOSING → REPAIRING → TESTING
                                     │                            │ (repair_attempts > max)
                                     │                            ▼
                                     └────────────────────────→ BLOCKED (needs human)

Orthogonal flags on any active state: AWAITING_APPROVAL · PAUSED
Terminal: COMPLETED · CANCELLED · ABANDONED
```

`AWAITING_APPROVAL` and `PAUSED` are **flags**, not states. The task keeps its phase (e.g. `EXECUTING + AWAITING_APPROVAL`), so resuming puts it back exactly where it was. `PAUSED` carries a reason (`user`, `usage_limit`, …) and, when known, the time it may resume (§8.4).

### 8.2 Transition rules

| From → To | Triggered by | Guard |
|---|---|---|
| PLAN_READY → EXECUTING | user (default) or policy `plan_approval: ALLOW` | A plan artifact exists with acceptance criteria, each testable |
| EXECUTING → TESTING | implementer reports done (MCP `task.report_done`) | A git snapshot was recorded; diff is non-empty or explicitly "no-change" |
| TESTING → REVIEWING | orchestrator | Configured test commands were **run by AMIRI** and their results recorded |
| TESTING → FAILED | orchestrator | Any required test failed (from exit codes, not agent prose) |
| REVIEWING → VERIFYING | reviewer verdict `approve` | Reviewer is a different agent/role instance than the implementer (configurable) |
| REVIEWING → REPAIRING | reviewer verdict `changes_requested` | Findings recorded as structured items |
| VERIFYING → DOCUMENTING | orchestrator | Every acceptance criterion has a claim that is VERIFIED, or WAIVED by the user |
| DOCUMENTING → COMPLETED | orchestrator | Vault writes done or skipped per policy; final git snapshot recorded |
| FAILED → BLOCKED | orchestrator | `repair_attempts >= max_repair_attempts` (default 3) |
| any → CANCELLED | user only | Running processes are interrupted and the outcome is recorded |

Agents cannot set states directly. They call MCP tools (`task.submit_plan`, `task.report_done`, `task.submit_review`, `evidence.record_run`). The orchestrator decides the transition. This is what prevents "uncontrolled conversational loops": every loop is bounded by a counter and every transition has a guard that checks recorded facts.

### 8.3 Handoff packets
Agents never message each other directly. The orchestrator hands each role a **handoff packet** built from recorded state:

```yaml
task: { id, objective, constraints, acceptance_criteria }
role: reviewer
repo: { path, base_commit, head_commit, diff_artifact }
evidence: [ {claim, status, metrics, artifact} ]
prior_findings: [...]          # for repair rounds
skills: [ CUDA/Blackwell, Benchmarking ]   # references, fetched via MCP on demand
project_rules: "<the project's single rules source>"
instructions: "<role prompt from agents.yaml>"
takeover:                      # only when failing over mid-task (§8.4)
  from_agent, reason, snapshot_commit, diff_artifact, plan_step_in_progress
```

This keeps context explicit, auditable, and provider-independent. The same packet works for Claude or GPT in any role (requirement 11).

**One source of project rules.** Claude Code reads `CLAUDE.md`; Codex reads `AGENTS.md`. AMIRI keeps one rules source per project (named in the project profile) and sends it in every packet as `project_rules`, so both agents work under the same instructions whichever files the repo has. If a repo has both files and they differ, `amiri doctor` and the project view report the divergence; AMIRI does not rewrite them.

### 8.4 Usage limits and provider failover `[DECISION]` (decisions.md, 2026-09-25)

**Detection.** The Claude stream carries `rate_limit_event` with `status`, `resetsAt`, `rateLimitType` (`five_hour`, …) and per-window `utilization` (seen in S2, all with `status: "allowed"`). `[UNVERIFIED: the event and status value when a limit is actually hit, and how the turn ends: risk R3; Codex's equivalent: R1, §16.4]`. The adapter normalizes these into an `AgentEvent::UsageLimit { window, utilization, resets_at, hit }`, and the UI shows utilization.

**On a limit hit** the task gets `PAUSED` with reason `usage_limit` and `resets_at` if reported. What happens next is set by the per-project policy `provider_failover`:
- `ASK` (default for new projects): the user chooses between **wait** (auto-resume at `resets_at`, same agent, same session), **API-key billing** for this task (the user's own API-key profile, §6.4), or **hand over** to the other provider.
- `ALLOW`: hand over without asking. The user sets this per project. **Gate:** until a real limit-hit event has been captured and turned into a test fixture, `ALLOW` behaves as `ASK`. An automatic handover must not fire on a signal nobody has seen. The first real limit is captured opportunistically from the raw stream logs (§9.1) during normal use, never provoked.

**Handover (e.g. Claude → Codex).**
1. AMIRI takes a git snapshot (commit on a scratch ref, working tree untouched) and records the diff and the plan step that was in progress. A limit can stop an agent mid-edit, so the step is never assumed finished.
2. It builds a handoff packet with a `takeover` block (§8.3). The fallback agent's first instruction is to verify the working state: read the diff and run the configured tests before continuing, and report what it found.
3. **Review independence.** If the fallback agent was this task's reviewer, it is now reviewing its own work. AMIRI records that review independence is lost and either queues a review by the original agent for when its limit resets, or asks the user to review. The REVIEWING guard (§8.2) doesn't pass on a self-review.
4. **Stay until done.** The fallback agent keeps the task until it completes. Normal role assignments return at the next task. No switching back mid-task.
5. **Both limited:** the task stays `PAUSED` until whichever agent resets first, and continues with that one. No ping-pong between providers.

Every step is an event (`task.paused`, `task.failover`, `review.independence_lost`). Failover changes the provider, never the account (§6.4).

---

## 9. Agent protocol

### 9.1 Trait `[PROVISIONAL until S3]`

`[DECISION]` (decisions.md, 2026-09-25): this trait and the `AgentEvent`/`AgentCapabilities` types are provisional until S3 has tested Codex. Phase 2 builds them against Claude, the only provider tested so far, and must not put Claude-specific behaviour into shared types. Examples: `init` arriving only after the first message, stdin EOF not stopping a turn, `control_request` shapes, `total_cost_usd: 0` on interrupted turns. Those stay inside the Claude adapter. S3 findings may change the trait. Contract tests (§15) are written against the trait, so a change shows up as test changes, not silent drift.

```rust
#[async_trait]
pub trait Agent: Send + Sync {
    fn id(&self) -> &AgentId;
    fn provider(&self) -> &str;                          // "claude-code", "codex", ...
    fn capabilities(&self) -> &AgentCapabilities;
    async fn start(&self, spec: SessionSpec) -> Result<SessionHandle, AmiriError>;
    async fn resume(&self, provider_ref: &str, spec: SessionSpec) -> Result<SessionHandle, AmiriError>;
    async fn send(&self, h: &SessionHandle, input: AgentInput) -> Result<TurnId, AmiriError>;
    async fn interrupt(&self, h: &SessionHandle) -> Result<(), AmiriError>;
    async fn terminate(&self, h: &SessionHandle) -> Result<(), AmiriError>;
    async fn status(&self, h: &SessionHandle) -> AgentStatus;          // observed, timestamped
    fn events(&self, h: &SessionHandle) -> BoxStream<'static, AgentEvent>;
    async fn answer_permission(&self, h: &SessionHandle, req: PermissionId, d: PolicyDecision) -> Result<(), AmiriError>;
}

pub struct AgentCapabilities {
    pub structured_channel: bool, pub pty_channel: bool,
    pub resume: bool, pub interrupt: bool, pub steer_mid_turn: bool,
    pub permission_callbacks: bool, pub mcp_client: bool,
    pub reports_cost: bool, pub native_skills: bool,
}

pub enum AgentEvent {                       // normalized across providers
    TurnStarted { turn: TurnId },
    MessageDelta { text: String },
    Message { text: String },
    ToolCall { name: String, input: serde_json::Value },
    ToolResult { name: String, ok: bool, summary: String },
    CommandExecuted { cmd: String, exit_code: Option<i32>, cwd: String },
    FileChanged { path: String, change: ChangeKind },
    PermissionRequested { id: PermissionId, action: ActionType, detail: String },
    Usage { input_tokens: u64, output_tokens: u64, cost_usd: Option<f64> },
    UsageLimit { window: String, utilization: Option<f64>, resets_at: Option<Timestamp>, hit: bool },  // §8.4
    TurnCompleted { turn: TurnId, outcome: TurnOutcome },
    Error(AmiriError),
    Raw { provider: String, json: serde_json::Value },   // preserved for debugging
}
```

Every raw provider line is also captured to `artifacts/<task>/<session>.jsonl` before parsing. When a CLI changes its format, the raw capture shows what happened and becomes a new test fixture.

### 9.2 Claude Code adapter
- Spawn: `claude -p --input-format stream-json --output-format stream-json --verbose --session-id <uuid> --mcp-config <generated.json> --permission-prompt-tool stdio [--allowedTools …]` (no `--safe-mode`, see below) in the target cwd, through the task's transport `[VERIFIED by S2, Claude Code 2.1.281]`. S2 established:
  - **Stdin schema, multi-turn and resume** work as planned. `system/init` arrives only after the first user message, so the adapter doesn't wait for it.
  - **Interrupt:** `control_request {subtype: interrupt}`. The turn ends as `error_during_execution` / `aborted_streaming`, and the session continues. Such turns report `total_cost_usd: 0`, so cost comes from per-message `usage` or is marked unknown.
  - **Stdin EOF is not a stop signal.** Mid-turn, `claude` finishes the turn (tools included) before exiting. Stop with interrupt or the job (§9.6); closing stdin only ends an idle session cleanly.
  - **Permission requests** round-trip both over stdio (`--permission-prompt-tool stdio`, `control_request {subtype: can_use_tool}`) and through an MCP tool. `[DECISION]` (decisions.md, 2026-09-25): **AMIRI uses stdio.** Permission decisions travel on the session's own pipe, don't depend on AMIRI's MCP server being up, and S2 showed allow and deny both work (a deny's message reaches the model; the `result` lists it in `permission_denials`).
  - **No `--safe-mode` for orchestrated sessions** `[DECISION]` (decisions.md, 2026-09-25). It disables `--mcp-config`, which removes AMIRI's own MCP tools (`task.report_done` and the rest, D6) that the orchestrator depends on. Consequence: the user's Claude Code hooks, plugins and settings can run inside orchestrated sessions (risk R13, §16.4).
  - `init.capabilities` lists protocol features. The adapter gates on these rather than on `claude_code_version`.
- Resume: `--resume <session-id>`. The PTY channel opens `claude --resume <id>` in ConPTY after the structured process has exited (session lock).
- Some sources report that Claude Code hooks do not run in `-p` mode and that `--append-system-prompt` is not preserved on resume `[README-level, UNVERIFIED: S2 ran with --safe-mode, so hooks were not tested; risk R13, §16.4]`. The adapter must not depend on hooks for enforcement; role instructions are re-sent in each handoff packet.
- AMIRI does not reimplement Claude Code features (subagents, skills, compaction). It configures and observes them.

### 9.3 Codex adapter
- Spawn `codex app-server` (stdio). Handshake with `initialize`/`initialized`. Then `thread/start` with `cwd`, `approvalPolicy`, `sandbox`, and `model`; `turn/start` to send input; `turn/interrupt` to stop; `turn/steer` to add input mid-turn `[README-level; deferred to S3: risk R1, §16.4]`.
- Server-initiated approval requests (`item/commandExecution/requestApproval`, file-change approvals) route to the Policy Engine. The adapter must handle inbound requests, not just responses, and must drain stderr continuously `[README: codex-wrapper issue #140]`.
- Reported behavior to verify: signaling the server may leave its child commands running, while closing stdin shuts it down cleanly `[README, UNVERIFIED: S3, risk R1, §16.4]`. `terminate()` therefore closes stdin first, then kills the process tree by job object on Windows.
- Uses the user's existing Codex login. An OpenAI-API-direct adapter (Responses API) is a separate future provider, not a Codex mode.

### 9.4 Adding a provider
Implement `Agent`, declare capabilities, add recorded fixtures, and add a contract test (§15). The orchestrator reads capabilities and never branches on provider names. A provider without `permission_callbacks` can only be assigned to tasks whose policy needs no ASK decisions, or it runs in a restricted execution target.

### 9.5 Roles
Roles (`planner`, `implementer`, `reviewer`, `debugger`, `researcher`, `tester`, `benchmarker`, `documenter`) are data in `agents.yaml` and the project profile: role → agent + role prompt + tool grants. Defaults follow the spec's example (Claude implements, GPT plans/reviews), but nothing in code assumes it.

### 9.6 Agent process containment `[DECISION, from S1b + review]`
- **Kill-on-close Job Object.** Every agent process `amirid` spawns (`claude -p`, `codex app-server`, fake CLIs in tests) is created suspended, assigned to a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` held through a non-inheritable handle, then resumed. When `amirid` exits or crashes, the kernel kills the whole tree, grandchildren included. This covers the reported Codex behaviour of child commands outliving signals, and the S1b orphan that survived ≥10.8 s after its host died. S3 must confirm it against the real CLI. `terminate()` (§9.3) uses the same job.
- **Scope: agent processes only.** Outside the job: ConPTY shells if `amiri-sessiond` is ever built (§17 Q10), and the UI. WSL needs no exception (verified by S1c): the job kills a foreground Linux process together with its `wsl.exe`, while a tmux server inside WSL survives even when the `wsl.exe` that started it was in the job. So work inside WSL tmux is **not** contained; startup reconciliation also lists AMIRI-named tmux sessions per distro and reports them (never kills silently).
- **Child environment is an allowlist** `[DECISION, from S1 non-admin rerun]`. Every child (agent, shell, git) gets an environment built from fresh Windows values (registry machine + user variables, system variables such as `SystemRoot`) plus deliberate additions from AMIRI's config. Nothing from `amirid`'s own environment passes through implicitly, so a secret in the daemon's environment never reaches an agent, and a stale PATH from whoever launched `amirid` doesn't either. (portable-pty already reads the registry PATH; `std::process::Command` inherits. The adapter uses `env_clear()` and sets the list itself, so both paths behave the same.)
- **Console control** `[DECISION, from S1 non-admin rerun]`. At startup `amirid` clears any inherited ignore-Ctrl+C flag (`SetConsoleCtrlHandler(NULL, FALSE)`), so shells under it get Ctrl+C whatever launched the daemon. It also installs its own handler: Ctrl+C and Ctrl+Break are ignored, console close, logoff and shutdown trigger a clean shutdown. The two are separate mechanisms (the flag is inherited by children, the handler is not). Spike S1d checks both.
- **Startup reconciliation for pre-job crashes.** `amirid` records each spawned PID with its process start time (Windows reuses PIDs). On startup it compares the records with running processes. Matches (same PID *and* start time) are marked `ORPHANED` and offered for cleanup in the UI and `amiri doctor`. They are **never killed silently**.
- Pattern reference: AgentsCommander `pty/job.rs` (reimplemented, not copied; see `docs/decisions.md`).

---

## 10. SSH architecture

### 10.1 Machine profile
The spec's YAML is kept, extended with `known_host_policy` (`strict` default), `allow_password: false` (default), `persistence: tmux|none`, and `default_workdir`. It never contains secret values.

### 10.2 Authentication order
1. Windows OpenSSH agent (named pipe `\\.\pipe\openssh-ssh-agent`) `[VERIFIED by S4: spikes/s4-russh/FINDINGS.md]`
2. `identity_file` read at connect time; passphrase from Credential Manager if stored, otherwise prompted and optionally stored
3. Password — only if `allow_password: true`; value from Credential Manager or prompt

### 10.3 Host keys
- Check AMIRI's `known_hosts` plus the user's `~/.ssh/known_hosts` (read-only).
- **Unknown host:** block and show the fingerprint (SHA256), key type, and host. The user must confirm explicitly. Recorded as an event.
- **Mismatch:** hard failure, never a "continue anyway" button. Resolved only through a separate re-key flow in Machine settings that shows both fingerprints.

### 10.4 Connection manager
One SSH connection per machine, multiplexed into channels (exec, PTY, SFTP). Keepalives, with exponential-backoff reconnect. Connection state (`CONNECTED/DISCONNECTED/RECONNECTING/UNKNOWN`) is tracked **separately** from process state (`PROCESS RUNNING/PROCESS EXITED/UNKNOWN`). The UI shows both, plus "last observed at", so a disconnected machine shows "last seen RUNNING 14:02, current UNKNOWN", never a bare RUNNING.

### 10.5 Persistent remote execution
- **Remote PTY sessions:** created inside `tmux new-session -d -s amiri-<session>` with the status bar off (`tmux set -t amiri-<session> status off`) so the pane size equals the PTY size (S4); AMIRI attaches through an SSH PTY channel. On reconnect it runs `tmux has-session` and reattaches. Scrollback survives.
- **Remote structured agents** (stdio JSON over a flaky link): launched inside tmux under a small wrapper script. The script reads stdin from a FIFO and appends stdout to `~/.amiri/sessions/<id>/out.jsonl`, writing the exit code to `exit` on completion. AMIRI writes to the FIFO and tails the journal by byte offset. On reconnect it resumes from its last stored offset, so no events are lost or duplicated. No binary is uploaded in v1. Future option: a static `amiri-relay` binary pushed over SFTP, as AgentMux does for its Windows daemon.
- If tmux is missing, the machine is marked `supports_detached: false`. Long tasks there require explicit acceptance that a disconnect kills them.

### 10.6 Compatibility
russh won't interpret all of `~/.ssh/config`. v1 honors `HostName`, `User`, `Port`, and `IdentityFile`. It **detects** `ProxyJump`/`ProxyCommand` and reports them as unsupported by the native transport. `[DECISION]` An alternative `ssh.exe` transport (OpenSSH client via ConPTY/stdio) covers those hosts, with the same Transport trait and fewer features (no native SFTP; uses `scp`).

### 10.7 Environment inspection
Read-only probe commands (`uname -a`, `cat /etc/os-release`, `nvidia-smi --query-gpu=...`, `python3 --version`, `tmux -V`, `which claude codex git`, `df -h`, `ps`) are policy class `inspect`, ALLOW by default, and cached as an environment fingerprint attached to evidence (§13).

### 10.8 WSL transport
`wsl.exe -d <distro> --cd <dir> -- <cmd>` through ConPTY/stdio. tmux inside WSL gives the same persistence model as remote Linux. Paths are translated both ways (`D:\x` ↔ `/mnt/d/x`) in one tested function.

### 10.9 Compute: training jobs on other machines' GPUs `[DECISION: accepted, Phase 5b]`
A subsystem separate from agents. A training run is not an agent session: it's a command with resources, run to completion, and its output is evidence.
- **GPU inventory per machine:** model, VRAM, driver/CUDA version, current use. Filled by the existing read-only probe (§10.7, `nvidia-smi --query-gpu=...`) and refreshed on the same cadence.
- **Job queue:** a job is command + target machine + GPUs needed + data location + checkpoint directory. Queued per machine; a job starts only when its GPUs are free by the inventory and the machine's cap allows it.
- **Execution:** each job runs in its own remote tmux session (§10.5; S4 showed tmux sessions survive a dropped connection), with logs streamed back by byte offset like the agent journal. After a disconnect AMIRI reattaches; after a reboot or crash it restarts the command, and the job resumes from its latest checkpoint (the job's command must support resuming, declared in the job spec).
- **Results as evidence (§13):** metrics and loss curves parsed from the log artifact, the final checkpoint's SHA-256, commit, dirty flag, machine and environment fingerprint, recorded like a benchmark run (`EXPERIMENT` evidence).
- **Policy:** `compute.start_job` is ASK by default on any machine, and always ASK on a machine the user marks as someone else's. Each machine profile can cap AMIRI's use (GPUs by index, and a limit on concurrent jobs).
- **Scope:** one job per machine first (parallel experiments, sweeps, different models). Distributed training of a single model across home machines is a later option: over ordinary Ethernet the GPUs mostly wait on the network.
- **Reuse before building:** spike S6 (§16.2) evaluates SkyPilot, dstack and Ray for scheduling on the user's own machines over SSH. If one fits, AMIRI wraps it and keeps only inventory, policy and evidence; the scheduler above is the fallback design.
- Phase placement: **Phase 5b**, right after the orchestrator (§16.2). It needs only the SSH transport (Phase 3), the event log and evidence recording (Phase 5); not Obsidian, skills or UI polish. If remote training is needed sooner, a minimal version (inventory, jobs in remote tmux, logs back) can follow Phase 3 on the user's request, with evidence recording added later.

---

## 11. Obsidian architecture

- **Configuration:** vaults are discovered from `%APPDATA%\obsidian\obsidian.json` (read-only) and confirmed by the user. The project profile names vault folders (`Projects/AI-TV`, `Skills/AI-TV`).
- **Write zones** `[DECISION]`: AMIRI may write only inside folders the user marks writable (default: `Projects/<project>/AMIRI/` and the `Skills/` tree via the proposal flow). Everything else is read-only to AMIRI and its agents.
- **Safe writes:** before overwriting, compare the stored content hash with the file on disk. If a human edited it meanwhile, write a sibling `… (AMIRI proposal).md` or append a clearly marked section instead of clobbering. All writes are atomic (temp file + rename) and logged as events.
- **Note conventions:** frontmatter written by AMIRI:
  ```yaml
  amiri_type: session-summary | decision | benchmark | skill
  project: AI-TV
  task: <id>
  status: verified | unverified
  evidence: [<evidence ids>]
  generated: 2026-09-24T10:00:00Z
  ```
  Wikilinks connect decisions → sessions → benchmarks, so the Obsidian graph shows project history.
- **Search:** SQLite FTS5 index built from the vault and kept fresh with `notify`. It's a cache: deletable and rebuildable at any time.
- **Agent access:** MCP tools `knowledge.search`, `knowledge.read`, `knowledge.propose_note`. Agents never get raw vault write access. Writes go through AMIRI's policy and zone checks.

---

## 12. Skills architecture

- **Format** `[DECISION]`: don't invent one. A skill is a folder with a `SKILL.md` whose frontmatter carries `name` and `description`, matching the Agent Skills convention that Claude Code uses natively. Codex also has a skills mechanism `[README-level; verify format compatibility in Phase 7: risk R11, §16.4]`. The spec's section layout (Environment, Known Issues, Verified Solutions, Benchmarks, Commands, Warnings, References) goes in the body.
- **Location:** the vault's `Skills/` tree, so skills stay ordinary Markdown in Obsidian.
- **Claim-level status:** each entry under *Verified Solutions* and *Benchmarks* carries an inline marker linking evidence, e.g. `✅ verified · ev:01J…` or `⚠ unverified`. A linter rejects "verified" markers that don't resolve to an evidence record.
- **Lifecycle:** agent calls `skills.propose(skill, patch, claims[])` → a pending proposal appears in the UI as a diff → the user accepts, edits, or rejects → the accepted patch is written to the vault. Claims arrive as UNVERIFIED unless their evidence IDs exist. **No path turns agent text into VERIFIED knowledge automatically** (requirement 15).
- **Discovery and loading:** skills named in the project profile are always offered. Others are ranked by FTS over name/description against the task objective. Agents fetch them via MCP `skills.get`. Optional sync into an agent's native skill directory (e.g. `~/.claude/skills`) is a policy-gated action, off by default, so AMIRI never silently modifies agent config directories.

---

## 13. Evidence and verification

The core rule: **only AMIRI-observed executions can verify a claim.**

1. An agent's statement ("this should cut latency") is recorded as `AGENT_CLAIM`, `UNVERIFIED`.
2. A benchmark or test is run **by AMIRI** through a transport (or by an agent whose command execution AMIRI observed as a structured `CommandExecuted` event with an exit code). The run record captures: command, machine, cwd, commit SHA, dirty flag, environment fingerprint, start/end time, exit code, and the raw output artifact with its SHA-256.
3. A parser (per benchmark type, declared in the project profile) extracts metrics, e.g. `p50_ms`, `p95_ms`, `n`. An agent may *propose* the parser mapping; the numbers come from the artifact.
4. Only then can the claim move to `VERIFIED`, linked to the evidence row. The user can mark `WAIVED` (recorded as USER DECISION) or `REFUTED`.

Benchmark rules for projects like AI-TV (`benchmark_before_acceptance`, `no_unmeasured_claims`) become **VERIFYING guards**. A task cannot complete while an acceptance criterion rests on an unverified claim. Results from a dirty working tree are flagged as not reproducible.

---

## 14. Observability

- **AMIRI's own logs:** `tracing` JSON logs, rotated, with a span per task/session/transport operation.
- **Event log** (§7) as the audit and debugging record, with a UI filter by task/session/machine/kind.
- **Error shape** used everywhere, from Rust to UI:
  ```rust
  pub struct AmiriError {
      pub what: String,          // "SSH connection to ai-server failed"
      pub where_: Location,      // subsystem + machine + session
      pub why: String,           // "host key mismatch for 192.168.1.50 (ED25519)"
      pub attempted: Vec<String>,// ["agent auth", "key ~/.ssh/id_ed25519"]
      pub next_steps: Vec<String>,// ["Compare fingerprints in Machines → ai-server → Re-key"]
      pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
  }
  ```
  A CI lint rejects user-facing errors with empty `why` or `next_steps`.
- **`amiri doctor`** (from adelost/agentmux's idea): one table of every silent failure mode. Daemon reachable, DB integrity, migration level, each CLI's version vs. adapter range, each machine's connection and tmux, MCP servers up, vault paths present, FTS index freshness, stale RUNNING states.
- **Metrics:** in-process counters (active sessions, reconnects, event write latency, turn durations, token/cost where reported), shown on a Diagnostics page. No telemetry leaves the machine.

---

## 15. Testing strategy

| Subsystem | Tests | Notes |
|---|---|---|
| Core | Unit tests. **Property tests** on the state machine: no sequence of agent tool calls can reach COMPLETED without passing all guards. | `proptest` (dev-only dependency) |
| Store | Migration tests from every previous schema fixture; projection rebuild equals live projection; redaction tests with seeded fake secrets | |
| PTY | Integration on Windows: spawn `cmd.exe`/`pwsh`, echo round-trip, ANSI color passthrough, resize, Ctrl+C interrupts a running `ping -t`, exit codes, 1-hour soak with output flood | Windows CI runner |
| SSH | Integration against an OpenSSH server container (Docker/WSL): key auth, password auth only when enabled, unknown-host block, mismatch hard-fail, PTY, exec, SFTP, forced disconnect → reconnect → tmux reattach with no lost journal bytes | Disconnects simulated by killing the connection mid-stream |
| Agents | (a) **Fixture tests:** recorded raw JSONL from real CLI runs replayed through adapters → expected normalized events. (b) **Fake CLIs:** small binaries emitting scripted streams, including malformed lines, crashes, and permission requests. (c) **Contract tests** against the real installed CLIs, gated by `AMIRI_LIVE_TESTS=1` because they cost tokens | Every format change seen in the wild becomes a fixture |
| MCP | Protocol tests of AMIRI's server with an MCP client; tool-grant scoping (a session can't call tools it wasn't granted) | |
| Git | Temp repos: status parsing incl. renames, submodules, detached HEAD, conflicts; verify AMIRI never runs a destructive command without a recorded approval | |
| Knowledge | Vault fixtures: zone enforcement, concurrent-edit proposal path, wikilink resolution, FTS rebuild | |
| Policy | Table-driven tests: scope precedence, unknown → ASK, destructive hard-classification | |
| UI | Smoke tests via `tauri-driver` (WebDriver) on Windows: launch, open terminal, run a command, open a project | |
| Packaging | Install on a **clean Windows Sandbox / fresh VM** without dev tools: installer runs per-user without admin, first-run diagnostics correctly report missing Claude/Codex/Git, uninstall leaves user data intact unless chosen | Scripted with a `.wsb` config |

A phase is complete only when its exit criteria (§16) pass on the Windows CI runner and the clean-install test where relevant. "It launches" is never an exit criterion.

**Locale is a permanent test dimension** `[DECISION, from S1]`: target users run Russian and Uzbek Windows. Windows tools localize their output (`mode con` prints `Столбцы:`) and `cmd` writes the OEM code page (866 on ru-RU). The PTY, probe, git and transport suites run under en-US, ru-RU and uz-UZ. Assertions use only locale-neutral values: numbers, exit codes, environment values such as `%OS%`, files the child writes, or output with the encoding forced to UTF-8 (`chcp 65001`, `[Console]::OutputEncoding`). They never match localized labels, in any language.

---

## 16. Implementation roadmap

### 16.1 Proposed changes to the phase order `[DECISION — needs reviewer agreement]`
1. **Add Phase 0 (spikes)** before any product code. Every `[UNVERIFIED]` protocol assumption above is load-bearing. A few days of throwaway spikes beats building adapters on guesses.
2. **Move a minimal Policy Engine and the event log into Phase 1–2**, not Phase 9. Claude permission requests arrive in Phase 2 and need somewhere to go; building without policy and adding it later is how "silently bypassing authorization" happens. Phase 9 becomes hardening: scoped approvals UI, redaction review, audit.
3. **Obsidian (6) and Skills (7) can swap with Codex (4)** if AI-TV needs knowledge continuity before a second agent. `[OPEN]`

### 16.2 Phases and exit criteria

**Phase 0 — Spikes (throwaway code, findings recorded as evidence)**
- S1 ConPTY: portable-pty on the target Windows build: interactive `pwsh`, ANSI, resize, Ctrl+C, and **what happens to the child when the host process dies** (settles D3).
- S2 Claude stream-json: bidirectional stdin/stdout session, `--session-id`/`--resume`, interrupt, `--permission-prompt-tool` round-trip through a toy MCP server; record raw fixtures.
- S3 Codex: **moved to the start of Phase 4** (decisions.md, 2026-09-25); see Phase 4. Its open questions are risk R1 (§16.4).
- S2: close stdin mid-turn and record whether the CLI and its child processes exit (done; S1b showed a child that ignores EOF survives as an orphan). Run within the approved limits in `docs/decisions.md`.
- S1b Process lifetime (done): host kill vs. ConPTY and pipe children; cause of death recorded. See `spikes/s1b-process-lifetime/FINDINGS.md`.
- S4 russh on Windows: key auth via Windows ssh-agent pipe, PTY into tmux, SFTP, reconnect.
- S5 AgentsCommander evaluation (§2.1): fork / lift / reference.
- Exit: a one-page findings note per spike; this document updated, with `[UNVERIFIED]` tags resolved or converted to risks.

**Phase 1 — Windows shell**
Workspace skeleton; `amirid` + named-pipe IPC + Tauri UI; local terminal (PowerShell, CMD, WSL) with xterm.js; process manager; SQLite with events table and migrations; minimal Policy Engine (ALLOW/ASK/DENY with UI prompt); project registry and selection; `amiri doctor` v1.
Exit: PTY integration suite green; UI closes and reopens while a local shell keeps running under `amirid`; the event log shows all actions.

**Phase 2 — Claude Code**
Detection and version range; structured adapter; PTY "open interactively" with session lock; permission requests → Policy Engine; session persistence, resume, pause, archive, export (with redaction).
Exit: fixture + fake-CLI tests green; live contract test green; AMIRI restart during a task → session shows UNKNOWN → reconciles → resumes via `--resume` with no lost history.

**Phase 3 — SSH**
Machine profiles; auth order; host-key flow; connection manager; remote PTY via tmux; remote structured agent via journal; SFTP; environment probe; `ssh.exe` fallback transport.
Exit: SSH integration suite green, including kill-the-connection-mid-task → reconnect → zero lost journal bytes; unknown/mismatched host tests.

**Phase 4 — Codex**
**Gate: spike S3 first.** No Codex adapter code is written until S3 passes (decisions.md, 2026-09-25). S3 covers:
- `codex app-server` thread/turn lifecycle, approval request round-trip, interrupt, and shutdown behaviour; decide `codex-codes` vs. an own client;
- closing stdin mid-turn, and whether Codex and its child processes exit; kill-on-close job containment of the real Codex tree (§9.6);
- Codex's usage-limit signal and its per-profile config mechanism (§6.4);
- whether Codex works natively on Windows or needs WSL;
- the **failover pickup** (§8.4): given a handoff packet with a `takeover` block, a half-finished diff in a scratch repo and a simulated Claude limit event, Codex's first actions are to inspect the diff and run the tests, then continue.

S3 runs within the approved limits in `docs/decisions.md`. Its findings may change the provisional Agent trait (§9.1). If S3 shows Codex needs WSL, this phase's plan is revised before it starts.
Then: Codex adapter; independent sessions; approval routing; manual task routing by role.
Exit: same test tiers as Phase 2; the same handoff packet runs on either provider.

**Phase 5 — Orchestrator**
Full task state machine with guards; roles; handoff packets; repair loop bounds; `AWAITING_APPROVAL`; structured review findings.
Exit: state-machine property tests; an end-to-end scripted task with fake CLIs traverses the happy path and failure → repair → BLOCKED path.

**Phase 5b — Compute** (§10.9)
Starts with spike S6: SkyPilot, dstack and Ray against the user's own machines over SSH (setup without a cloud account, GPU discovery, job survival across a dropped connection, checkpoint resume, Windows client support, license). Then: GPU inventory; job queue with per-machine caps and `compute.start_job` policy; jobs in remote tmux with log streaming; checkpoint resume; results as `EXPERIMENT` evidence. Wraps an existing scheduler if S6 finds one that fits.
Exit: a job survives a killed connection and a remote reboot (resuming from its checkpoint) with no lost log bytes; its metrics and checkpoint hash are recorded as evidence.

**Phase 6 — Obsidian**
Vault discovery; zones; safe writes; FTS index; MCP knowledge tools; session summaries and decision notes.
Exit: concurrent-edit test proves no human edit is ever overwritten.

**Phase 7 — Skills**
SKILL.md format; discovery and ranking; proposal/diff/approve flow; evidence-linked verification markers and linter.
Exit: no path in the test suite produces a VERIFIED marker without an evidence record.

**Phase 8 — Git and project intelligence**
Status panel; pre/post task snapshots; diffs; pre-commit test enforcement; commit recording; "Continue <project>" resolution.

**Phase 9 — Security hardening**
Scoped approvals UI; redaction review; `cargo deny`/audit gates; IPC token rotation; zeroize sensitive buffers (terminal scrollback) before freeing, e.g. the `zeroize` crate (Slice B findings); threat-model review against §6.1.

**Phase 10 — Packaging**
Tauri bundler, NSIS per-user installer; first-run diagnostics; config migration; logging; optional updater; code signing `[OPEN: certificate source]`.
Also: if starting the daemon from inside a restrictive job turns out to matter (Phase 1 only reports it, never escapes the job), the installer adds a per-user on-demand scheduled task that starts `amirid`.
Exit: clean Windows Sandbox install test passes, **including a pass under the uz-UZ locale** (a Phase 1 gap).

### 16.3 Trace: "Continue AI-TV." (requirement 29)

| # | Step | Component | Phase |
|---|---|---|---|
| 1–2 | Identify project, locate repo | Project registry + `.amiri/project.yaml` | 1, 8 |
| 3 | Inspect previous session | Sessions + event log | 2 |
| 4 | Relevant Obsidian notes | Knowledge FTS + project vault folders | 6 |
| 5 | Load skills | Skills discovery | 7 |
| 6–7 | Execution machine, SSH | Profile → Transport → connection manager | 3 |
| 8–9 | Start/resume Claude, GPT when required | Agent adapters, role config | 2, 4, 5 |
| 10–12 | Plan, execute, test | State machine PLANNING→TESTING | 5 |
| 13 | Human approval | Policy Engine, AWAITING_APPROVAL | 1–2, 9 |
| 14 | Cross-agent review | REVIEWING with a different agent | 5 |
| 15 | Measured results | Evidence pipeline | 5, 7 |
| 16–17 | Docs, skill proposals | Vault writes, skill proposals | 6, 7 |
| 18–20 | Preserve session, Git state, reproducibility | Events, git snapshots, dirty-tree flags | 2, 8 |

### 16.4 Named risks after Phase 0
What the design still rests on without proof, after S0–S2, S4 and S5. Each risk names what depends on it and what settles it. A risk is closed by a findings note or a test, and removed from this list in the same commit.

| ID | Risk | What depends on it | Settled by |
|---|---|---|---|
| R1 | **Codex, entirely untested.** `app-server` protocol (lifecycle, interrupt, steer, approval requests); stop behaviour (signals reportedly leave children running); job containment of its process tree; its usage-limit signal; `CODEX_HOME` profiles; native Windows vs WSL. | Phase 4; the Agent trait (R2); Claude→Codex failover (§8.4) | S3, a hard gate at the start of Phase 4 |
| R2 | **Agent trait shaped by one provider.** Only Claude has been tested. | Every adapter, the orchestrator (Phase 5) | Trait provisional (§9.1); S3 may change it; contract tests show any change |
| R3 | **Claude's limit-hit event never observed.** S2 only saw `status: "allowed"`. | Usage-limit pause and automatic failover (§8.4) | First real limit captured from raw stream logs during normal use; never provoked. Until then `provider_failover: ALLOW` acts as ASK |
| R4 | **Claude CLI format drift.** Tested on 2.1.281 only; the stdin schema is under-documented upstream. Claude Code **updates itself**, so the version can change under AMIRI without anyone noticing. | Claude adapter (Phase 2) | Raw capture of every line (§9.1), fixtures from S2, gating on `init.capabilities`, a live contract test per supported version. The CLI version is recorded in every session's events, and `amiri doctor` warns when the installed version is outside the tested range |
| R5 | **Claude account profiles.** Per-profile config directory mechanism not tested. | Account profiles per project (§6.4) | Phase 2 check before profiles ship |
| R6 | **Claude under WSL and the real `claude` process tree under the job.** S2 ran `claude` natively; S1b/S1c proved the job mechanism with stand-in processes. | WSL execution targets; crash containment (§9.6) | Phase 1 process-manager tests (job vs real `claude` tree); Phase 2 WSL check |
| R7 | **ConPTY beyond `cmd.exe`.** `pwsh` 7 and `wsl.exe` as PTY children, a 1-hour soak with output flood, and `wsl --shutdown` while attached were not run. | Phase 1 local terminal | Phase 1 PTY integration suite (§15) |
| R8 | **Real networks.** S4 ran over localhost: no latency, loss, NAT timeouts or keepalive tuning; one server type (Ubuntu in WSL) and one host-key type (ed25519). | Connection manager and reconnect (§10.4), remote agents | Phase 3 SSH suite, including a real remote host |
| R9 | **SSH agent forwarding and `~/.ssh/config`.** Forwarding untested; the `ssh.exe` fallback for `ProxyJump`/`ProxyCommand` untested. | Hosts that need forwarding or jump hosts (§10.6) | Phase 3 |
| R10 | **`rmcp`** version and stdio server support. | AMIRI's MCP server (D6): task tools (Phase 5), vault tools (Phase 6). Not permissions, which go over stdio | Pin and test at first use |
| R11 | **Codex skills format** compatibility with `SKILL.md`. | Skills shared across providers (§12) | Phase 7 |
| R12 | **Unsigned `AMIRI.exe` that spawns shells and agents** may be flagged by SmartScreen or Defender. | Installation and first run (Phase 10) | Phase 10, together with the code-signing decision (§17 Q9) |
| R13 | **User's Claude Code config inside orchestrated sessions.** Without `--safe-mode`, the user's hooks, plugins and settings load in AMIRI-launched sessions. Whether hooks run in `-p` mode is itself unverified. | Predictable agent behaviour and policy (§6); AMIRI never relies on hooks for enforcement | Phase 2: test which settings load, and limit `--setting-sources` (S2's suggested alternative) if they interfere |

Not load-bearing, left unverified: the Obsidian Local REST API serving MCP (§2). The design doesn't use it.

---

## 17. Open questions for the reviewer

1. Accept the phase-order changes in §16.1 (Phase 0 spikes; early minimal policy)?
2. AgentsCommander: reference/lift (recommended) or fork (§2.1)?
3. Where is AI-TV's execution target in practice: the local RTX 5070 Ti machine, a separate Linux GPU server, or both? This decides whether Phase 3 is on the critical path for the first real use.
4. Config format: keep YAML (spec) given the YAML-crate maintenance issue, or use TOML for AMIRI config and YAML only for Obsidian frontmatter?
5. Default for `plan_approval`: ASK (recommended) or ALLOW?
6. Should the reviewer role be required to be a *different provider* from the implementer, or just a different session?
7. Minimum trust bar for third-party crates/repos (§5.3).
8. Licensing for AMIRI itself (MIT/Apache-2.0 dual is the Rust norm and compatible with the MIT/Apache sources above).
9. Code signing: acquire a certificate, use a foundation program (as AgentsCommander plans with SignPath), or ship unsigned initially?
10. `amiri-sessiond` (a minimal process owning only the ConPTYs so local shells survive an `amirid` crash or upgrade, §D3). **Deferred.** In v1 a daemon crash marks sessions UNKNOWN; agent conversations resume from the providers' own session stores; long-running work goes through tmux in WSL or on a remote machine, which already survives. Decide once there is data on real `amirid` crash frequency. S1b settled the mechanism: ConPTY children die from `CTRL_CLOSE_EVENT` when the owning process dies (no job object involved), so a separate `amiri-sessiond` would keep them alive. The option is technically sound; only its value is open.

11. ~~Compute subsystem (§10.9) and spike S6: accept, and where in the phase order?~~ **Decided 2026-09-25:** Phase 5b, with S6 at its start (decisions.md).

---

## Appendix A — Deliberate non-goals for v1
- AMIRI does not reimplement a coding agent, a model API client for chat, or Claude Code/Codex features.
- AMIRI does not sandbox agents; it chooses execution targets and configures agents' own permission systems.
- No cloud component, account, or telemetry.
- No automatic promotion of agent output to verified knowledge.
