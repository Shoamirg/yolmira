//! `ensure_daemon`: connect to the running daemon, or start one detached
//! (phase1-plan §2). Shared by the CLI and the UI.

use crate::client::{Client, ClientError};
use crate::paths::Instance;
use crate::protocol::{exit, HelloResult};
use crate::win::env::child_env;
use crate::win::process::{
    self, current_process_in_job, SpawnOptions, CREATE_BREAKAWAY_FROM_JOB,
    CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
};
use crate::win::sync::NamedMutex;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use yolmira_core::YolmiraError;

const WHAT: &str = "Start the Yolmira daemon";
const WHERE: &str = "daemon launcher";
const ERROR_ACCESS_DENIED: i32 = 5;
const POLL: Duration = Duration::from_millis(100);
const START_LOCK_WAIT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub instance: Instance,
    pub daemon_exe: PathBuf,
    pub allow_elevated: bool,
    /// How long to wait for a started daemon to answer.
    pub timeout: Duration,
    pub client_name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Launched {
    AlreadyRunning(HelloResult),
    Started(HelloResult),
}

impl Launched {
    pub fn hello(&self) -> &HelloResult {
        match self {
            Self::AlreadyRunning(h) | Self::Started(h) => h,
        }
    }
}

/// Blocking: uses a private single-threaded runtime, so call it from a plain thread
/// (or through [`ensure_daemon`]). The start lock is a mutex, which Windows ties to
/// the acquiring thread; everything here stays on one thread.
pub fn ensure_daemon_blocking(opts: &LaunchOptions) -> Result<Launched, YolmiraError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| YolmiraError::new(WHAT, WHERE, format!("async runtime: {e}")))?;
    let probe = || rt.block_on(probe(opts));
    if let Some(h) = probe()? {
        return Ok(Launched::AlreadyRunning(h));
    }
    let (lock, _) = NamedMutex::open(&opts.instance.start_mutex())
        .map_err(|e| io_error("open start lock", e))?;
    let _guard = lock
        .acquire(START_LOCK_WAIT)
        .map_err(|e| io_error("wait for start lock", e))?
        .ok_or_else(|| {
            YolmiraError::new(WHAT, WHERE, "another launcher held the start lock for 15 s")
                .next_step("Retry; if it keeps happening, run `yolmira doctor`.")
        })?;
    if let Some(h) = probe()? {
        return Ok(Launched::AlreadyRunning(h));
    }
    let child = spawn_daemon(opts)?;
    wait_for_daemon(opts, &child, probe)
}

/// Poll until the new daemon answers, exits, or the timeout passes.
fn wait_for_daemon(
    opts: &LaunchOptions,
    child: &process::Process,
    probe: impl Fn() -> Result<Option<HelloResult>, YolmiraError>,
) -> Result<Launched, YolmiraError> {
    let deadline = Instant::now() + opts.timeout;
    while Instant::now() < deadline {
        if let Some(h) = probe()? {
            return Ok(Launched::Started(h));
        }
        match child.exit_code() {
            // Another daemon holds the instance (e.g. one still shutting down); the new
            // daemon waits for the instance itself, so this means one is really running.
            Ok(Some(exit::ALREADY_RUNNING)) => {
                return wait_for_existing(opts, deadline, &probe);
            }
            Ok(Some(code)) => return Err(daemon_exited(opts, code)),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(pid = child.pid(), error = %e, "could not read the new daemon's exit code")
            }
        }
        std::thread::sleep(POLL);
    }
    Err(YolmiraError::new(
        WHAT,
        WHERE,
        format!(
            "yolmirad (pid {}) did not answer on its pipe within {} s",
            child.pid(),
            opts.timeout.as_secs()
        ),
    )
    .next_step(format!(
        "Check the daemon log in {}",
        opts.instance.log_dir().display()
    ))
    .next_step("Run `yolmira doctor`."))
}

fn wait_for_existing(
    opts: &LaunchOptions,
    deadline: Instant,
    probe: &impl Fn() -> Result<Option<HelloResult>, YolmiraError>,
) -> Result<Launched, YolmiraError> {
    while Instant::now() < deadline {
        if let Some(h) = probe()? {
            return Ok(Launched::AlreadyRunning(h));
        }
        std::thread::sleep(POLL);
    }
    Err(YolmiraError::new(
        WHAT,
        WHERE,
        "another yolmirad holds this instance but does not answer on its pipe",
    )
    .next_step(format!(
        "Check the daemon log in {}",
        opts.instance.log_dir().display()
    ))
    .next_step("If it is stuck, end yolmirad.exe in Task Manager, then retry."))
}

