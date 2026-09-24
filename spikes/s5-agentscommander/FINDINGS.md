# Spike S5 — AgentsCommander evaluation: findings (static review)

- Date: 2026-09-24
- Subject: `mblua/AgentsCommander` @ `08078683e9ce85847d9ee4da25db7b5398e7cd1e` (shallow clone, outside this repo), MIT
- Method: **read-only code review**; nothing built or run, because this session was elevated and building runs third-party build scripts and npm lifecycle hooks. The "build and run it for a day" part of the §2.1 gate is still pending and must happen in a non-admin session.
- Key claims spot-checked by hand: Tauri coupling (`src-tauri/src/pty/output.rs:7`), Job Object kill-on-close (`pty/job.rs`), absence of DSR handling (no `ESC[6n` anywhere in `src-tauri/src`), `pty/` size (51,300 non-blank lines).

## Observations

| Question | Finding | Where |
|---|---|---|
| Size of the PTY layer | ~51,300 non-blank lines in 32 files under `src-tauri/src/pty/`, plus ~20,600 in `session/`. A full product subsystem, not a ConPTY wrapper. | `src-tauri/src/pty/` |
| Separability | **Not separable.** The output fan-out emits Tauri events straight from the read loop; the local backend owns a `GitWatcher` that needs a Tauri `AppHandle` (their own comments say it can't be built in a unit test). The `PtyBackend` trait carries AC domain types (Telegram senders, resource-monitor slots, container credentials, window-label attach model). | `pty/output.rs:7,412`, `pty/local_backend.rs:99,205`, `pty/backend.rs:206-326` |
| portable-pty | 0.8 (AMIRI's spikes use 0.9) | `src-tauri/Cargo.toml:15` |
| Job Objects | **Yes, done well**: non-inheritable job handle + `KILL_ON_JOB_CLOSE`, spawn suspended → assign → resume so the child can't escape before assignment, settlement polls `ActiveProcesses` until 0, Windows CI test proves child + grandchild tree kill. | `pty/job.rs` (~250 lines of real logic, no Tauri deps) |
| DSR (`ESC[6n`) answering | Not found | — |
| Locale / code page handling | Not found | — |
| UTF-8 split across reads | Relies on the stateful `vt100` parser; no explicit boundary buffering | `pty/output.rs:59` |
| `crates/session-bridge` | **Not** an `amiri-sessiond` analogue: a container-resident PTY owner that connects outbound over WebSocket to AC's hosted API with bearer/ticket auth. Irrelevant to local crash survival. | `crates/session-bridge/src/lib.rs` |
| Tests | Windows PTY tests exist and run on `windows-latest` CI | `src-tauri/tests/pty_*.rs`, `pty/job.rs:636-859` |
| Build-time execution | One `build.rs` (Tauri build + manifest; no network). Root `package.json`: `prepare: husky`. `npm/package.json`: `postinstall: node install.js`, which downloads a release binary from GitHub with SHA-256 check. No obfuscation, no secrets found. | `src-tauri/build.rs`, `npm/install.js` |
| Licensing | MIT. Exception: the bundled DejaVu Sans Mono font (Bitstream Vera-derived license) in the snapshot renderer, outside the PTY layer. | `THIRD_PARTY_NOTICES.md:15-26` |

## Recommendation: REFERENCE ONLY — this reverses the tentative "lift" decision

The 2026-09-24 decision was "lift the PTY layer **if S5 finds it clean and separable**". It is well-engineered but not separable: extracting a `PtySession` would mean cutting Tauri events, git watching, Telegram, resource monitoring, containers and idle detection out of ~50k entangled lines. AMIRI's own S1/S1b code already covers the raw `portable-pty` calls, and AC lacks the two Windows fixes AMIRI already needs (DSR answering, locale handling).

Reimplement from scratch in AMIRI, using AC as a design reference (credit in `docs/dependencies.md`; no MIT notice needed when no code is copied):

1. **Job Object pattern** (`pty/job.rs`): non-inheritable handle, `CREATE_SUSPENDED` → assign → resume, settlement by `ActiveProcesses == 0`. This is exactly the S1b recommendation and should be the basis of `amirid`'s agent containment. ~1–2 days.
2. **Startup resize gate** (`pty/backend.rs:90-261`): hold back resizes until the child has rendered.
3. **Injection-safe PowerShell/cmd argument adapters** (`pty/local_backend.rs` §4.1–4.6): design reference for launching agents with user-supplied arguments.

Needs reviewer agreement, because it changes the recorded decision (see `docs/decisions.md`).
