//! Spike S1d (docs/ARCHITECTURE.md §9.6, console control): can `amirid` give its
//! shells a working Ctrl+C while surviving a Ctrl+C in its own console?
//!
//! The parent first sets the ignore-Ctrl+C flag on itself, the way a launcher
//! using CREATE_NEW_PROCESS_GROUP does, so every host inherits it deterministically.
//! It then runs a host (stands in for `amirid`) inside an outer ConPTY, one per mode:
//!   - `none`:         leaves the inherited flag alone (the S1 failure)
//!   - `restore-only`: clears the flag, no handler (the reviewer's concern)
//!   - `fixed`:        clears the flag and installs its own handler (the decision)
//! Each host opens an inner ConPTY with `cmd.exe`, runs `ping -t`, sends `\x03`
//! and checks whether the prompt returns. Then the parent sends `\x03` to the
//! host's own console, checks whether the host survived, and closes the outer
//! pseudoconsole to see whether the host shut down cleanly. Events go to a log
//! file (locale-neutral tokens only). No tokens spent.
//!
//! Usage: `cargo run --release` (`host` is internal).

use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};
use serde_json::{json, Map, Value};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread::sleep;
use std::time::{Duration, Instant};
use windows_sys::core::BOOL;
use windows_sys::Win32::System::Console::{
    SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT,
    CTRL_SHUTDOWN_EVENT,
};

const MARK: &str = "AMIRI_Windows_NT";
const ECHO_MARK: &str = "echo AMIRI_%OS%\r";
const STEP_TIMEOUT: Duration = Duration::from_secs(20);
const SIZE: PtySize = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
const MODES: [&str; 3] = ["none", "restore-only", "fixed"];

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

static LOG: OnceLock<PathBuf> = OnceLock::new();

fn log(event: &str) {
    if let Some(p) = LOG.get() {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{event}");
        }
    }
}

struct Pty {
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: SharedWriter,
    rx: mpsc::Receiver<Vec<u8>>,
    buf: String,
}

impl Pty {
    fn spawn(cmd: CommandBuilder) -> Result<Self, String> {
        let pair = native_pty_system().openpty(SIZE).map_err(|e| format!("openpty: {e}"))?;
        let child = pair.slave.spawn_command(cmd).map_err(|e| format!("spawn: {e}"))?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().map_err(|e| format!("reader: {e}"))?;
        let writer: SharedWriter =
            Arc::new(Mutex::new(pair.master.take_writer().map_err(|e| format!("writer: {e}"))?));
        let (tx, rx) = mpsc::channel();
        spawn_reader(reader, Arc::clone(&writer), tx);
        Ok(Self { master: pair.master, child, writer, rx, buf: String::new() })
    }

    fn send(&self, text: &str) -> Result<(), String> {
        let mut w = self.writer.lock().map_err(|_| "writer lock poisoned".to_string())?;
        w.write_all(text.as_bytes()).and_then(|_| w.flush()).map_err(|e| format!("write: {e}"))
    }