/// Async wrapper: runs [`ensure_daemon_blocking`] on its own thread.
pub async fn ensure_daemon(opts: LaunchOptions) -> Result<Launched, YolmiraError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(ensure_daemon_blocking(&opts));
    });
    rx.await
        .unwrap_or_else(|_| Err(YolmiraError::new(WHAT, WHERE, "launcher thread died")))
}

/// `Ok(None)` only when no daemon is there (or it is mid-start/stop); every other
/// failure is reported, so a misbehaving pipe never leads to a second daemon.
async fn probe(opts: &LaunchOptions) -> Result<Option<HelloResult>, YolmiraError> {
    let connect = "Connect to the Yolmira daemon";
    match Client::connect(&opts.instance, &opts.client_name).await {
        Ok(c) => Ok(Some(c.hello)),
        Err(ClientError::NotRunning | ClientError::Closed) => Ok(None),
        // The pipe exists but the token file isn't written yet.
        Err(ClientError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(ClientError::Io(e)) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            Err(YolmiraError::new(connect, WHERE, format!("the daemon's token file is not readable: {e}"))
                .next_step("If yolmirad runs elevated (--allow-elevated), use Yolmira from an elevated window too,                             or stop it and start it normally."))
        }
        Err(ClientError::Impostor(why)) => Err(YolmiraError::new(connect, WHERE, why)
            .next_step("Another program owns Yolmira's pipe name. Sign out other users or restart Windows,                         then start Yolmira again.")),
        Err(e) => Err(YolmiraError::new(connect, WHERE, e.to_string()).next_step("Run `yolmira doctor`.")),
    }
}

fn spawn_daemon(opts: &LaunchOptions) -> Result<process::Process, YolmiraError> {
    std::fs::create_dir_all(&opts.instance.home)
        .map_err(|e| io_error("create the Yolmira data folder", e))?;
    let mut args = opts.instance.daemon_args();
    if opts.allow_elevated {
        args.push("--allow-elevated".into());
    }
    let spawn_opts = SpawnOptions {
        exe: opts.daemon_exe.clone(),
        args: args.clone(),
        env: Some(child_env()),
        cwd: Some(opts.instance.home.clone()),
        flags: DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB,
    };
    process::spawn(&spawn_opts).map_err(|e| {
        let manual = process::command_line(&opts.daemon_exe.to_string_lossy(), &args);
        if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) && current_process_in_job().unwrap_or(false) {
            // Never escape the job another way (WMI etc.): decisions.md, "Phase 1 plan".
            YolmiraError::new(
                "Start the Yolmira daemon",
                WHERE,
                "this program runs inside a restricted Windows job that doesn't allow starting a background \
                 process outside it (Windows error 5, access denied)",
            )
            .attempted("CreateProcessW with CREATE_BREAKAWAY_FROM_JOB")
            .next_step(format!("Open a new terminal from the Start menu and run: {manual}"))
            .next_step("Then retry; Yolmira connects to the running daemon.")
        } else {
            YolmiraError::new("Start the Yolmira daemon", WHERE, format!("CreateProcessW failed: {e}"))
                .attempted(manual)
                .next_step("Check that yolmirad.exe exists next to yolmira.exe.")
        }
    })
}

fn daemon_exited(opts: &LaunchOptions, code: u32) -> YolmiraError {
    let e = YolmiraError::new("Start the Yolmira daemon", WHERE, match code {
        exit::ELEVATED => "yolmirad refused to run elevated (Administrator); every shell and agent would inherit \
                           admin rights (ARCHITECTURE §6.5)".to_string(),
        exit::USAGE => "yolmirad rejected its arguments".to_string(),
        exit::STARTUP_FAILED => "yolmirad failed during startup".to_string(),
        other => format!("yolmirad exited with code {other} before answering"),
    });
    match code {
        exit::ELEVATED => e.next_step("Start Yolmira from a normal (non-administrator) window."),
        _ => e.next_step(format!(
            "Check the daemon log in {}",
            opts.instance.log_dir().display()
        )),
    }
}

fn io_error(what: &str, e: std::io::Error) -> YolmiraError {
    YolmiraError::new(
        format!("Start the Yolmira daemon: {what}"),
        WHERE,
        e.to_string(),
    )
    .next_step("Run `yolmira doctor`.")
}
