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
| `portable-pty` | ConPTY shells (`amiri-exec`, Slice B) | §5.1; the version S1 tested | raw ConPTY via `windows-sys` | MIT | 0.9.0 | 2026-09-25 | ConPTY sends DSR (`ESC[6n`) and stalls until answered (S1 finding 5); its output doesn't reach EOF when the shell exits (only when the pseudoconsole closes), so exit is watched separately; it asks the outer terminal for win32-input-mode (`ESC[?9001h`) |
| `base64` | Terminal bytes in JSON-RPC (`session.input`, `session.output`, replay) | Standard; binary-safe | hex (twice the size) | MIT OR Apache-2.0 | 0.23.1 | 2026-09-25 | |
| `windows-sys` features added in Slice B | `Win32_UI_Shell` + `Win32_System_Com` (known folders), `Win32_Globalization` (UTF-8 console code page) | | | | 0.61.2 | 2026-09-25 | |
| `tempfile` (dev) | Test data folders | Standard | — | MIT OR Apache-2.0 | 3.27.0 | 2026-09-25 | |
| `gitleaks` (tool, not a dependency) | Secret scan before every push (over the new commits; full history before the first push) | Reviewer's requirement | — | MIT | 8.30.1 | 2026-09-25 | Release zip SHA-256 `d29144de…32afc4e` verified against the published checksums; the extracted `gitleaks.exe` is SHA-256 `17157e2e…4d78fcb7c` (recorded 2026-10-05) |

## Desktop UI (`app/`, Slice C)

npm packages are exact versions (no ranges) and locked by `app/package-lock.json` (registry.npmjs.org, with integrity hashes); `app/.npmrc` turns lifecycle scripts off. The app crate's Tauri crates are `=`-pinned in `app/src-tauri/Cargo.toml` and locked by its own `Cargo.lock`.

| Dependency | Purpose | Reason | Alternatives | License | Pinned | Reviewed | Known behaviour (recheck on upgrade) |
|---|---|---|---|---|---|---|---|
| `tauri` / `tauri-build` (+ `tauri-runtime`, `-runtime-wry`, `-macros`, `-utils`, `-codegen`) | Desktop shell over WebView2; commands and events between webview and Rust | §5.1 | Electron (bundles Chromium) | MIT OR Apache-2.0 | 2.11.6 / 2.6.3 (runtime 2.11.3, wry 2.11.4, macros 2.6.3, utils 2.9.3, codegen 2.6.3) | 2026-10-05 | 2.11.6 asks for its sub-crates with caret ranges; a fresh resolve picks 2.12 ones it doesn't compile with, hence the explicit pins. App commands go through the permission system (`AppManifest` in `build.rs`). |
| `@tauri-apps/api` / `@tauri-apps/cli` (dev) | Frontend side of commands/events; build tool | Matches the Rust side | — | MIT OR Apache-2.0 | 2.11.1 / 2.11.5 | 2026-10-05 | The CLI is a project devDependency (`npx tauri`), not a global install. |
| `react` / `react-dom` | UI | §5.2 | — | MIT | 19.3.0 | 2026-10-05 | No component library (§5.2). |
| `zustand` | UI state | Small, no providers | React context | MIT | 5.0.15 | 2026-10-05 | |
| `@xterm/xterm` / `@xterm/addon-fit` | Terminal rendering | The standard web terminal | — | MIT | 6.0.0 / 0.11.0 | 2026-10-05 | `write(data, callback)` drives flow control; xterm drops input past ~50 MB of pending writes with only a console warning. |
| `vite` / `@vitejs/plugin-react` (dev) | Build | | | MIT | 8.3.1 / 6.1.1 | 2026-10-05 | |
| `typescript` (dev) | Type checking | | | Apache-2.0 | 5.9.3 | 2026-10-05 | Not 7.0 (new native compiler, first major) yet. |
| Node.js (tool) | Builds, unit tests, E2E harness | | | MIT | 22.23.1 (CI pins it; `engines` ≥ 22.18) | 2026-10-05 | `npm test` runs TypeScript directly (type stripping, on by default from 22.18). |
| `msedgedriver` (tool, E2E only, not shipped) | W3C WebDriver for WebView2: the UI E2E tests attach to each UI's debugging port | Replaces `tauri-driver` (decisions.md 2026-10-05): the tests launch the UI themselves | `tauri-driver` | Microsoft software license terms (proprietary; a developer tool here, never redistributed) | Must equal the installed WebView2: **154.0.4258.62** (154.0.4258.53 until 2026-10-09) | 2026-10-05 | `edgedriver_win64.zip` SHA-256 `C99F91B6…1407B42E` for .62, `16ACCCCA…3CAC8D72B` for .53 (full value in `app/e2e/fetch-driver.ps1`), plus an Authenticode check (status Valid, signer `O=Microsoft Corporation`). WebView2 updates itself: on a new version the harness fails with *"WebView2 is now X, msedgedriver is Y: update the driver"* until a row is added. |