    fn expect(&mut self, needle: &str) -> Result<(), String> {
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            if let Some(i) = self.buf.find(needle) {
                self.buf = self.buf.split_off(i + needle.len());
                return Ok(());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!("timeout waiting for {needle:?}"));
            }
            match self.rx.recv_timeout(left) {
                Ok(data) => self.buf.push_str(&String::from_utf8_lossy(&data)),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("pty closed".into()),
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

/// The `fixed` host's handler: survive Ctrl+C/Break, shut down cleanly on close/logoff/shutdown.
extern "system" fn amirid_handler(ctrl: u32) -> BOOL {
    match ctrl {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => {
            log(&format!("HOST_IGNORED_CTRL {ctrl}"));
            1
        }
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
            log(&format!("HOST_CLEAN_SHUTDOWN {ctrl}"));
            std::process::exit(0);
        }
        _ => 0,
    }
}

fn inner_shell_gets_ctrl_c() -> Result<(), String> {
    let mut c = CommandBuilder::new("cmd.exe");
    c.args(["/Q", "/K"]);
    let mut s = Pty::spawn(c)?;
    s.send("ping -t 127.0.0.1\r")?;
    // ping's reply lines are localized, so don't match them; `-t` never ends on its own.
    sleep(Duration::from_millis(2500));
    s.send("\x03")?;
    s.send(ECHO_MARK)?;
    let r = s.expect(MARK);
    let _ = s.child.kill();
    r
}

fn run_host(mode: &str, log_path: &str) -> Result<(), String> {
    let _ = LOG.set(PathBuf::from(log_path));
    log(&format!("HOST_START {mode}"));
    // SAFETY: plain Win32 calls; the handler is a static function with the required ABI.
    unsafe {
        if mode != "none" && SetConsoleCtrlHandler(None, 0) == 0 {
            log("HOST_RESTORE_FAILED");
        }
        if mode == "fixed" && SetConsoleCtrlHandler(Some(amirid_handler), 1) == 0 {
            log("HOST_HANDLER_FAILED");
        }
    }
    match inner_shell_gets_ctrl_c() {
        Ok(()) => log("INNER_CTRL_C ok"),
        Err(e) => log(&format!("INNER_CTRL_C fail {}", e.replace(['\r', '\n'], " "))),
    }
    log("READY");
    sleep(Duration::from_secs(600));
    Ok(())
}

fn read_log(p: &Path) -> Vec<String> {
    std::fs::read_to_string(p).unwrap_or_default().lines().map(str::to_string).collect()
}

fn wait_for_log(p: &Path, token: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if read_log(p).iter().any(|l| l == token) {
            return true;
        }
        sleep(Duration::from_millis(200));
    }
    false
}

fn run_mode(exe: &Path, mode: &str) -> Result<Value, String> {
    let log_path = std::env::temp_dir().join(format!("amiri_s1d_{mode}.log"));
    let _ = std::fs::remove_file(&log_path);
    let mut cmd = CommandBuilder::new(exe);
    cmd.args(["host", mode, &log_path.to_string_lossy()]);
    let mut outer = Pty::spawn(cmd)?;
    let ready = wait_for_log(&log_path, "READY", Duration::from_secs(45));

    outer.send("\x03")?; // Ctrl+C in amirid's own console
    sleep(Duration::from_secs(2));
    let survived_ctrl_c = outer.child.try_wait().map_err(|e| e.to_string())?.is_none();

    // Close the pseudoconsole (≈ user closes the terminal amirid runs in).
    let Pty { master, mut child, writer, rx, .. } = outer;
    drop(writer);
    drop(master);
    drop(rx);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut exit_after_close = None;
    while Instant::now() < deadline {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            exit_after_close = Some(st.exit_code());
            break;
        }
        sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let events = read_log(&log_path);
    let _ = std::fs::remove_file(&log_path);
    let has = |p: &str| events.iter().any(|l| l.starts_with(p));
    Ok(json!({
        "host_ready": ready,
        "inner_shell_interrupted_by_ctrl_c": has("INNER_CTRL_C ok"),
        "host_survived_own_ctrl_c": survived_ctrl_c,
        "host_clean_shutdown_on_close": has("HOST_CLEAN_SHUTDOWN"),
        "host_exit_code_after_close": exit_after_close,
        "events": events,
    }))
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("host") {
        let (Some(mode), Some(log_path)) = (args.get(2), args.get(3)) else {
            eprintln!("usage: host <mode> <log>");
            std::process::exit(2);
        };
        if let Err(e) = run_host(mode, log_path) {
            eprintln!("host error: {e}");
            std::process::exit(1);
        }
        return;
    }
    // Simulate a launcher that disabled Ctrl+C (CREATE_NEW_PROCESS_GROUP) so the
    // starting condition doesn't depend on how this spike itself was started.
    // SAFETY: null handler + TRUE only sets this process's ignore flag.
    let simulated = unsafe { SetConsoleCtrlHandler(None, 1) } != 0;
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("current_exe: {e}");
            std::process::exit(1);
        }
    };
    let mut results = Map::new();
    let mut failed = 0;
    for mode in MODES {
        let entry = run_mode(&exe, mode).unwrap_or_else(|e| {
            failed += 1;
            json!({ "error": e })
        });
        eprintln!("{mode}: {}", if entry.get("error").is_some() { "FAILED" } else { "ran" });
        results.insert(mode.to_string(), entry);
    }
    let report = json!({
        "spike": "S1d-console-control", "git": git_state(),
        "launcher_ignore_flag_simulated": simulated, "modes": results,
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
    std::process::exit(if failed > 0 { 1 } else { 0 });
}
