//! `amirid`: the daemon. Startup order matters (phase1-plan §2):
//! DLL search + console control → elevation check → single instance → store → token →
//! pipe → serve.

mod auth;
mod console;
mod handlers;
mod server;
mod shutdown;
mod state;

use amiri_core::event::{actors, kinds};
use amiri_core::NewEvent;
use amiri_ipc::protocol::exit;
use amiri_ipc::win::security::{is_elevated, write_private_file};
use amiri_ipc::win::sync::NamedMutex;
use amiri_ipc::Instance;
use amiri_store::Store;
use clap::Parser;
use serde_json::json;
use state::State;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use windows_sys::Win32::System::LibraryLoader::{
    SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_APPLICATION_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

/// A daemon that is stopping still holds the instance for a moment; a new one waits.
const INSTANCE_WAIT: Duration = Duration::from_secs(5);

#[derive(Parser, Debug)]
#[command(name = "amirid", version, about = "AMIRI daemon")]
struct Args {
    /// Data folder (default %LOCALAPPDATA%\AMIRI).
    #[arg(long)]
    home: Option<PathBuf>,
    /// Named instance (tests); the default instance serves the user.
    #[arg(long)]
    instance: Option<String>,
    /// Also log to stderr (development). The daemon is normally started detached.
    #[arg(long)]
    foreground: bool,
    /// Run even with an Administrator token (ARCHITECTURE §6.5). Logged.
    #[arg(long)]
    allow_elevated: bool,
}

pub fn main() -> ExitCode {
    // SAFETY: process-wide loader setting with valid flags; restricts DLL lookup to
    // System32 and the executable's folder (the working directory is the data folder).
    unsafe {
        SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32 | LOAD_LIBRARY_SEARCH_APPLICATION_DIR)
    };
    // Before anything else: children must not inherit an ignore-Ctrl+C flag (S1d).
    let console_ok = console::install();
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            return code(if e.use_stderr() {
                exit::USAGE
            } else {
                exit::OK
            });
        }
    };
    match start(&args, console_ok) {
        Ok(()) => code(exit::OK),
        Err((exit_code, msg)) => fail(exit_code, &msg),
    }
}

type Failure = (u32, String);

fn start(args: &Args, console_ok: bool) -> Result<(), Failure> {
    let startup = |e: String| (exit::STARTUP_FAILED, e);
    let instance = Instance::resolve(args.instance.clone(), args.home.clone())
        .map_err(|e| (exit::USAGE, format!("instance: {e}")))?;
    let elevated = is_elevated().map_err(|e| startup(format!("elevation check: {e}")))?;
    elevation_decision(elevated, args.allow_elevated)
        .map_err(|m| (exit::ELEVATED, m.to_string()))?;
    for dir in [
        instance.home.clone(),
        instance.home.join("run"),
        instance.log_dir(),
    ] {
        std::fs::create_dir_all(&dir)
            .map_err(|e| startup(format!("create {}: {e}", dir.display())))?;
    }
    let _log_guard = init_logging(&instance, args.foreground);
    tracing::info!(console_ok, elevated, instance = ?instance.name, "amirid starting");

    let (single, _) = NamedMutex::open(&instance.daemon_mutex())
        .map_err(|e| startup(format!("instance lock: {e}")))?;
    // Held on this thread until `start` returns.
    let _running = single
        .acquire(INSTANCE_WAIT)
        .map_err(|e| startup(format!("instance lock: {e}")))?
        .ok_or((
            exit::ALREADY_RUNNING,
            "another amirid already serves this instance".to_string(),
        ))?;
    run(instance, elevated).map_err(startup)
}

/// ARCHITECTURE §6.5: refuse an elevated token unless explicitly allowed.
pub fn elevation_decision(elevated: bool, allow_elevated: bool) -> Result<(), &'static str> {
    if elevated && !allow_elevated {
        Err(
            "refusing to run elevated: every shell and agent would inherit Administrator rights \
             (start AMIRI from a normal window, or pass --allow-elevated)",
        )
    } else {
        Ok(())
    }
}

