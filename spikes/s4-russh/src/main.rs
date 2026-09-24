//! Spike S4 (docs/ARCHITECTURE.md §16.2): russh on Windows against a throwaway
//! WSL sshd (see setup-target.sh). Checks host-key pinning and hard-fail on
//! mismatch, key auth through the Windows ssh-agent pipe and from a key file,
//! password refusal, PTY into tmux with resize, forced disconnect → reconnect →
//! reattach, SFTP round trip, and exec exit codes. Prints one JSON object.
//!
//! Usage: `cargo run --release -- <work-dir>` (work-dir from setup-target.sh; the
//! test key must already be in the Windows agent for the agent check).
//! Throwaway code.

use russh::client::{self, AuthResult, Handle};
use russh::keys::agent::client::AgentClient;
use russh::keys::{check_known_hosts_path, load_public_key, load_secret_key, PrivateKeyWithHashAlg};
use russh::keys::PublicKeyOrCertificate;
use russh::{ChannelMsg, Disconnect};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::time::timeout;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 22422;
const USER: &str = "amiri";
const AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";
const TMUX: &str = "amiri_s4";
const STEP: Duration = Duration::from_secs(20);

type CheckResult = Result<Value, String>;

/// Accepts the server key only if `known_hosts` has it for [HOST]:PORT.
struct Pinned {
    known_hosts: PathBuf,
}

impl client::Handler for Pinned {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => {
                // KeyChanged (mismatch) is an error, unknown is Ok(false): both reject.
                check_known_hosts_path(HOST, PORT, key, &self.known_hosts).map_err(russh::Error::from)
            }
            PublicKeyOrCertificate::Certificate(_) => Ok(false),
        }
    }
}

fn config() -> Arc<client::Config> {
    Arc::new(client::Config { inactivity_timeout: Some(Duration::from_secs(60)), ..Default::default() })
}

async fn connect(known_hosts: &Path) -> Result<Handle<Pinned>, String> {
    let h = Pinned { known_hosts: known_hosts.to_path_buf() };
    timeout(STEP, client::connect(config(), (HOST, PORT), h))
        .await
        .map_err(|_| "connect timeout".to_string())?
        .map_err(|e| format!("connect: {e}"))
}

async fn auth_keyfile(s: &mut Handle<Pinned>, work: &Path) -> Result<AuthResult, String> {
    let key = load_secret_key(work.join("s4_key"), None).map_err(|e| format!("load key: {e}"))?;
    s.authenticate_publickey(USER, PrivateKeyWithHashAlg::new(Arc::new(key), None))
        .await
        .map_err(|e| format!("auth: {e}"))
}

/// Connected and authenticated with the key file.
async fn session(work: &Path) -> Result<Handle<Pinned>, String> {
    let mut s = connect(&work.join("known_hosts")).await?;
    match auth_keyfile(&mut s, work).await? {
        AuthResult::Success => Ok(s),
        other => Err(format!("key auth failed: {other:?}")),
    }
}

/// Read channel data until `needle` appears; returns everything read.
async fn read_until(ch: &mut russh::Channel<client::Msg>, needle: &str) -> Result<String, String> {
    let deadline = Instant::now() + STEP;
    let mut buf = String::new();
    while !buf.contains(needle) {
        let left = deadline.saturating_duration_since(Instant::now());
        match timeout(left, ch.wait()).await {
            Err(_) => return Err(format!("timeout waiting for {needle:?}; tail {:?}", tail(&buf))),
            Ok(None) => return Err(format!("channel closed waiting for {needle:?}")),
            Ok(Some(ChannelMsg::Data { data })) => buf.push_str(&String::from_utf8_lossy(&data)),
            Ok(Some(_)) => {}
        }
    }
    Ok(buf)
}

fn tail(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    c[c.len().saturating_sub(160)..].iter().collect()
}

async fn exec(s: &Handle<Pinned>, cmd: &str) -> Result<(String, Option<u32>), String> {
    let mut ch = s.channel_open_session().await.map_err(|e| format!("open: {e}"))?;
    ch.exec(true, cmd).await.map_err(|e| format!("exec: {e}"))?;
    let (mut out, mut code) = (String::new(), None);
    let deadline = Instant::now() + STEP;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match timeout(left, ch.wait()).await {
            Err(_) => return Err("exec timeout".into()),
            Ok(None) => break,
            Ok(Some(ChannelMsg::Data { data })) => out.push_str(&String::from_utf8_lossy(&data)),
            Ok(Some(ChannelMsg::ExitStatus { exit_status })) => code = Some(exit_status),
            Ok(Some(_)) => {}
        }
    }
    Ok((out, code))
}

async fn tmux_pty(s: &Handle<Pinned>, cmd: &str) -> Result<russh::Channel<client::Msg>, String> {
    let ch = s.channel_open_session().await.map_err(|e| format!("open: {e}"))?;
    ch.request_pty(true, "xterm-256color", 120, 40, 0, 0, &[]).await.map_err(|e| format!("pty: {e}"))?;
    ch.exec(true, cmd).await.map_err(|e| format!("exec: {e}"))?;
    Ok(ch)
}

