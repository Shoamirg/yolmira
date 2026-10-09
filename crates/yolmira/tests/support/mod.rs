//! Test harness: each test gets its own daemon instance and data folder, so tests
//! run in parallel and never touch the user's real daemon.
#![allow(dead_code)]

pub mod shell;

use serde_json::Value;
use std::future::Future;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use yolmira_ipc::win::security::is_elevated;
use yolmira_ipc::{Client, Instance};

pub const YOLMIRA: &str = env!("CARGO_BIN_EXE_yolmira");
pub const YOLMIRAD: &str = env!("CARGO_BIN_EXE_yolmirad");

pub struct TestEnv {
    pub name: String,
    pub home: tempfile::TempDir,
    /// False inside a job that forbids breakaway (cargo, GitHub's runner): the
    /// detached launcher can't work there, so daemons are started directly.
    pub can_detach: bool,
    /// The latest shell output of this test's sessions (see `shell::Shell`), for
    /// failure reports when the test as a whole runs out of time.
    pub transcript: Arc<Mutex<String>>,
}

/// Opening a pipe connection and its hello; `Client::connect` itself has no limit.
pub const CONNECT_LIMIT: Duration = Duration::from_secs(30);
/// How much of the transcript a failure report shows.
const TRANSCRIPT_TAIL: usize = 4000;

/// Tests outside the policy suite kill shells and stop their daemon freely; the
/// built-in defaults would make both ASK and hold the test (phase1-plan Slice D).
pub const TEST_POLICIES: &str =
    "[global]\n\"session.kill\" = \"allow\"\n\"daemon.stop\" = \"allow\"\n";

/// Set by the CI workflow: the runner's job forbids breakaway, so detach tests are
/// reported as not run instead of failing.
pub const CI_NO_BREAKAWAY: &str = "YOLMIRA_CI_NO_BREAKAWAY";

impl TestEnv {
    pub fn new() -> Self {
        let can_detach = yolmira_ipc::win::process::current_job_allows_breakaway()
            .ok()
            .flatten()
            != Some(false);
        let name = format!("t{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        let env = Self {
            name,
            home: tempfile::tempdir().expect("temp home"),
            can_detach,
            transcript: Arc::default(),
        };
        env.write_policies(TEST_POLICIES);
        env
    }

    /// Replace this instance's `policies.toml` (its own, under its home: never the
    /// user's). The daemon reads it on every check.
    pub fn write_policies(&self, toml: &str) {
        let path = self.instance().policies_path();
        std::fs::create_dir_all(path.parent().expect("config folder")).expect("config folder");
        std::fs::write(&path, toml).expect("write policies.toml");
    }

    /// Make the daemon treat this test binary as the Yolmira UI (debug builds only, via
    /// a file in the instance's own config folder). Call before `start`.
    pub fn trust_this_test_as_ui(&self) {
        let me = std::env::current_exe().expect("test exe");
        let config = self.instance().config_dir();
        std::fs::create_dir_all(&config).expect("config folder");
        std::fs::write(
            config.join("debug-trusted-ui.txt"),
            me.to_string_lossy().as_bytes(),
        )
        .expect("trusted UI file");
    }

