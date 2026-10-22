# Spike S4: russh on Windows (findings)

- Date: 2026-09-24. `russh` 0.63 (`ring` backend; the default `aws-lc-rs` needs NASM to build on Windows), `russh-sftp` 3.0, tmux 3.4 on the target.
- Raw evidence: `findings.json`: clean commit `a00ad4f`, non-admin session, exit 0, 8/8 checks passed, each one asserting its expected value.
- Superseded run: `findings-6ebeda6-superseded.json` (clean commit `9c3b89a`) reported 8/8 "ok", but `pty_tmux_resize_reconnect` passed with `size_after_resize: 29x100` against `expected_size: 30x100`. That run only recorded its observations: no check could fail on a wrong value. The mismatch went unexplained until it was queried (below), and `a00ad4f` made every check assert.
- Target (decisions.md, "S4 image"): throwaway distro `amiri-s4-sshd` imported from Ubuntu's official WSL image `ubuntu-noble-wsl-amd64-24.04lts.rootfs.tar.gz`, SHA-256 `2a790896740b14d637dbdc583cce1ba081ac53b9e9cdb46dc09a2f73abbd9934`. It matched the published `SHA256SUMS`, and the signature on that file checked against key `D2EB44626FDDC30B513D5BB71A5D6C4C7DB87C81` (UEC Image Automatic Signing Key). sshd: key-only, `127.0.0.1:22422`, user `amiri`, started by hand outside systemd. The user's own distros were not touched.
- Method: `setup-target.sh`, then sshd, then `ssh-add` of the test key into the Windows agent (commands in `src/main.rs`), then `cargo run --release -- <work-dir>`, then `teardown-target.sh`.

## Observations

| Check | Result |
|---|---|
| Host key pinned in `known_hosts` | Accepted. |
| Same host, different key in `known_hosts` | **Hard fail** at connect (`The server key changed at line 1`). An unknown key is rejected too. |
| Key auth through the Windows agent pipe `\\.\pipe\openssh-ssh-agent` | Works (`AgentClient::connect_named_pipe`). Only the test key is used; other keys in the agent are neither tried nor recorded. |
| Key auth from a key file | Works. |
| Password auth | Refused. Server offers `PublicKey` only. |
| PTY into tmux, resize to 30×100 | tmux **client** is `30x100`. With tmux's default bottom status bar the **pane** is `29x100` and `tput` in the pane agrees. With the status bar off (queried by hand) the pane is `30x100`. |
| Forced disconnect (TCP dropped without `SSH_MSG_DISCONNECT`), reconnect, reattach | tmux session survives. Shell state (`export`ed var) is intact after reattach. Reconnect 105 ms, reconnect + reattach 210 ms (localhost). |
| SFTP round trip, 80 000 bytes of non-ASCII text | Identical bytes back, `stat` size 80 000. |
| `exec "exit 7"` | Exit status 7. |

## Answers for the SSH transport (§10)

1. **russh works on Windows for everything the §10 transport needs:** host-key pinning with a hard fail on change, Windows agent pipe auth (resolves the `[UNVERIFIED]` in §10.2), key files, PTY, resize, exec with exit codes, and SFTP.
2. **Resize: the PTY size belongs to the tmux client, not the pane.** Whatever runs inside tmux sees the client size minus status lines (and minus any splits). With the status bar on, the terminal AMIRI renders is one row taller than what the remote program sees. `[DECISION]` (decisions.md, 2026-09-25): AMIRI turns the status bar off in the sessions it creates (`tmux set -t <s> status off`), so the sizes match exactly.
3. **Reconnect after a network drop works as §10 assumes:** tmux keeps the session and state, and a fresh SSH session reattaches in about 0.2 s on localhost.
4. `sftp.write()` in `russh-sftp` 3.0 opens without `CREATE` and fails on a new file. Use `sftp.create()` to write new files. Recorded in `docs/dependencies.md` as pinned-version behaviour, to recheck on upgrade.
5. Build: `default-features = false, features = ["ring", "flate2", "rsa"]` avoids the NASM dependency of `aws-lc-rs` on Windows.

## Teardown

`teardown-target.sh` after the rerun: test key removed from the Windows agent (`agent_still_has_test_key=false`), `wsl --unregister amiri-s4-sshd` exit 0, `distro_still_listed=false`, no VHDX left. The verified image and the work dir stay in `%TEMP%\amiri-s4` (outside the repo).

## Not covered

- **Agent forwarding.** Not tested. It is the open half of the `[UNVERIFIED]` tag on `russh` in §5.1; the agent-pipe half (§5.1 and §10.2) is resolved above.
- `~/.ssh/config` handling, `ProxyJump`/`ProxyCommand` (the `ssh.exe` fallback transport).
- Real network conditions: latency, packet loss, NAT timeouts, keepalive tuning. Everything ran over localhost.
- A non-WSL server (plain Linux or macOS host) and other host-key types (only ed25519).
