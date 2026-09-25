# AMIRI — Dependency register

One row per dependency (ARCHITECTURE §5.3): purpose, reason, alternatives, license, pinned version, date reviewed. "Known behaviour" lists what AMIRI relies on or works around in the pinned version; recheck it whenever the version changes.

| Crate | Purpose | Reason | Alternatives | License | Pinned | Reviewed | Known behaviour (recheck on upgrade) |
|---|---|---|---|---|---|---|---|
| `russh` | SSH client: exec, PTY, agent auth | Pure Rust, async, no OpenSSL/libssh2 C build on Windows (§5.1). S4: host-key pinning, Windows agent pipe, PTY, exec work on Windows. | `ssh2` (libssh2, blocking); `ssh.exe` fallback transport (§10.6) | Apache-2.0 | 0.63.3 | 2026-09-25 | Build with `default-features = false, features = ["ring", "flate2", "rsa"]`: the default `aws-lc-rs` backend needs NASM on Windows. Agent forwarding not yet verified (S4 "Not covered"). |
| `russh-sftp` | SFTP over a `russh` channel | Native SFTP on the same connection (§10.4). S4: 80 kB non-ASCII round trip identical. | `scp` via the `ssh.exe` transport | Apache-2.0 | 3.0.0 | 2026-09-25 | `SftpSession::write()` opens without `CREATE` and fails on a new file; use `create()` for new files (S4). |

## Product workspace (Phase 1)

Pinned by the committed `Cargo.lock` (123 packages in total at Slice A, including transitive ones). The rows below are the direct dependencies. `cargo deny` / `cargo audit` gates come in Phase 9.

| Crate | Purpose | Reason | Alternatives | License | Pinned | Reviewed | Known behaviour (recheck on upgrade) |
|---|---|---|---|---|---|---|---|
| `tokio` | Async runtime, named pipes | `tokio::net::windows::named_pipe` covers the IPC server and client with first-instance, remote-reject and raw security-attribute options, so no extra IPC crate | `interprocess` (extra dependency) | MIT | 1.53.1 | 2026-09-25 | `ServerOptions::create_with_security_attributes_raw` is `unsafe`; the SECURITY_ATTRIBUTES must outlive the call |
| `windows-sys` | Win32 APIs: tokens, SDDL, mutexes, jobs, processes, registry, console, session-end window | Raw bindings, no runtime cost; used by the spikes | `windows` (heavier), `winreg` + others (several crates) | MIT OR Apache-2.0 | 0.61.2 | 2026-09-25 | Feature list in the workspace `Cargo.toml` |
| `rusqlite` (`bundled`) | SQLite event log | §5.1; bundled SQLite (`libsqlite3-sys` 0.38.2) | `sqlx` | MIT | 0.40.2 | 2026-09-25 | WAL + `synchronous=NORMAL`; `VACUUM INTO` for pre-migration backups |
| `serde` / `serde_json` | Serialization | Standard | — | MIT OR Apache-2.0 | 1.0.229 / 1.0.151 | 2026-09-25 | |
| `uuid` (`v4`, `v7`) | Event IDs (v7), IPC token and test names (v4) | §5.1 | `ulid` | Apache-2.0 OR MIT | 1.26.1 | 2026-09-25 | |
| `thiserror` | Store error type | §5.1 | — | MIT OR Apache-2.0 | 2.0.21 | 2026-09-25 | `portable-pty` also pulls `thiserror` 1.0.69 (duplicate) |
| `tracing` / `tracing-subscriber` (`json`, `env-filter`) / `tracing-appender` | Daemon logs, JSON, daily files in `<home>\logs` | §5.1, §14 | `log` | MIT | 0.1.44 / 0.3.23 / 0.2.5 | 2026-09-25 | Level via `AMIRI_LOG` |
| `clap` (`derive`) | CLI and daemon arguments | Standard | `argh` | MIT OR Apache-2.0 | 4.6.7 | 2026-09-25 | |
| `portable-pty` | ConPTY (dev-dependency in Slice A tests; product use in Slice B) | §5.1; the version S1 tested | raw ConPTY via `windows-sys` | MIT | 0.9.0 | 2026-09-25 | ConPTY sends DSR (`ESC[6n`) and stalls until answered (S1 finding 5; hit again in Slice A tests) |
| `tempfile` (dev) | Test data folders | Standard | — | MIT OR Apache-2.0 | 3.27.0 | 2026-09-25 | |
| `gitleaks` (tool, not a dependency) | Secret scan before pushes | Reviewer's requirement | — | MIT | 8.30.1 | 2026-09-25 | Release zip SHA-256 `d29144de…32afc4e` verified against the published checksums |
