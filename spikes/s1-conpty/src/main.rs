//! Spike S1 (docs/ARCHITECTURE.md §16.2): ConPTY behaviour through portable-pty.
//!
//! Throwaway code. Runs each check against real ConPTY sessions and prints one
//! JSON object of observations. The `host-kill` / `host-drop` checks settle §D3:
//! what happens to a PTY child when the process owning the pseudoconsole dies.
//!
//! Usage: `cargo run --release` (the `host` subcommand is internal).

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// What `echo AMIRI_%OS%` prints. It never appears in the typed command, so
/// matching it proves the shell executed the line rather than echoing input.
const MARK: &str = "AMIRI_Windows_NT";
const ECHO_MARK: &str = "echo AMIRI_%OS%\r";
const STEP_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_SIZE: PtySize = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
const RESIZED: PtySize = PtySize { rows: 40, cols: 120, pixel_width: 0, pixel_height: 0 };

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;
type CheckResult = Result<Value, String>;

struct Session {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: SharedWriter,
    rx: mpsc::Receiver<Vec<u8>>,
    buf: String,
}

impl Session {
    fn spawn(cmd: CommandBuilder) -> Result<Self, String> {
        let pair = native_pty_system().openpty(DEFAULT_SIZE).map_err(|e| format!("openpty: {e}"))?;
        let child = pair.slave.spawn_command(cmd).map_err(|e| format!("spawn: {e}"))?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().map_err(|e| format!("reader: {e}"))?;
        let writer = pair.master.take_writer().map_err(|e| format!("writer: {e}"))?;
        let writer: SharedWriter = Arc::new(Mutex::new(writer));
        let (tx, rx) = mpsc::channel();
        spawn_reader(reader, Arc::clone(&writer), tx);
        Ok(Self { master: pair.master, child, writer, rx, buf: String::new() })
    }

    fn send(&self, text: &str) -> Result<(), String> {
        let mut w = self.writer.lock().map_err(|_| "writer lock poisoned".to_string())?;
        w.write_all(text.as_bytes()).and_then(|_| w.flush()).map_err(|e| format!("write: {e}"))
    }

    /// Consume output up to and including `needle`, returning what was consumed.
    fn expect(&mut self, needle: &str) -> Result<String, String> {
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            if let Some(i) = self.buf.find(needle) {
                let end = i + needle.len();
                let rest = self.buf.split_off(end);
                return Ok(std::mem::replace(&mut self.buf, rest));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!("timeout waiting for {needle:?}; tail {:?}", tail(&self.buf)));
            }
            match self.rx.recv_timeout(left) {
                Ok(data) => self.buf.push_str(&String::from_utf8_lossy(&data)),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(format!("pty closed waiting for {needle:?}; tail {:?}", tail(&self.buf)))
                }
            }
        }
    }
}

impl Session {
    /// Consume a run of ASCII digits once a non-digit follows it.
    fn expect_digits(&mut self) -> Result<String, String> {
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            let n = self.buf.bytes().take_while(u8::is_ascii_digit).count();
            if n > 0 && n < self.buf.len() {
                let rest = self.buf.split_off(n);
                return Ok(std::mem::replace(&mut self.buf, rest));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!("timeout waiting for digits; tail {:?}", tail(&self.buf)));
            }
            if let Ok(data) = self.rx.recv_timeout(left) {
                self.buf.push_str(&String::from_utf8_lossy(&data));
            }
        }
    }
}

fn spawn_reader(mut reader: Box<dyn Read + Send>, writer: SharedWriter, tx: mpsc::Sender<Vec<u8>>) {
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        while let Ok(n) = reader.read(&mut chunk) {
            if n == 0 {
                break;
            }
            let data = chunk[..n].to_vec();
            // ConPTY can request the cursor position (DSR) and stall until answered.
            if data.windows(4).any(|w| w == b"\x1b[6n") {
                if let Ok(mut w) = writer.lock() {
                    let _ = w.write_all(b"\x1b[1;1R").and_then(|_| w.flush());
                }
            }
            if tx.send(data).is_err() {
                break;
            }
        }
    });
}

fn tail(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    chars[chars.len().saturating_sub(200)..].iter().collect()
}

fn interactive_cmd() -> CommandBuilder {
    let mut c = CommandBuilder::new("cmd.exe");
    c.args(["/Q", "/K"]);
    c
}

fn check_echo() -> CheckResult {
    let mut s = Session::spawn(interactive_cmd())?;
    let started = Instant::now();
    s.send(ECHO_MARK)?;
    s.expect(MARK)?;
    Ok(json!({ "round_trip_ms": started.elapsed().as_millis() }))
}