    /// For tests of detaching itself. Returns false (test must return) only on CI with
    /// the explicit flag; anywhere else a restrictive job is a hard failure.
    pub fn require_detach(&self, test: &str) -> bool {
        if self.can_detach {
            return true;
        }
        if std::env::var(CI_NO_BREAKAWAY).as_deref() == Ok("1") {
            // Not eprintln!: the test harness captures that for passing tests, which would
            // hide exactly this report. A direct write to the handle isn't captured.
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "NOT RUN ON CI (needs breakaway): {test}");
            return false;
        }
        panic!(
            "{test} needs to run outside a job that forbids breakaway (cargo's does):              use scripts/test.ps1, which runs the test binaries directly"
        );
    }

    pub fn home(&self) -> PathBuf {
        self.home.path().to_path_buf()
    }

    pub fn instance(&self) -> Instance {
        Instance::resolve(Some(self.name.clone()), Some(self.home())).expect("instance")
    }

    /// `yolmira --instance … --home … --json <args>`.
    pub fn yolmira_args(&self, args: &[&str]) -> Vec<String> {
        let mut v = vec![
            "--instance".into(),
            self.name.clone(),
            "--home".into(),
            self.home().to_string_lossy().into_owned(),
            "--json".into(),
        ];
        v.extend(args.iter().map(|s| s.to_string()));
        v
    }

    pub fn yolmira(&self, args: &[&str]) -> Output {
        run_bounded(YOLMIRA, &self.yolmira_args(args), &self.home())
    }

    /// Arguments for `yolmirad` serving this instance (with `--allow-elevated` on elevated CI runners).
    pub fn yolmirad_args(&self) -> Vec<String> {
        let mut v = self.instance().daemon_args();
        if elevated() {
            v.push("--allow-elevated".into());
        }
        v
    }

    pub fn start_args(&self) -> Vec<String> {
        let mut v = self.yolmira_args(&["daemon", "start"]);
        if elevated() {
            v.push("--allow-elevated".into());
        }
        v
    }

    /// Start the daemon: through the real (detaching) launcher when possible,
    /// otherwise `yolmirad` directly inside the current job.
    pub fn start(&self) -> Value {
        self.start_with_env(&[])
    }

    /// Start with extra variables in the *launcher's* environment (they must not reach
    /// the daemon's shells: the child environment is an allowlist).
    pub fn start_with_env(&self, extra: &[(&str, &str)]) -> Value {
        if !self.can_detach {
            return self.start_in_job(extra);
        }
        let out = run_bounded_env(YOLMIRA, &self.start_args(), &self.home(), extra);
        assert!(
            out.status.success(),
            "start failed: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        last_json(&out)
    }

    fn start_in_job(&self, extra: &[(&str, &str)]) -> Value {
        if let Some(pid) = self.running_pid() {
            return serde_json::json!({ "state": "already_running", "pid": pid });
        }
        let opts = yolmira_ipc::win::process::SpawnOptions {
            exe: YOLMIRAD.into(),
            args: self.yolmirad_args(),
            env: (!extra.is_empty()).then(|| {
                let mut vars: Vec<(String, String)> = std::env::vars().collect();
                vars.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
                vars
            }),
            flags: yolmira_ipc::win::process::CREATE_NO_WINDOW,
            ..Default::default()
        };
        let p = yolmira_ipc::win::process::spawn(&opts).expect("spawn yolmirad");
        let mut pid = None;
        self.wait_until("in-job daemon answers", Duration::from_secs(20), || {
            pid = self.running_pid();
            pid.is_some()
        });
        assert_eq!(
            pid,
            Some(u64::from(p.pid())),
            "the answering daemon is the one started"
        );
        serde_json::json!({ "state": "started", "pid": pid })
    }

    /// Daemon pid if it answers.
    pub fn running_pid(&self) -> Option<u64> {
        let out = self.yolmira(&["daemon", "status"]);
        let v = last_json(&out);
        (v["state"] == "running")
            .then(|| v["status"]["pid"].as_u64())
            .flatten()
    }

    pub fn wait_until(&self, what: &str, timeout: Duration, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("timed out after {timeout:?} waiting for {what}");
    }

    pub async fn client(&self) -> Client {
        match tokio::time::timeout(CONNECT_LIMIT, Client::connect(&self.instance(), "test")).await {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => panic!("{}", self.report(&format!("connect: {e}"))),
            Err(_) => panic!(
                "{}",
                self.report(&format!("connect: no answer within {CONNECT_LIMIT:?}"))
            ),
        }
    }

    /// A failure message with what a person would look at first: the shell output
    /// and the daemon's log.
    pub fn report(&self, msg: &str) -> String {
        let transcript = self
            .transcript
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        format!(
            "{msg}\n--- shell output tail ---\n{}\n--- daemon log tail ---\n{}",
            tail_chars(&transcript, TRANSCRIPT_TAIL),
            daemon_log_tail(&self.home())
        )
    }

    /// Run a test body with an overall deadline. Every step inside is bounded too;
    /// this catches what a step limit can't (many slow steps, a wait missed in review).
    pub fn block_on<F: Future>(&self, limit: Duration, f: F) -> F::Output {
        runtime()
            .block_on(async { tokio::time::timeout(limit, f).await })
            .unwrap_or_else(|_| {
                panic!(
                    "{}",
                    self.report(&format!("the test did not finish within {limit:?}"))
                )
            })
    }

    /// All events (starts the daemon first if needed, as `yolmira events` would).
    pub fn events(&self) -> Vec<Value> {
        if self.running_pid().is_none() {
            self.start();
        }
        let out = self.yolmira(&["events", "--limit", "10000"]);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v.get("seq").is_some())
            .collect()
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        // Never panic here: a panic during an already-failing test aborts the binary.
        let run = |args: &[&str]| try_run_bounded(YOLMIRA, &self.yolmira_args(args), &self.home());
        let pid = match run(&["daemon", "status"]) {
            Ok(out) => last_json(&out)["status"]["pid"].as_u64(),
            Err(msg) => {
                eprintln!("TestEnv cleanup: status stalled: {msg}");
                self.pid_from_log()
            }
        };
        if let Err(msg) = run(&["daemon", "stop"]) {
            eprintln!("TestEnv cleanup: stop stalled: {msg}");
        }
        if let Some(pid) = pid {
            if !yolmira_ipc::win::process::wait_pid_exit(pid as u32, Duration::from_secs(5))
                .unwrap_or(false)
            {
                let _ = Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/F"])
                    .output();
            }
        }
        // A failing test keeps its home (daemon log, database) for diagnosis.
        if std::thread::panicking() {
            if let Ok(spare) = tempfile::tempdir() {
                let kept = std::mem::replace(&mut self.home, spare).keep();
                eprintln!("TestEnv: kept the failing test's home: {}", kept.display());
            }
        }
    }
}

