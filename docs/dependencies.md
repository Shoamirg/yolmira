# AMIRI — Dependency register

One row per dependency (ARCHITECTURE §5.3): purpose, reason, alternatives, license, pinned version, date reviewed. "Known behaviour" lists what AMIRI relies on or works around in the pinned version; recheck it whenever the version changes.

| Crate | Purpose | Reason | Alternatives | License | Pinned | Reviewed | Known behaviour (recheck on upgrade) |
|---|---|---|---|---|---|---|---|
| `russh` | SSH client: exec, PTY, agent auth | Pure Rust, async, no OpenSSL/libssh2 C build on Windows (§5.1). S4: host-key pinning, Windows agent pipe, PTY, exec work on Windows. | `ssh2` (libssh2, blocking); `ssh.exe` fallback transport (§10.6) | Apache-2.0 | 0.63.3 | 2026-09-25 | Build with `default-features = false, features = ["ring", "flate2", "rsa"]`: the default `aws-lc-rs` backend needs NASM on Windows. Agent forwarding not yet verified (S4 "Not covered"). |
| `russh-sftp` | SFTP over a `russh` channel | Native SFTP on the same connection (§10.4). S4: 80 kB non-ASCII round trip identical. | `scp` via the `ssh.exe` transport | Apache-2.0 | 3.0.0 | 2026-09-25 | `SftpSession::write()` opens without `CREATE` and fails on a new file; use `create()` for new files (S4). |