/// Absolute path: the PTY child's PATH may not resolve `powershell` (see `check_env_path`).
fn powershell_cmd(script: &str) -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    format!("\"{root}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\" -NoProfile -Command \"{script}\"\r")
}

/// Does the PTY child see the same PATH as its parent? portable-pty builds the
/// child environment itself on Windows, so this is not a given.
fn check_env_path() -> CheckResult {
    // Via a file: ConPTY wraps long screen output at the terminal width.
    let out = std::env::temp_dir().join("amiri_s1_child_path.txt");
    let mut s = Session::spawn(interactive_cmd())?;
    s.send(&format!(">\"{}\" echo %PATH%\r", out.display()))?;
    s.send(ECHO_MARK)?;
    s.expect(MARK)?;
    // cmd writes the OEM code page (866 here), hence lossy decoding.
    let bytes = std::fs::read(&out).map_err(|e| format!("read {}: {e}", out.display()))?;
    let _ = std::fs::remove_file(&out);
    let child_path = String::from_utf8_lossy(&bytes).trim().to_string();
    let parent_path = std::env::var("PATH").unwrap_or_default();
    let entries = |p: &str| p.split(';').filter(|e| !e.is_empty()).count();
    Ok(json!({
        "parent_entries": entries(&parent_path),
        "child_entries": entries(&child_path),
        "identical": child_path.eq_ignore_ascii_case(&parent_path),
        "child_has_windows_powershell": child_path.to_lowercase().contains("windowspowershell"),
        "child_has_unexpanded_vars": child_path.contains("%SystemRoot%") || child_path.contains("%USERPROFILE%"),
    }))
}

fn check_ansi() -> CheckResult {
    let mut s = Session::spawn(interactive_cmd())?;
    // Concatenation keeps the literal out of the echoed input line.
    s.send(&powershell_cmd("Write-Host -ForegroundColor Red ('AMIRI_' + 'RED')"))?;
    let seen = s.expect("AMIRI_RED")?;
    let before = tail(&seen);
    // ConPTY re-encodes colours; S1 observed the 256-colour form `38;5;9`.
    let red_sgr = ["\x1b[31m", "\x1b[91m", "\x1b[31;1m", "\x1b[1;31m", "\x1b[38;5;9m", "\x1b[38;5;1m"]
        .iter()
        .any(|c| before.contains(c));
    Ok(json!({ "red_sgr_seen": red_sgr, "any_escape_seen": seen.contains('\x1b'), "raw_tail": before }))
}

fn check_resize() -> CheckResult {
    let mut s = Session::spawn(interactive_cmd())?;
    s.send(ECHO_MARK)?;
    s.expect(MARK)?;
    s.master.resize(RESIZED).map_err(|e| format!("resize: {e}"))?;
    sleep(Duration::from_millis(300));
    // Locale-neutral (`mode con` labels are localized).
    s.send(&powershell_cmd("('AMIRI_' + 'W=' + [Console]::WindowWidth)"))?;
    s.expect("AMIRI_W=")?;
    // Digits only: ConPTY may end the line with a cursor move instead of "\n".
    let cols = s.expect_digits()?;
    Ok(json!({ "requested_cols": RESIZED.cols, "reported_cols": cols, "ok": cols == RESIZED.cols.to_string() }))
}

fn check_ctrl_c() -> CheckResult {
    let mut s = Session::spawn(interactive_cmd())?;
    s.send("ping -t 127.0.0.1\r")?;
    // ping's reply lines are localized, so don't match them; `-t` never ends on its own.
    sleep(Duration::from_millis(2500));
    s.send("\x03")?;
    s.send(ECHO_MARK)?;
    s.expect(MARK).map_err(|e| format!("shell did not return after Ctrl+C: {e}"))?;
    Ok(json!({ "interrupted_and_prompt_returned": true }))
}

fn check_exit_code() -> CheckResult {
    let mut c = CommandBuilder::new("cmd.exe");
    c.args(["/C", "exit 7"]);
    let mut s = Session::spawn(c)?;
    let deadline = Instant::now() + STEP_TIMEOUT;
    loop {
        if let Some(status) = s.child.try_wait().map_err(|e| format!("try_wait: {e}"))? {
            return Ok(json!({ "expected": 7, "observed": status.exit_code() }));
        }
        if Instant::now() > deadline {
            return Err("child did not exit".into());
        }
        sleep(Duration::from_millis(20));
    }
}