impl TestEnv {
    /// Pid from the latest `daemon.started` event, read straight from the database
    /// (works even when the daemon no longer answers).
    fn pid_from_log(&self) -> Option<u64> {
        let store = yolmira_store::Store::open(&self.instance().db_path()).ok()?;
        let started = store.list(0, u32::MAX, Some("daemon.started")).ok()?;
        started.last()?.payload["pid"].as_u64()
    }
}

/// Hard limit for any single CLI call a test makes. A hang becomes a failure that
/// names the command and shows the daemon's log, instead of a silent stall.
pub const COMMAND_LIMIT: Duration = Duration::from_secs(60);

pub fn run_bounded(exe: &str, args: &[String], home: &std::path::Path) -> Output {
    try_run_bounded(exe, args, home).unwrap_or_else(|msg| panic!("{msg}"))
}

/// Like `run_bounded`, but a timeout is an `Err` (for cleanup code that must not panic).
pub fn try_run_bounded(
    exe: &str,
    args: &[String],
    home: &std::path::Path,
) -> Result<Output, String> {
    try_run_bounded_env(exe, args, home, &[])
}

pub fn run_bounded_env(
    exe: &str,
    args: &[String],
    home: &std::path::Path,
    extra: &[(&str, &str)],
) -> Output {
    try_run_bounded_env(exe, args, home, extra).unwrap_or_else(|msg| panic!("{msg}"))
}