// ---- checks ----------------------------------------------------------------

async fn check_hostkey_pinned(work: &Path) -> CheckResult {
    let s = connect(&work.join("known_hosts")).await?;
    let _ = s.disconnect(Disconnect::ByApplication, "", "").await;
    Ok(json!({ "accepted": true }))
}

/// A known_hosts entry for our host with a *different* key of the same type must hard-fail.
async fn check_hostkey_mismatch(work: &Path) -> CheckResult {
    let wrong = load_public_key(work.join("s4_key.pub")).map_err(|e| format!("load pub: {e}"))?;
    let path = work.join("known_hosts_wrong");
    let line = format!("[{HOST}]:{PORT} {}\n", wrong.to_openssh().map_err(|e| e.to_string())?);
    std::fs::write(&path, line).map_err(|e| e.to_string())?;
    let r = connect(&path).await;
    let _ = std::fs::remove_file(&path);
    match r {
        Ok(_) => Err("connected despite a mismatched host key".into()),
        Err(e) => Ok(json!({ "rejected": true, "error": e })),
    }
}

async fn check_auth_agent(work: &Path) -> CheckResult {
    let want = load_public_key(work.join("s4_key.pub")).map_err(|e| format!("load pub: {e}"))?;
    let mut agent = AgentClient::connect_named_pipe(AGENT_PIPE).await.map_err(|e| format!("agent: {e}"))?;
    let ids = agent.request_identities().await.map_err(|e| format!("identities: {e}"))?;
    // Only the test key is used; other keys in the user's agent are neither tried nor recorded.
    let key = ids.iter().map(|i| i.public_key().into_owned()).find(|k| k.key_data() == want.key_data())
        .ok_or("test key not in the Windows agent")?;
    let mut s = connect(&work.join("known_hosts")).await?;
    let r = s.authenticate_publickey_with(USER, key, None, &mut agent).await.map_err(|e| format!("{e:?}"))?;
    let ok = matches!(r, AuthResult::Success);
    let who = if ok { exec(&s, "whoami").await?.0.trim().to_string() } else { String::new() };
    let _ = s.disconnect(Disconnect::ByApplication, "", "").await;
    Ok(json!({ "success": ok, "whoami": who, "pipe": AGENT_PIPE }))
}

async fn check_auth_keyfile(work: &Path) -> CheckResult {
    let s = session(work).await?;
    let who = exec(&s, "whoami").await?.0.trim().to_string();
    let _ = s.disconnect(Disconnect::ByApplication, "", "").await;
    Ok(json!({ "success": true, "whoami": who }))
}

async fn check_password_refused(work: &Path) -> CheckResult {
    let mut s = connect(&work.join("known_hosts")).await?;
    let r = s.authenticate_password(USER, "not-the-password").await.map_err(|e| format!("auth: {e}"))?;
    match r {
        AuthResult::Success => Err("password auth succeeded".into()),
        AuthResult::Failure { remaining_methods, .. } => {
            Ok(json!({ "refused": true, "remaining_methods": format!("{remaining_methods:?}") }))
        }
    }
}

/// PTY into a new tmux session, set state, resize; then drop the TCP connection
/// without a disconnect message, reconnect, reattach, and read the state back.
async fn check_tmux_reconnect(work: &Path) -> CheckResult {
    let _ = exec(&session(work).await?, &format!("tmux kill-session -t {TMUX} 2>/dev/null; true")).await;
    let s = session(work).await?;
    let mut ch = tmux_pty(&s, &format!("tmux new-session -A -s {TMUX}")).await?;
    ch.data_bytes("export AMIRI_S4_VAR=42; echo AMIRI_$((6*7))_SET\r").await.map_err(|e| e.to_string())?;
    read_until(&mut ch, "AMIRI_42_SET").await?;
    ch.window_change(100, 30, 0, 0).await.map_err(|e| e.to_string())?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    // `$((1+1))` keeps the end marker out of the echoed command line.
    ch.data_bytes("echo SIZE_$(tput lines)x$(tput cols)_$((1+1))END\r").await.map_err(|e| e.to_string())?;
    let size_out = read_until(&mut ch, "_2END").await?;
    let size = size_out.rsplit("SIZE_").next().and_then(|s| s.split("_2END").next()).unwrap_or("").to_string();

    // Forced disconnect: drop everything without SSH_MSG_DISCONNECT (≈ network loss / amirid crash).
    drop(ch);
    drop(s);
    tokio::time::sleep(Duration::from_secs(2)).await;

    let started = Instant::now();
    let s2 = session(work).await?;
    let reconnect_ms = started.elapsed().as_millis();
    let (has, _) = exec(&s2, &format!("tmux has-session -t {TMUX} && echo ALIVE")).await?;
    let mut ch2 = tmux_pty(&s2, &format!("tmux attach -t {TMUX}")).await?;
    ch2.data_bytes("echo VAR_${AMIRI_S4_VAR}_END\r").await.map_err(|e| e.to_string())?;
    let back = read_until(&mut ch2, "VAR_42_END").await.is_ok();
    let reattach_ms = started.elapsed().as_millis();
    drop(ch2);
    let _ = exec(&s2, &format!("tmux kill-session -t {TMUX}")).await;
    let _ = s2.disconnect(Disconnect::ByApplication, "", "").await;
    Ok(json!({
        "size_after_resize": size, "expected_size": "30x100",
        "tmux_alive_after_forced_disconnect": has.contains("ALIVE"),
        "shell_state_after_reattach": back,
        "reconnect_ms": reconnect_ms, "reconnect_and_reattach_ms": reattach_ms,
    }))
}

