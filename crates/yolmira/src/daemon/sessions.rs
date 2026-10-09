//! Live terminal sessions (phase1-plan Slice B). Each owns a ConPTY shell, a
//! scrollback ring and a broadcast of output. Ring push and broadcast happen under
//! one lock, so an attaching client's snapshot + subscription has no gap or overlap.
//! The PTY layer answers ConPTY's cursor request itself (yolmira-exec `pty`).
//! Everything here blocks (process creation, pipe writes, SQLite): callers on the
//! async runtime use `spawn_blocking`.

use super::state::State;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, watch};
use yolmira_core::event::{actors, kinds};
use yolmira_core::{NewEvent, YolmiraError};
use yolmira_exec::{resolve, PtyEvent, PtySession, PtySpec, Ring, ShellKind};
use yolmira_ipc::protocol::SessionInfo;
use yolmira_ipc::win::env::child_env;

/// Output chunks (up to 16 KiB each) a session's broadcast keeps for its forwarders. The
/// channel keeps its last chunks even once read, so this is held per session: 4 MiB.
/// Forwarders never wait on clients (`outbox`), so they lag only when starved.
const OUTPUT_CHANNEL: usize = 256;
/// Live shells per daemon (each is a ConPTY, a conhost, two threads and a 2 MiB ring).
pub const MAX_LIVE_SESSIONS: usize = 32;
const WHERE: &str = "yolmirad sessions";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

type Chunk = Arc<Vec<u8>>;

pub struct Output {
    inner: Mutex<(Ring, u32)>,
    tx: broadcast::Sender<Chunk>,
}

impl Output {
    fn new() -> Self {
        let ring = Ring::new(yolmira_exec::ring::DEFAULT_CAPACITY);
        Self {
            inner: Mutex::new((ring, 0)),
            tx: broadcast::channel(OUTPUT_CHANNEL).0,
        }
    }

    /// Count a client; returns the scrollback's current size. Its forwarder then takes
    /// the snapshot (`resync`) when its outbox has room for it.
    pub fn attach(&self) -> usize {
        let mut inner = lock(&self.inner);
        inner.1 += 1;
        inner.0.len()
    }

    /// Snapshot and subscribe, atomically: output after the snapshot arrives through the
    /// new subscription, so nothing is missed or repeated.
    pub fn resync(&self) -> (Vec<u8>, broadcast::Receiver<Chunk>) {
        let inner = lock(&self.inner);
        (inner.0.snapshot(), self.tx.subscribe())
    }

    pub fn detach(&self) {
        let mut inner = lock(&self.inner);
        inner.1 = inner.1.saturating_sub(1);
    }

    pub fn attached(&self) -> u32 {
        lock(&self.inner).1
    }

    fn push(&self, data: Vec<u8>) {
        let mut inner = lock(&self.inner);
        inner.0.push(&data);
        let _ = self.tx.send(Arc::new(data));
    }
}

pub struct LiveSession {
    pub id: String,
    pub shell: String,
    pub cwd: String,
    /// The registered project it was started in, if any (its rules apply to it).
    pub project: Option<String>,
    pub pty: PtySession,
    pub output: Arc<Output>,
    size: Mutex<(u16, u16)>,
    /// `Some(code)` once the shell exited (`code` may be unknown).
    pub exit: watch::Receiver<Option<Option<i64>>>,
}

impl LiveSession {
    pub fn info(&self) -> SessionInfo {
        let (cols, rows) = *lock(&self.size);
        let exited = *self.exit.borrow();
        SessionInfo {
            id: self.id.clone(),
            shell: self.shell.clone(),
            cwd: self.cwd.clone(),
            pid: self.pty.pid(),
            status: if exited.is_some() {
                "EXITED"
            } else {
                "RUNNING"
            }
            .into(),
            exit_code: exited.flatten(),
            attached: self.output.attached(),
            cols,
            rows,
            project: self.project.clone(),
        }
    }

    pub fn has_exited(&self) -> bool {
        self.exit.borrow().is_some()
    }

