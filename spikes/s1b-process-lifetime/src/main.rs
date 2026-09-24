//! Spike S1b (docs/ARCHITECTURE.md §17 Q10): when the host process (≈ `amirid`)
//! is killed, does its child survive, over plain pipes (structured agents:
//! `claude -p`, `codex app-server`) as well as over ConPTY? And *why* does it die:
//! console close (CTRL_CLOSE_EVENT), stdin EOF, broken stdout, or a job object?
//!
//! Throwaway code. The "agent" is this binary in `child` mode, so no tokens are
//! spent. Usage: `cargo run --release` (`host` and `child` subcommands are internal).

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde_json::{json, Map, Value};
use std::any::Any;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::thread::{sleep, spawn};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

const HEARTBEAT: Duration = Duration::from_millis(250);
const SETTLE: Duration = Duration::from_millis(1500);

struct Scenario {
    name: &'static str,
    transport: &'static str, // "pty" | "pipe"
    policy: &'static str,    // "eof-exit": exits when stdin closes | "eof-ignore": keeps running
}

const SCENARIOS: [Scenario; 3] = [
    Scenario { name: "pty_child_ignores_eof", transport: "pty", policy: "eof-ignore" },
    Scenario { name: "pipe_child_exits_on_eof", transport: "pipe", policy: "eof-exit" },
    Scenario { name: "pipe_child_ignores_eof", transport: "pipe", policy: "eof-ignore" },
];

// ---------- child: the fake agent ----------

static LOG: OnceLock<PathBuf> = OnceLock::new();

fn unix_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

fn log(event: &str) {
    let Some(path) = LOG.get() else { return };
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{} {event}", unix_ms());
    }
}

/// 0 CTRL_C, 1 CTRL_BREAK, 2 CTRL_CLOSE, 5 LOGOFF, 6 SHUTDOWN.
unsafe extern "system" fn on_ctrl(kind: u32) -> i32 {
    log(&format!("CTRL_EVENT {kind}"));
    1
}

fn run_child(policy: &str, log_path: &str) {
    let _ = LOG.set(PathBuf::from(log_path));
    log(&format!("START pid={}", std::process::id()));
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
    let exit_on_eof = policy == "eof-exit";
    spawn(move || {
        let mut sink = [0u8; 1024];
        let mut stdin = std::io::stdin();
        loop {
            match stdin.read(&mut sink) {
                Ok(0) => { log("STDIN_EOF"); break; }
                Ok(_) => {}
                Err(e) => { log(&format!("STDIN_ERR {e}")); break; }
            }
        }
        if exit_on_eof {
            log("EXIT_ON_EOF");
            std::process::exit(0);
        }
    });
    let mut stdout_failed = false;
    for tick in 0u64.. {
        let mut out = std::io::stdout();
        if let Err(e) = writeln!(out, "hb {tick}").and_then(|_| out.flush()) {
            if !stdout_failed {
                log(&format!("STDOUT_ERR {e}"));
                stdout_failed = true;
            }
        }
        if tick % 4 == 0 {
            log("ALIVE");
        }
        sleep(HEARTBEAT);
    }
}

// ---------- host: stands in for amirid ----------

type Kept = Box<dyn Any>;

fn spawn_pipe_child(exe: &Path, policy: &str, log_path: &str) -> Result<(u32, Kept), String> {
    let mut child = Command::new(exe)
        .args(["child", policy, log_path])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;
    let mut out = child.stdout.take().ok_or("no child stdout")?;
    spawn(move || { let _ = std::io::copy(&mut out, &mut std::io::sink()); });
    Ok((child.id(), Box::new(child))) // keeps the stdin pipe open until the host dies
}

fn spawn_pty_child(exe: &Path, policy: &str, log_path: &str) -> Result<(u32, Kept), String> {
    let size = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
    let pair = native_pty_system().openpty(size).map_err(|e| format!("openpty: {e}"))?;
    let mut cmd = CommandBuilder::new(exe);
    cmd.args(["child", policy, log_path]);
    let child = pair.slave.spawn_command(cmd).map_err(|e| format!("spawn: {e}"))?;
    drop(pair.slave);
    let pid = child.process_id().ok_or("no child pid")?;
    let mut reader = pair.master.try_clone_reader().map_err(|e| format!("reader: {e}"))?;
    let mut writer = pair.master.take_writer().map_err(|e| format!("writer: {e}"))?;
    spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 { break; }
            if buf[..n].windows(4).any(|w| w == b"\x1b[6n") {
                let _ = writer.write_all(b"\x1b[1;1R"); // answer ConPTY's DSR (S1 finding 5)
            }
        }
    });
    Ok((pid, Box::new((pair.master, child))))
}