async fn check_sftp(work: &Path) -> CheckResult {
    let s = session(work).await?;
    let ch = s.channel_open_session().await.map_err(|e| format!("open: {e}"))?;
    ch.request_subsystem(true, "sftp").await.map_err(|e| format!("subsystem: {e}"))?;
    let sftp = russh_sftp::client::SftpSession::new(ch.into_stream()).await.map_err(|e| format!("sftp: {e}"))?;
    let path = "/home/amiri/s4_sftp.txt";
    // Non-ASCII on purpose: ru-RU and uz-UZ are permanent test locales.
    let payload = "AMIRI S4 Привет Salom oʻzbek ✓\n".repeat(2000);
    // Not `sftp.write()`: it opens with WRITE only (no CREATE) and fails on a new file.
    let mut f = sftp.create(path).await.map_err(|e| format!("create: {e}"))?;
    f.write_all(payload.as_bytes()).await.map_err(|e| format!("write: {e}"))?;
    f.shutdown().await.map_err(|e| format!("close: {e}"))?;
    let back = sftp.read(path).await.map_err(|e| format!("read: {e}"))?;
    let size = sftp.metadata(path).await.map_err(|e| format!("stat: {e}"))?.size;
    sftp.remove_file(path).await.map_err(|e| format!("rm: {e}"))?;
    let _ = s.disconnect(Disconnect::ByApplication, "", "").await;
    Ok(json!({ "bytes": payload.len(), "round_trip_identical": back == payload.as_bytes(),
               "stat_size": size }))
}

async fn check_exec_exit_code(work: &Path) -> CheckResult {
    let s = session(work).await?;
    let (_, code) = exec(&s, "exit 7").await?;
    let _ = s.disconnect(Disconnect::ByApplication, "", "").await;
    Ok(json!({ "expected": 7, "observed": code }))
}

/// Commit the spike ran against; `findings.json` is excluded (the shell truncates it first).
fn git_state() -> Value {
    let dir = env!("CARGO_MANIFEST_DIR");
    let git = |args: &[&str]| {
        Command::new("git").arg("-C").arg(dir).args(args).output().ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let head = git(&["rev-parse", "HEAD"]);
    let dirty = git(&["status", "--porcelain", "--untracked-files=no", "--", ":/", ":(exclude)findings.json"])
        .map(|s| !s.is_empty());
    json!({ "commit": head, "dirty_tracked_files": dirty })
}

#[tokio::main]
async fn main() {
    let Some(work) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: s4-russh <work-dir>");
        std::process::exit(2);
    };
    let mut results = Map::new();
    let mut failed = 0;
    macro_rules! run {
        ($name:literal, $check:expr) => {{
            let started = Instant::now();
            let entry = match $check.await {
                Ok(v) => json!({ "ok": true, "observed": v, "ms": started.elapsed().as_millis() }),
                Err(e) => {
                    failed += 1;
                    json!({ "ok": false, "error": e, "ms": started.elapsed().as_millis() })
                }
            };
            eprintln!("{}: {}", $name, if entry["ok"] == true { "ran" } else { "FAILED" });
            results.insert($name.to_string(), entry);
        }};
    }
    run!("hostkey_pinned", check_hostkey_pinned(&work));
    run!("hostkey_mismatch_hard_fail", check_hostkey_mismatch(&work));
    run!("auth_windows_agent_pipe", check_auth_agent(&work));
    run!("auth_key_file", check_auth_keyfile(&work));
    run!("password_refused", check_password_refused(&work));
    run!("pty_tmux_resize_reconnect", check_tmux_reconnect(&work));
    run!("sftp_round_trip", check_sftp(&work));
    run!("exec_exit_code", check_exec_exit_code(&work));
    let target: Value = std::fs::read_to_string(work.join("target.json")).ok()
        .and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    let report = json!({ "spike": "S4-russh", "russh": "0.63", "russh_sftp": "3.0", "git": git_state(),
                         "target": target, "checks": results });
    println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
    std::process::exit(if failed > 0 { 1 } else { 0 });
}