    pub fn set_size(&self, cols: u16, rows: u16) {
        *lock(&self.size) = (cols, rows);
    }
}

#[derive(Default)]
pub struct Sessions {
    live: Mutex<HashMap<String, Arc<LiveSession>>>,
}

impl Sessions {
    pub fn get(&self, id: &str) -> Option<Arc<LiveSession>> {
        lock(&self.live).get(id).cloned()
    }

    pub fn all(&self) -> Vec<Arc<LiveSession>> {
        lock(&self.live).values().cloned().collect()
    }

    fn remove(&self, id: &str) {
        lock(&self.live).remove(id);
    }

    /// End every shell (daemon shutdown) and wait up to `timeout` for their exits.
    pub fn kill_all(&self, timeout: Duration) {
        let all = self.all();
        for s in &all {
            if let Err(e) = s.pty.kill() {
                tracing::warn!(session = s.id, error = %e, "kill on shutdown failed");
            }
        }
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline && all.iter().any(|s| !s.has_exited()) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

pub struct CreateRequest {
    pub shell: ShellKind,
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub client: String,
    pub project: Option<String>,
}

/// With a project and no explicit folder, the shell starts in the project's folder.
fn resolve_project(state: &State, req: CreateRequest) -> Result<CreateRequest, YolmiraError> {
    let Some(name) = req.project.as_deref() else {
        return Ok(req);
    };
    let project = state.with_store(|s| s.project(name))?.ok_or_else(|| {
        YolmiraError::new("Start a shell", WHERE, format!("no project named {name:?}"))
            .next_step("List projects with `yolmira project list`; add one with `yolmira project add <name> <path>`.")
    })?;
    Ok(CreateRequest {
        cwd: req.cwd.or(Some(project.path)),
        ..req
    })
}

/// Start a shell. `session.created` is recorded first, so a shell that exits at once
/// can't record its exit before its creation.
pub fn create(state: &Arc<State>, req: CreateRequest) -> Result<Arc<LiveSession>, YolmiraError> {
    if state
        .sessions
        .all()
        .iter()
        .filter(|s| !s.has_exited())
        .count()
        >= MAX_LIVE_SESSIONS
    {
        return Err(YolmiraError::new(
            "Start a shell",
            WHERE,
            format!("{MAX_LIVE_SESSIONS} shells are already running"),
        )
        .next_step("End some with `yolmira shell kill <id>`."));
    }
    let req = resolve_project(state, req)?;
    let env = child_env();
    let spec = shell_spec(state, &req, env)?;
    let id = uuid::Uuid::now_v7().to_string();
    let shell = req.shell.label().to_string();
    let cwd = spec.cwd.to_string_lossy().into_owned();
    let payload = json!({ "shell_kind": shell, "cwd": cwd, "program": spec.program, "cols": req.cols, "rows": req.rows, "project": req.project });
    state.record(
        NewEvent::new(
            kinds::SESSION_CREATED,
            format!("client:{}", req.client),
            payload,
        )
        .for_session(&id),
    )?;
    let output = Arc::new(Output::new());
    let (exit_tx, exit_rx) = watch::channel(None);
    let sink = event_sink(Arc::clone(state), id.clone(), Arc::clone(&output), exit_tx);
    let pty = PtySession::spawn(&spec, sink).map_err(|e| {
        record_exit(state, &id, None, "spawn_failed");
        YolmiraError::new("Start a shell", WHERE, e.to_string()).next_step("Run `yolmira doctor`.")
    })?;
    let session = Arc::new(LiveSession {
        id: id.clone(),
        shell,
        cwd,
        project: req.project.clone(),
        pty,
        output,
        size: Mutex::new((req.cols, req.rows)),
        exit: exit_rx,
    });
    lock(&state.sessions.live).insert(id.clone(), Arc::clone(&session));
    // The shell may have exited before it was in the map (the sink couldn't remove it).
    if session.has_exited() {
        state.sessions.remove(&id);
    }
    Ok(session)
}

/// Program, arguments, validated working folder, environment.
fn shell_spec(
    state: &State,
    req: &CreateRequest,
    env: Vec<(String, String)>,
) -> Result<PtySpec, YolmiraError> {
    let var = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let windows_dir = var("SystemRoot").ok_or_else(|| {
        YolmiraError::new("Start a shell", WHERE, "the Windows directory is unknown")
            .next_step("Run `yolmira doctor`.")
    })?;
    let command = resolve(&req.shell, &env, &windows_dir, state.elevated).map_err(|e| {
        YolmiraError::new("Start a shell", WHERE, e.to_string())
            .next_step("Choose another shell kind.")
    })?;
    let cwd = match &req.cwd {
        Some(c) => validate_cwd(c)?,
        None => var("USERPROFILE").unwrap_or_else(|| windows_dir.clone()),
    };
    Ok(PtySpec {
        program: command.program,
        args: command.args,
        cwd: cwd.into(),
        env,
        cols: req.cols,
        rows: req.rows,
    })
}

/// An existing folder, as an absolute drive-letter (`C:\…`) or UNC (`\\server\share\…`)
/// path. Relative paths would resolve against the daemon's folder; device paths
/// (`\\?\`, `\\.\`) are not folders a user means to work in.
pub fn validate_cwd(cwd: &str) -> Result<String, YolmiraError> {
    let bytes = cwd.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    let unc = cwd.starts_with(r"\\") && !cwd.starts_with(r"\\?\") && !cwd.starts_with(r"\\.\");
    let invalid = |why: &str| {
        YolmiraError::new(
            "Start a shell",
            WHERE,
            format!("working folder {cwd:?} {why}"),
        )
        .next_step(r"Pass an existing absolute folder, e.g. C:\Users\you\project.")
    };
    if !(drive || unc) {
        return Err(invalid("is not an absolute drive or network path"));
    }
    if !Path::new(cwd).is_dir() {
        return Err(invalid("does not exist or is not a folder"));
    }
    Ok(cwd.to_string())
}

fn event_sink(
    state: Arc<State>,
    id: String,
    output: Arc<Output>,
    exit_tx: watch::Sender<Option<Option<i64>>>,
) -> yolmira_exec::pty::EventSink {
    Arc::new(move |event| match event {
        PtyEvent::Output(data) => output.push(data),
        PtyEvent::Exited(code) => {
            // The PTY layer delivered all output and closed the pseudoconsole already.
            let code = code.map(i64::from);
            record_exit(&state, &id, code, "exited");
            let _ = exit_tx.send(Some(code));
            state.sessions.remove(&id);
        }
    })
}

fn record_exit(state: &State, id: &str, code: Option<i64>, reason: &str) {
    let payload = json!({ "exit_code": code, "reason": reason });
    if let Err(e) =
        state.record(NewEvent::new(kinds::SESSION_EXITED, actors::YOLMIRA, payload).for_session(id))
    {
        tracing::error!(session = id, error = %e, "could not record session.exited");
    }
}

/// On startup: sessions the log still shows RUNNING belonged to a previous daemon;
/// their shells died with it (D3, S1). Mark them UNKNOWN, then EXITED.
pub fn reconcile(state: &State) -> Result<usize, YolmiraError> {
    let stale: Vec<String> = state
        .with_store(|s| s.sessions())?
        .into_iter()
        .filter(|s| s.status == "RUNNING")
        .map(|s| s.id)
        .collect();
    for id in &stale {
        let event = NewEvent::new(
            kinds::SESSION_UNKNOWN,
            actors::YOLMIRA,
            json!({ "reason": "daemon_restart" }),
        );
        state.record(event.for_session(id))?;
        record_exit(state, id, None, "daemon_restart");
    }
    Ok(stale.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwd_must_be_an_absolute_folder() {
        let tmp = std::env::temp_dir();
        assert!(validate_cwd(&tmp.to_string_lossy()).is_ok());
        for bad in [
            r"relative\dir",
            r"\\?\C:\Windows",
            r"\\.\PhysicalDrive0",
            r"C:",
            "",
            r"C:\definitely\not\here",
        ] {
            assert!(validate_cwd(bad).is_err(), "{bad:?}");
        }
    }
}