fn run_host(transport: &str, policy: &str, log_path: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let (pid, _kept) = match transport {
        "pty" => spawn_pty_child(&exe, policy, log_path)?,
        _ => spawn_pipe_child(&exe, policy, log_path)?,
    };
    sleep(Duration::from_millis(300));
    println!("PID {pid}");
    println!("JOB child={:?} host={:?}", in_job(pid), in_job_self());
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    sleep(Duration::from_secs(600));
    Ok(())
}

// ---------- Win32 queries ----------

unsafe fn job_flag(process: HANDLE) -> Option<bool> {
    let mut result = 0;
    (IsProcessInJob(process, std::ptr::null_mut(), &mut result) != 0).then_some(result != 0)
}

fn in_job_self() -> Option<bool> {
    unsafe { job_flag(GetCurrentProcess()) }
}

fn in_job(pid: u32) -> Option<bool> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let r = job_flag(h);
        CloseHandle(h);
        r
    }
}

/// Prototype of the `amirid` startup refusal (ARCHITECTURE §6.5).
fn is_elevated() -> Option<bool> {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut TOKEN_ELEVATION as *mut _,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        (ok != 0).then_some(elevation.TokenIsElevated != 0)
    }
}

/// Locale-neutral: CSV output quotes the PID; "not found" messages are localized but never contain it.
fn pid_alive(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
        .unwrap_or(false)
}

// ---------- parent: runs scenarios ----------

/// Child log lines as "+ms_since_host_kill EVENT"; heartbeats collapsed to the last one.
fn events_since(log: &str, kill_ms: u128) -> Vec<String> {
    let parsed: Vec<(i128, &str)> = log
        .lines()
        .filter_map(|l| l.split_once(' '))
        .filter_map(|(ms, ev)| ms.parse::<i128>().ok().map(|ms| (ms - kill_ms as i128, ev)))
        .collect();
    let last_alive = parsed.iter().rev().find(|(_, ev)| *ev == "ALIVE").copied();
    parsed
        .iter()
        .filter(|(_, ev)| *ev != "ALIVE")
        .chain(last_alive.iter())
        .map(|(ms, ev)| format!("{ms:+} {}", if *ev == "ALIVE" { "LAST_ALIVE" } else { ev }))
        .collect()
}

fn run_scenario(sc: &Scenario) -> Result<Value, String> {
    let log_path = std::env::temp_dir().join(format!("amiri_s1b_{}.log", sc.name));
    let _ = std::fs::remove_file(&log_path);
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut host = Command::new(exe)
        .args(["host", sc.transport, sc.policy, &log_path.to_string_lossy()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn host: {e}"))?;
    let mut lines = BufReader::new(host.stdout.take().ok_or("no host stdout")?).lines();
    let mut next = || lines.next().and_then(Result::ok).ok_or_else(|| "host output ended".to_string());
    let pid_line = next()?;
    let pid: u32 = pid_line.trim_start_matches("PID ").parse().map_err(|_| format!("bad {pid_line:?}"))?;
    let job_line = next()?;
    sleep(SETTLE);
    let alive_before = pid_alive(pid);
    let kill_ms = unix_ms();
    host.kill().map_err(|e| format!("kill host: {e}"))?; // TerminateProcess ≈ amirid crash
    sleep(Duration::from_secs(3));
    let alive_3s = pid_alive(pid);
    sleep(Duration::from_secs(7));
    let alive_10s = pid_alive(pid);
    let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).output();
    let _ = host.wait();
    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    let _ = std::fs::remove_file(&log_path);
    Ok(json!({
        "transport": sc.transport, "child_policy": sc.policy, "child_pid": pid, "jobs": job_line,
        "alive_before_kill": alive_before, "alive_3s_after": alive_3s, "alive_10s_after": alive_10s,
        "child_events_ms_from_kill": events_since(&log, kill_ms),
    }))
}

/// Commit the spike ran against; `findings.json` excluded because the shell truncates it first.
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
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
    match arg(1) {
        "child" => return run_child(arg(2), arg(3)),
        "host" => {
            if let Err(e) = run_host(arg(2), arg(3), arg(4)) {
                eprintln!("host error: {e}");
                std::process::exit(1);
            }
            return;
        }
        _ => {}
    }
    let mut results = Map::new();
    let mut failed = 0;
    for sc in &SCENARIOS {
        let entry = run_scenario(sc).unwrap_or_else(|e| {
            failed += 1;
            json!({ "error": e })
        });
        eprintln!("{}: {}", sc.name, if entry.get("error").is_some() { "FAILED" } else { "ran" });
        results.insert(sc.name.to_string(), entry);
    }
    let report = json!({
        "spike": "S1b-process-lifetime", "git": git_state(),
        "elevated": is_elevated(), "parent_in_job": in_job_self(), "scenarios": results,
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
    std::process::exit(if failed > 0 { 1 } else { 0 });
}
