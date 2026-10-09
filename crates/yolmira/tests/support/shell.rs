//! Drive a daemon session over IPC like a client would: one connection attached for
//! output, one for input. Markers are computed by the shell (`%OS%`, `6*7`), so the
//! echoed command line never matches by accident.
#![allow(dead_code)]

use super::{tail_chars, TestEnv};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use yolmira_ipc::protocol::{
    methods, notifications, SessionAttachResult, SessionExited, SessionIdParams, SessionInfo,
    SessionInputParams, SessionOutput, SessionResizeParams,
};
use yolmira_ipc::Client;

/// One command round trip. Generous: the first command in a fresh Windows PowerShell
/// builds its module-analysis cache, which is slow on a new machine (CI).
pub const STEP: Duration = Duration::from_secs(90);
/// Allowance for a shell to start (cold machines, CI).
pub const SHELL_START: Duration = Duration::from_secs(120);
/// Overall limit for one PTY test: a cold PowerShell start plus a few steps.
pub const PTY_TEST: Duration = Duration::from_secs(300);
/// The test transcript keeps this much of the latest output.
const TRANSCRIPT_KEEP: usize = 64 * 1024;
/// `Shell::text` keeps at least this much of the latest output (an hour of flood must
/// not grow the test process without bound); trimmed when it reaches twice this.
pub const TEXT_KEEP: usize = 8 * 1024 * 1024;

/// Error text including the daemon's `why` (the RPC message alone is only the `what`).
fn describe(e: yolmira_ipc::ClientError) -> String {
    match e {
        yolmira_ipc::ClientError::Rpc(r) => match r.data {
            Some(d) => format!("{}: {}", d.what, d.why),
            None => r.message,
        },
        other => other.to_string(),
    }
}

pub struct Shell {
    pub id: String,
    pub info: SessionInfo,
    ctl: Client,
    feed: Client,
    /// Output since the last `reset` (lossy UTF-8), including the replay; at most the
    /// latest `2 * TEXT_KEEP` bytes.
    pub text: String,
    /// `reset` notifications received: 1 for the attach replay, one more per resync.
    pub resets: u32,
    /// Output bytes received since attaching (the replay counts first).
    replayed: u64,
    pub exited: Option<Option<i64>>,
    /// For failure reports: the daemon's log folder lives here.
    home: PathBuf,
    /// Mirrors `text` into the test's transcript (`TestEnv::report`).
    transcript: Arc<Mutex<String>>,
}

impl Shell {
    /// Create a session with `shell` (e.g. `{"kind":"cmd"}`) and attach to it.
    pub async fn open(env: &TestEnv, shell: Value, cwd: Option<&str>) -> Result<Self, String> {
        let mut ctl = env.client().await;
        let info: SessionInfo = ctl
            .call(
                methods::SESSION_CREATE,
                json!({ "shell": shell, "cwd": cwd, "cols": 120, "rows": 30 }),
            )
            .await
            .map_err(describe)?;
        let is_powershell = matches!(shell["kind"].as_str(), Some("powershell" | "pwsh"));
        let mut s = Self::attach(env, &info.id, ctl).await?;
        if is_powershell {
            s.wait_powershell_prompt().await;
        }
        Ok(s)
    }

    /// Wait for the `PS …> ` prompt before typing, as a person would. Windows
    /// PowerShell can take far longer than a test step to start on a cold machine (CI);
    /// the prompt text isn't localized.
    pub async fn wait_powershell_prompt(&mut self) {
        let ready = |t: &str| t.find("PS ").is_some_and(|i| t[i..].contains("> "));
        let found = tokio::time::timeout(SHELL_START, async {
            while !ready(&self.text) {
                if !self.pump().await {
                    return false;
                }
            }
            true
        })
        .await
        .unwrap_or(false);
        if !found {
            self.fail(&format!("no PowerShell prompt within {SHELL_START:?}"));
        }
    }

    /// Attach to an existing session on fresh connections.
    pub async fn attach(env: &TestEnv, id: &str, ctl: Client) -> Result<Self, String> {
        let mut feed = env.client().await;
        let r: SessionAttachResult = feed
            .call(
                methods::SESSION_ATTACH,
                json!(SessionIdParams { id: id.into() }),
            )
            .await
            .map_err(describe)?;
        let mut shell = Self {
            id: id.into(),
            info: r.info,
            ctl,
            feed,
            text: String::new(),
            resets: 0,
            replayed: 0,
            exited: None,
            home: env.home(),
            transcript: Arc::clone(&env.transcript),
        };
        shell.read_replay(r.replay_bytes).await;
        Ok(shell)
    }

    /// The scrollback arrives as `session.output` notifications, the first with `reset`.
    async fn read_replay(&mut self, bytes: u64) {
        let done = tokio::time::timeout(STEP, async {
            while self.replayed < bytes {
                if !self.pump().await {
                    return false;
                }
            }
            true
        })
        .await
        .unwrap_or(false);
        if !done {
            self.fail(&format!(
                "replay incomplete: {} of {bytes} bytes",
                self.replayed
            ));
        }
    }

    pub async fn send(&mut self, text: &str) {
        self.send_bytes(text.as_bytes()).await;
    }

