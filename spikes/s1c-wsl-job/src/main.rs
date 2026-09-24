//! Spike S1c (docs/ARCHITECTURE.md §9.6): does a kill-on-close Job Object reach
//! Linux processes started through `wsl.exe`?
//!
//! A host process (stands in for `amirid`) puts itself in a Job Object with
//! JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, then starts through `wsl.exe`:
//!   - `fg`:   a foreground Linux process (`sleep 607`) tied to its wsl.exe
//!   - `tmux`: a detached tmux session (`amiri_s1c`, running `sleep 613`)
//! The parent kills the host (≈ `amirid` crash; the kernel closes the job and
//! kills every Windows process in it) and checks the Linux side from outside.
//!
//! Usage: `cargo run --release [distro]` (default Ubuntu-22.04; `host` is internal).
//! Touches only the tmux session `amiri_s1c` and the `sleep 607`/`sleep 613` markers.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const DEFAULT_DISTRO: &str = "Ubuntu-22.04";
const TMUX_SESSION: &str = "amiri_s1c";
const FG_MARK: &str = "sleep 607";
const TMUX_MARK: &str = "sleep 613";
const READY_TIMEOUT: Duration = Duration::from_secs(30);

fn wsl(distro: &str, script: &str) -> Command {
    let mut c = Command::new("wsl.exe");
    c.args(["-d", distro, "--", "sh", "-c", script]);
    c
}

/// Linux-side state, queried from outside the job. Prints fixed tokens only (locale-neutral).
fn linux_state(distro: &str) -> Value {
    let script = format!(
        "pgrep -fx '{FG_MARK}' >/dev/null && echo FG; \
         tmux has-session -t {TMUX_SESSION} 2>/dev/null && echo TMUX; \
         pgrep -fx '{TMUX_MARK}' >/dev/null && echo TMUXCHILD; true"
    );
    let out = wsl(distro, &script).stderr(Stdio::null()).output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    let has = |t: &str| text.lines().any(|l| l.trim() == t);
    json!({ "fg_sleep": has("FG"), "tmux_session": has("TMUX"), "tmux_child": has("TMUXCHILD") })
}

fn cleanup(distro: &str) {
    let script = format!(
        "tmux kill-session -t {TMUX_SESSION} 2>/dev/null; pkill -fx '{FG_MARK}'; pkill -fx '{TMUX_MARK}'; true"
    );
    let _ = wsl(distro, &script).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

fn pid_alive(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
        .unwrap_or(false)
}

fn put_self_in_kill_on_close_job() -> Result<(), String> {
    // SAFETY: plain Win32 calls on handles owned by this process; the job handle is
    // deliberately leaked so that only process death closes it.
    unsafe {
        let job: HANDLE = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err("CreateJobObjectW failed".into());
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            return Err("SetInformationJobObject failed".into());
        }
        if AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
            return Err("AssignProcessToJobObject failed".into());
        }
    }
    Ok(())
}

fn in_job_self() -> Option<bool> {
    let mut r = 0;
    // SAFETY: pseudo-handle for the current process; null job = "any job".
    (unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut r) } != 0).then_some(r != 0)
}

/// Internal: join a kill-on-close job, start both WSL scenarios, report, then wait to be killed.
fn run_host(distro: &str) -> Result<(), String> {
    put_self_in_kill_on_close_job()?;
    let fg = wsl(distro, &format!("exec {FG_MARK}"))
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().map_err(|e| format!("spawn fg: {e}"))?;
    let tmux_status = wsl(distro, &format!("tmux new-session -d -s {TMUX_SESSION} '{TMUX_MARK}'"))
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .status().map_err(|e| format!("spawn tmux: {e}"))?;
    println!("FG_WSL_PID {}", fg.id());
    println!("TMUX_START_EXIT {}", tmux_status.code().unwrap_or(-1));
    println!("HOST_IN_JOB {:?}", in_job_self());
    println!("READY");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    sleep(Duration::from_secs(600));
    Ok(())
}

fn wait_linux(distro: &str, want: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let st = linux_state(distro);
        if want(&st) || Instant::now() > deadline {
            return st;
        }
        sleep(Duration::from_millis(250));
    }
}

fn run_parent(distro: &str) -> Result<Value, String> {
    cleanup(distro);
    let pre = linux_state(distro);
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut host = Command::new(exe).args(["host", distro]).stdout(Stdio::piped()).spawn()
        .map_err(|e| format!("spawn host: {e}"))?;
    let mut lines = BufReader::new(host.stdout.take().ok_or("no host stdout")?).lines();
    let mut report = Vec::new();
    for line in lines.by_ref() {
        let line = line.map_err(|e| e.to_string())?;
        if line == "READY" {
            break;
        }
        report.push(line);
    }
    let fg_pid: Option<u32> = report.iter()
        .find_map(|l| l.strip_prefix("FG_WSL_PID ")).and_then(|p| p.parse().ok());
    let before = wait_linux(distro, |s| s["fg_sleep"] == true && s["tmux_session"] == true);
    let fg_wsl_alive_before = fg_pid.map(pid_alive);

    host.kill().map_err(|e| format!("kill host: {e}"))?; // TerminateProcess ≈ amirid crash
    let _ = host.wait();
    sleep(Duration::from_secs(3));
    let after_3s = linux_state(distro);
    let fg_wsl_alive_3s = fg_pid.map(pid_alive);
    sleep(Duration::from_secs(7));
    let after_10s = linux_state(distro);
    cleanup(distro);
    let after_cleanup = linux_state(distro);
    Ok(json!({
        "host_report": report,
        "linux_before_start": pre,
        "linux_before_kill": before,
        "fg_wsl_exe_alive_before_kill": fg_wsl_alive_before,
        "fg_wsl_exe_alive_3s_after": fg_wsl_alive_3s,
        "linux_3s_after_kill": after_3s,
        "linux_10s_after_kill": after_10s,
        "linux_after_cleanup": after_cleanup,
    }))
}

fn is_elevated() -> Option<bool> {
    // SAFETY: token handle is opened and closed here; the struct matches the query class.
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
        let distro = args.get(2).map(String::as_str).unwrap_or(DEFAULT_DISTRO);
        if let Err(e) = run_host(distro) {
            eprintln!("host error: {e}");
            std::process::exit(1);
        }
        return;
    }
    let distro = args.get(1).map(String::as_str).unwrap_or(DEFAULT_DISTRO).to_string();
    let (ok, result) = match run_parent(&distro) {
        Ok(v) => (true, v),
        Err(e) => {
            cleanup(&distro);
            (false, json!({ "error": e }))
        }
    };
    let report = json!({
        "spike": "S1c-wsl-job", "distro": distro, "elevated": is_elevated(),
        "git": git_state(), "ok": ok, "result": result,
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
    std::process::exit(if ok { 0 } else { 1 });
}