fn run(instance: Instance, elevated: bool) -> Result<(), String> {
    let store = Store::open(&instance.db_path())
        .map_err(|e| format!("open {}: {e}", instance.db_path().display()))?;
    let migration = store.migration_report().clone();
    // A fresh token per daemon start. Only this user can read it; when elevated, only
    // elevated processes of this user (High mandatory label).
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    write_private_file(
        &instance.token_path(),
        &instance.sid,
        elevated,
        token.as_bytes(),
    )
    .map_err(|e| format!("write token: {e}"))?;
    let state = Arc::new(State::new(store, instance, token, elevated));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    let started = json!({
        "pid": std::process::id(),
        "version": env!("CARGO_PKG_VERSION"),
        "instance": state.instance.name,
        "elevated": elevated,
        "schema_from": migration.from_version,
        "schema_to": migration.to_version,
        "backup": migration.backup.map(|p| p.to_string_lossy().into_owned()),
    });
    rt.block_on(serve_until_stopped(state, started))?;
    // Don't wait for connection tasks blocked on client reads.
    rt.shutdown_timeout(Duration::from_millis(500));
    Ok(())
}

async fn serve_until_stopped(state: Arc<State>, started: serde_json::Value) -> Result<(), String> {
    let (stop_tx, mut stop_rx) = tokio::sync::mpsc::unbounded_channel();
    shutdown::install(stop_tx);
    let mut factory =
        server::PipeFactory::new(&state).map_err(|e| format!("pipe security: {e}"))?;
    let first = factory
        .create(true)
        .map_err(|e| format!("create pipe {}: {e}", state.instance.pipe_name()))?;
    state
        .record(NewEvent::new(kinds::DAEMON_STARTED, actors::AMIRI, started))
        .map_err(|e| e.to_string())?;
    console::spawn_session_end_listener();
    let server = tokio::spawn(server::serve(Arc::clone(&state), factory, first));
    let reason = stop_rx
        .recv()
        .await
        .unwrap_or_else(|| "channel_closed".into());
    server.abort();
    if let Err(e) = state.record(NewEvent::new(
        kinds::DAEMON_STOPPED,
        actors::AMIRI,
        json!({ "reason": reason }),
    )) {
        tracing::error!(error = %e, "could not record daemon.stopped");
    }
    tracing::info!(reason, "amirid stopped");
    shutdown::mark_done();
    Ok(())
}

fn init_logging(
    instance: &Instance,
    foreground: bool,
) -> tracing_appender::non_blocking::WorkerGuard {
    use tracing_subscriber::prelude::*;
    let file = tracing_appender::rolling::daily(instance.log_dir(), "amirid.log");
    let (writer, guard) = tracing_appender::non_blocking(file);
    let json = tracing_subscriber::fmt::layer().json().with_writer(writer);
    let stderr = foreground.then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    let filter =
        tracing_subscriber::EnvFilter::try_from_env("AMIRI_LOG").unwrap_or_else(|_| "info".into());
    if let Err(e) = tracing_subscriber::registry()
        .with(filter)
        .with(json)
        .with(stderr)
        .try_init()
    {
        eprintln!("amirid: logging not initialised: {e}");
    }
    guard
}

fn fail(exit_code: u32, msg: &str) -> ExitCode {
    tracing::error!(exit_code, msg, "amirid exiting");
    eprintln!("amirid: {msg}");
    code(exit_code)
}

fn code(c: u32) -> ExitCode {
    ExitCode::from(u8::try_from(c).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elevation_check() {
        assert!(elevation_decision(true, false).is_err());
        assert!(elevation_decision(true, true).is_ok());
        assert!(elevation_decision(false, false).is_ok());
    }
}