/// Internal: own a PTY running a long `ping`, print its PID, then either wait to
/// be killed (`kill`) or close the pseudoconsole ourselves and keep running (`drop`).
fn run_host(mode: &str) -> Result<(), String> {
    let mut c = CommandBuilder::new("ping.exe");
    c.args(["-n", "3600", "127.0.0.1"]);
    let s = Session::spawn(c)?;
    let pid = s.child.process_id().ok_or("no child pid")?;
    println!("PID {pid}");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    if mode == "drop" {
        sleep(Duration::from_secs(1));
        drop(s);
        println!("DROPPED");
        std::io::stdout().flush().map_err(|e| e.to_string())?;
    }
    sleep(Duration::from_secs(600));
    Ok(())
}

fn pid_alive(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
        .unwrap_or(false)
}

fn check_host_death(mode: &str) -> CheckResult {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut host = Command::new(exe)
        .args(["host", mode])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn host: {e}"))?;
    let mut lines = BufReader::new(host.stdout.take().ok_or("no host stdout")?).lines();
    let first = lines.next().ok_or("host printed nothing")?.map_err(|e| e.to_string())?;
    let pid: u32 = first.trim_start_matches("PID ").parse().map_err(|_| format!("bad pid line {first:?}"))?;
    if mode == "drop" {
        lines.next(); // wait for DROPPED
    }
    sleep(Duration::from_secs(1));
    let alive_before = pid_alive(pid);
    if mode == "kill" {
        host.kill().map_err(|e| format!("kill host: {e}"))?; // TerminateProcess ≈ amirid crash
    }
    sleep(Duration::from_secs(3));
    let alive_after_3s = pid_alive(pid);
    sleep(Duration::from_secs(7));
    let alive_after_10s = pid_alive(pid);
    // Cleanup regardless of outcome.
    let _ = host.kill();
    let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).output();
    Ok(json!({ "child_pid": pid, "alive_before": alive_before,
               "alive_3s_after": alive_after_3s, "alive_10s_after": alive_after_10s }))
}

/// With `AMIRI_S1_RESTORE_CTRL_C=1`, clear an inherited "ignore Ctrl+C" flag
/// before spawning PTYs. A parent started with CREATE_NEW_PROCESS_GROUP passes
/// that flag down, and ConPTY children then ignore `\x03`.
fn ctrl_c_setup() -> Value {
    if std::env::var("AMIRI_S1_RESTORE_CTRL_C").as_deref() != Ok("1") {
        return json!({ "restored": false });
    }
    // SAFETY: a null handler with FALSE only resets this process's ignore flag.
    let ok = unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(None, 0) } != 0;
    json!({ "restored": true, "call_succeeded": ok })
}

/// Commit the spike ran against (§13: results from a dirty tree aren't reproducible).
/// `findings.json` is excluded: the shell truncates it before this process starts.
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("host") {
        if let Err(e) = run_host(args.get(2).map(String::as_str).unwrap_or("kill")) {
            eprintln!("host error: {e}");
            std::process::exit(1);
        }
        return;
    }
    let ctrl_c = ctrl_c_setup();
    let checks: [(&str, fn() -> CheckResult); 8] = [
        ("echo_round_trip", check_echo),
        ("env_path", check_env_path),
        ("ansi_passthrough", check_ansi),
        ("resize", check_resize),
        ("ctrl_c_interrupt", check_ctrl_c),
        ("exit_code", check_exit_code),
        ("host_killed", || check_host_death("kill")),
        ("host_closes_pseudoconsole", || check_host_death("drop")),
    ];
    let mut results = Map::new();
    let mut failed = 0;
    for (name, check) in checks {
        let started = Instant::now();
        let entry = match check() {
            Ok(v) => json!({ "ok": true, "observed": v, "ms": started.elapsed().as_millis() }),
            Err(e) => {
                failed += 1;
                json!({ "ok": false, "error": e, "ms": started.elapsed().as_millis() })
            }
        };
        eprintln!("{name}: {}", if entry["ok"] == true { "ran" } else { "FAILED" });
        results.insert(name.to_string(), entry);
    }
    let os = Command::new("cmd.exe").args(["/C", "ver"]).output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let report = json!({ "spike": "S1-conpty", "os": os, "portable_pty": "0.9", "git": git_state(),
                         "ctrl_c_setup": ctrl_c, "checks": results });
    println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
    std::process::exit(if failed > 0 { 1 } else { 0 });
}
