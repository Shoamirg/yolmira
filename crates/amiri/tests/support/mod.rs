//! Test harness: each test gets its own daemon instance and data folder, so tests
//! run in parallel and never touch the user's real daemon.
#![allow(dead_code)]

use amiri_ipc::win::security::is_elevated;
use amiri_ipc::{Client, Instance};
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

pub const AMIRI: &str = env!("CARGO_BIN_EXE_amiri");
pub const AMIRID: &str = env!("CARGO_BIN_EXE_amirid");

pub struct TestEnv {
    pub name: String,
    pub home: tempfile::TempDir,
    /// False inside a job that forbids breakaway (cargo, GitHub's runner): the
    /// detached launcher can't work there, so daemons are started directly.
    pub can_detach: bool,
}

/// Set by the CI workflow: the runner's job forbids breakaway, so detach tests are
/// reported as not run instead of failing.
pub const CI_NO_BREAKAWAY: &str = "AMIRI_CI_NO_BREAKAWAY";

impl TestEnv {
    pub fn new() -> Self {
        let can_detach = amiri_ipc::win::process::current_job_allows_breakaway()
            .ok()
            .flatten()
            != Some(false);
        let name = format!("t{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        Self {
            name,
            home: tempfile::tempdir().expect("temp home"),
            can_detach,
        }
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

    /// `amiri --instance … --home … --json <args>`.
    pub fn amiri_args(&self, args: &[&str]) -> Vec<String> {
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

    pub fn amiri(&self, args: &[&str]) -> Output {
        run_bounded(AMIRI, &self.amiri_args(args), &self.home())
    }

    /// Arguments for `amirid` serving this instance (with `--allow-elevated` on elevated CI runners).
    pub fn amirid_args(&self) -> Vec<String> {
        let mut v = self.instance().daemon_args();
        if elevated() {
            v.push("--allow-elevated".into());
        }
        v
    }

    pub fn start_args(&self) -> Vec<String> {
        let mut v = self.amiri_args(&["daemon", "start"]);
        if elevated() {
            v.push("--allow-elevated".into());
        }
        v
    }

    /// Start the daemon: through the real (detaching) launcher when possible,
    /// otherwise `amirid` directly inside the current job.
    pub fn start(&self) -> Value {
        if !self.can_detach {
            return self.start_in_job();
        }
        let out = run_bounded(AMIRI, &self.start_args(), &self.home());
        assert!(
            out.status.success(),
            "start failed: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        last_json(&out)
    }

    fn start_in_job(&self) -> Value {
        if let Some(pid) = self.running_pid() {
            return serde_json::json!({ "state": "already_running", "pid": pid });
        }
        let opts = amiri_ipc::win::process::SpawnOptions {
            exe: AMIRID.into(),
            args: self.amirid_args(),
            flags: amiri_ipc::win::process::CREATE_NO_WINDOW,
            ..Default::default()
        };
        let p = amiri_ipc::win::process::spawn(&opts).expect("spawn amirid");
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
        let out = self.amiri(&["daemon", "status"]);
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
        Client::connect(&self.instance(), "test")
            .await
            .expect("connect")
    }

    /// All events (starts the daemon first if needed, as `amiri events` would).
    pub fn events(&self) -> Vec<Value> {
        if self.running_pid().is_none() {
            self.start();
        }
        let out = self.amiri(&["events", "--limit", "10000"]);
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
        let run = |args: &[&str]| try_run_bounded(AMIRI, &self.amiri_args(args), &self.home());
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
            if !amiri_ipc::win::process::wait_pid_exit(pid as u32, Duration::from_secs(5))
                .unwrap_or(false)
            {
                let _ = Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/F"])
                    .output();
            }
        }
    }
}

impl TestEnv {
    /// Pid from the latest `daemon.started` event, read straight from the database
    /// (works even when the daemon no longer answers).
    fn pid_from_log(&self) -> Option<u64> {
        let store = amiri_store::Store::open(&self.instance().db_path()).ok()?;
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
    use std::io::Read;
    use std::process::Stdio;
    let mut child = Command::new(exe)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn amiri");
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
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
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
        }
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

pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}