    pub async fn send_bytes(&mut self, bytes: &[u8]) {
        let p = SessionInputParams {
            id: self.id.clone(),
            data: B64.encode(bytes),
        };
        if let Err(e) = self.ctl.call_value(methods::SESSION_INPUT, json!(p)).await {
            self.fail(&format!("session.input: {e}"));
        }
    }

    pub async fn resize(&mut self, cols: u16, rows: u16) {
        let p = SessionResizeParams {
            id: self.id.clone(),
            cols,
            rows,
        };
        if let Err(e) = self.ctl.call_value(methods::SESSION_RESIZE, json!(p)).await {
            self.fail(&format!("session.resize: {e}"));
        }
    }

    pub async fn kill(&mut self) {
        let p = SessionIdParams {
            id: self.id.clone(),
        };
        if let Err(e) = self.ctl.call_value(methods::SESSION_KILL, json!(p)).await {
            self.fail(&format!("session.kill: {e}"));
        }
    }

    /// Read output until `needle` appears; panics with the output tail on timeout.
    pub async fn expect(&mut self, needle: &str) {
        self.expect_within(needle, STEP).await
    }

    pub async fn expect_within(&mut self, needle: &str, timeout: Duration) {
        // Search only text not searched yet (plus an overlap for a needle split across
        // chunks): searching all text after every chunk grew quadratically with output and
        // made a long flood's client fall behind (Phase 1 exit checks, soak).
        let found = tokio::time::timeout(timeout, async {
            let (mut from, mut seen_resets, mut seen_len) = (0, self.resets, 0);
            loop {
                if self.resets != seen_resets || self.text.len() < seen_len {
                    from = 0; // cleared by a reset, or trimmed
                    seen_resets = self.resets;
                }
                let mut start = from.min(self.text.len());
                while !self.text.is_char_boundary(start) {
                    start -= 1;
                }
                if self.text[start..].contains(needle) {
                    return true;
                }
                from = self.text.len().saturating_sub(needle.len());
                seen_len = self.text.len();
                if !self.pump().await {
                    return false;
                }
            }
        })
        .await
        .unwrap_or(false);
        if !found {
            self.fail(&format!(
                "timed out after {timeout:?} waiting for {needle:?}"
            ));
        }
    }

    /// Wait for the shell to exit; returns its exit code.
    pub async fn wait_exit(&mut self) -> Option<i64> {
        let done = tokio::time::timeout(STEP, async {
            while self.exited.is_none() {
                if !self.pump().await {
                    break;
                }
            }
        })
        .await;
        if done.is_err() {
            self.fail(&format!("shell did not exit within {STEP:?}"));
        }
        match self.exited {
            Some(code) => code,
            None => self.fail("the feed closed without an exit notification"),
        }
    }

    /// One notification; false when the feed closed.
    /// Read one more notification (bounded by `STEP`).
    pub async fn pump_once(&mut self) {
        let _ = tokio::time::timeout(STEP, self.pump()).await;
    }

    async fn pump(&mut self) -> bool {
        let Ok(n) = self.feed.next_notification().await else {
            return false;
        };
        if n.method == notifications::SESSION_OUTPUT {
            let o: SessionOutput = serde_json::from_value(n.params).expect("output");
            let data = B64.decode(o.data).expect("base64");
            if o.reset {
                self.text.clear();
                self.resets += 1;
            }
            // The daemon answers ConPTY's cursor request and never forwards it.
            assert!(
                !data.windows(4).any(|w| w == b"\x1b[6n"),
                "a cursor request reached the client"
            );
            self.replayed += data.len() as u64;
            let chunk = String::from_utf8_lossy(&data);
            self.text.push_str(&chunk);
            if self.text.len() >= 2 * TEXT_KEEP {
                let mut cut = self.text.len() - TEXT_KEEP;
                while !self.text.is_char_boundary(cut) {
                    cut += 1;
                }
                self.text.drain(..cut);
            }
            self.mirror(&chunk, o.reset);
        } else if n.method == notifications::SESSION_EXITED {
            let e: SessionExited = serde_json::from_value(n.params).expect("exited");
            self.exited = Some(e.exit_code);
        }
        true
    }

    /// Drop both connections without detaching (like a killed client).
    pub fn drop_connections(self) {}

    fn mirror(&self, chunk: &str, reset: bool) {
        let mut t = self
            .transcript
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if reset {
            t.push_str("\n[replay]\n");
        }
        t.push_str(chunk);
        if t.len() > 2 * TRANSCRIPT_KEEP {
            *t = tail_chars(&t, TRANSCRIPT_KEEP);
        }
    }

    /// Fail with this session's output tail and the daemon's log tail.
    fn fail(&self, msg: &str) -> ! {
        panic!(
            "{msg}\n--- shell output tail ---\n{}\n--- daemon log tail ---\n{}",
            tail(&self.text),
            super::daemon_log_tail(&self.home)
        )
    }
}

pub fn tail(s: &str) -> String {
    tail_chars(s, 2000)
}

/// Print a visible "not run" line past the test harness's capture; returns false.
pub fn not_run(test: &str, why: &str) -> bool {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "NOT RUN (environment): {test}: {why}");
    false
}