pub fn try_run_bounded_env(
    exe: &str,
    args: &[String],
    home: &std::path::Path,
    extra: &[(&str, &str)],
) -> Result<Output, String> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = Command::new(exe)
        .args(args)
        .envs(extra.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn yolmira");
    let mut stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");
    let out_t = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stdout.read_to_end(&mut b);
        b
    });
    let err_t = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stderr.read_to_end(&mut b);
        b
    });
    let started = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait().expect("try_wait") {
            break st;
        }
        if started.elapsed() > COMMAND_LIMIT {
            let _ = child.kill();
            return Err(format!(
                "`{exe} {}` did not finish within {COMMAND_LIMIT:?}
--- daemon log tail ---
{}",
                args.join(" "),
                daemon_log_tail(home)
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ok(Output {
        status,
        stdout: out_t.join().unwrap_or_default(),
        stderr: err_t.join().unwrap_or_default(),
    })
}

/// Last lines of the instance's daemon logs (for failure messages).
pub fn daemon_log_tail(home: &std::path::Path) -> String {
    let Ok(dir) = std::fs::read_dir(home.join("logs")) else {
        return "(no log folder)".into();
    };
    let mut text = String::new();
    for entry in dir.flatten() {
        text.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
    }
    let lines: Vec<_> = text.lines().collect();
    lines[lines.len().saturating_sub(40)..].join(
        "
",
    )
}

pub fn elevated() -> bool {
    is_elevated().unwrap_or(false)
}

/// The last JSON object printed on stdout.
pub fn last_json(out: &Output) -> Value {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str(l).ok())
        .unwrap_or(Value::Null)
}

/// A process in a pseudoconsole. The reader thread answers ConPTY's cursor-position
/// request (DSR, `ESC[6n`), without which the child stalls (S1 finding 5).
pub struct Pty {
    pub master: Box<dyn portable_pty::MasterPty + Send>,
    pub child: Box<dyn portable_pty::Child + Send + Sync>,
    pub writer: std::sync::Arc<std::sync::Mutex<Box<dyn std::io::Write + Send>>>,
    /// Everything the child wrote (lossy UTF-8).
    pub output: std::sync::Arc<std::sync::Mutex<String>>,
}

impl Pty {
    pub fn spawn(exe: &str, args: &[String]) -> Self {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};
        use std::io::{Read, Write};
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = native_pty_system().openpty(size).expect("openpty");
        let mut cmd = CommandBuilder::new(exe);
        cmd.args(args);
        let child = pair.slave.spawn_command(cmd).expect("spawn in pty");
        drop(pair.slave);
        let writer = std::sync::Arc::new(std::sync::Mutex::new(
            pair.master.take_writer().expect("writer"),
        ));
        let mut reader = pair.master.try_clone_reader().expect("reader");
        let answer = std::sync::Arc::clone(&writer);
        let output = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = std::sync::Arc::clone(&output);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if let Ok(mut o) = sink.lock() {
                    o.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
                if buf[..n].windows(4).any(|w| w == b"\x1b[6n") {
                    if let Ok(mut w) = answer.lock() {
                        let _ = w.write_all(b"\x1b[1;1R").and_then(|_| w.flush());
                    }
                }
            }
        });
        Self {
            master: pair.master,
            child,
            writer,
            output,
        }
    }

    pub fn text(&self) -> String {
        self.output.lock().map(|o| o.clone()).unwrap_or_default()
    }

    /// Wait until the output contains `needle`; panics with the tail on timeout.
    pub fn wait_for(&self, needle: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.text().contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "timed out waiting for {needle:?}; output tail: {:?}",
            shell::tail(&self.text())
        );
    }

    pub fn send(&self, bytes: &[u8]) {
        use std::io::Write;
        let mut w = self.writer.lock().expect("writer lock");
        w.write_all(bytes)
            .and_then(|_| w.flush())
            .expect("pty write");
    }

    /// Close the pseudoconsole (like closing the terminal window); returns the child.
    pub fn close(self) -> Box<dyn portable_pty::Child + Send + Sync> {
        drop(self.writer);
        drop(self.master);
        self.child
    }
}

/// The last `n` characters (not bytes: output is often Cyrillic).
pub fn tail_chars(s: &str, n: usize) -> String {
    let c: Vec<char> = s.chars().collect();
    c[c.len().saturating_sub(n)..].iter().collect()
}

pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}
