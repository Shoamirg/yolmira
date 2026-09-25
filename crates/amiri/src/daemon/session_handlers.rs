//! `session.*` methods. Attachments are per connection: a task forwards a session's
//! output to that client until it detaches, disconnects, or the shell exits.
//! Blocking work (process creation, pipe writes, SQLite) runs on the blocking pool.

use super::sessions::{self, CreateRequest, LiveSession};
use super::state::State;
use amiri_core::event::kinds;
use amiri_core::{AmiriError, NewEvent};
use amiri_exec::ShellKind;
use amiri_ipc::protocol::{
    codes, notifications, Notification, RpcError, SessionAttachResult, SessionCreateParams,
    SessionExited, SessionIdParams, SessionInfo, SessionInputParams, SessionOutput,
    SessionResizeParams,
};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

const WHERE: &str = "amirid sessions";
const MAX_COLS_ROWS: u16 = 1000;
/// Replay/resync is sent in pieces: each notification stays far below the 1 MiB frame.
const REPLAY_CHUNK: usize = 256 * 1024;
/// Ended sessions `session.list` returns (all running ones are always included).
const LIST_ENDED: usize = 200;

type Reason = Arc<Mutex<&'static str>>;

/// Tasks a connection owns; aborted when it ends.
#[derive(Default)]
pub struct ConnTasks {
    pub events: Option<JoinHandle<()>>,
    attachments: HashMap<String, (JoinHandle<()>, Reason)>,
}

impl ConnTasks {
    pub fn abort_all(&mut self) {
        if let Some(t) = self.events.take() {
            t.abort();
        }
        for (_, (task, _)) in self.attachments.drain() {
            task.abort();
        }
    }
}

fn set_reason(reason: &Reason, value: &'static str) {
    *reason.lock().unwrap_or_else(|e| e.into_inner()) = value;
}

fn failed(what: &str, e: impl std::fmt::Display) -> RpcError {
    RpcError::amiri(
        AmiriError::new(what, WHERE, e.to_string())
            .next_step("Retry; if it persists, run `amiri doctor`."),
    )
}

fn live(state: &State, id: &str) -> Result<Arc<LiveSession>, RpcError> {
    match state.sessions.get(id) {
        Some(s) if !s.has_exited() => Ok(s),
        _ => Err(RpcError::amiri(
            AmiriError::new(
                "Find the session",
                WHERE,
                format!("no running session {id}"),
            )
            .next_step("List sessions with `amiri shell list`."),
        )),
    }
}

fn size_ok(cols: u16, rows: u16) -> Result<(), RpcError> {
    if (1..=MAX_COLS_ROWS).contains(&cols) && (1..=MAX_COLS_ROWS).contains(&rows) {
        Ok(())
    } else {
        Err(RpcError::new(
            codes::INVALID_PARAMS,
            format!("terminal size {cols}x{rows} out of range"),
        ))
    }
}

/// Run blocking session work off the async workers.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, RpcError> + Send + 'static,
) -> Result<T, RpcError> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(failed("Run the request", e)))
}

pub async fn create(
    state: &Arc<State>,
    p: SessionCreateParams,
    client: &str,
) -> Result<Value, RpcError> {
    size_ok(p.cols, p.rows)?;
    let shell: ShellKind = serde_json::from_value(p.shell)
        .map_err(|e| RpcError::new(codes::INVALID_PARAMS, e.to_string()))?;
    let req = CreateRequest {
        shell,
        cwd: p.cwd,
        cols: p.cols,
        rows: p.rows,
        client: client.into(),
    };
    let state = Arc::clone(state);
    blocking(move || {
        sessions::create(&state, req)
            .map(|s| json!(s.info()))
            .map_err(RpcError::amiri)
    })
    .await
}

pub async fn list(state: &Arc<State>) -> Result<Value, RpcError> {
    let state = Arc::clone(state);
    blocking(move || {
        let live: HashMap<String, SessionInfo> = state
            .sessions
            .all()
            .iter()
            .map(|s| (s.id.clone(), s.info()))
            .collect();
        let rows = state
            .with_store(|s| s.sessions())
            .map_err(RpcError::amiri)?;
        let mut ended = 0;
        let mut all: Vec<SessionInfo> = Vec::new();
        // Newest first, so the ended-session limit keeps the recent ones.
        for r in rows.into_iter().rev() {
            if let Some(info) = live.get(&r.id) {
                all.push(info.clone());
            } else if ended < LIST_ENDED {
                ended += 1;
                all.push(SessionInfo {
                    id: r.id,
                    shell: r.shell_kind.unwrap_or_default(),
                    cwd: r.cwd.unwrap_or_default(),
                    pid: None,
                    status: r.status,
                    exit_code: r.exit_code,
                    attached: 0,
                    cols: 0,
                    rows: 0,
                });
            }
        }
        all.reverse();
        Ok(json!(all))
    })
    .await
}

pub fn attach(
    state: &Arc<State>,
    p: SessionIdParams,
    client: &str,
    tx: &mpsc::Sender<String>,
    tasks: &mut ConnTasks,
) -> Result<Value, RpcError> {
    tasks.attachments.retain(|_, (task, _)| !task.is_finished());
    if tasks.attachments.contains_key(&p.id) {
        return Err(RpcError::new(
            codes::INVALID_REQUEST,
            "already attached on this connection",
        ));
    }
    let session = live(state, &p.id)?;
    let (replay, rx) = session.output.attach();
    let reason: Reason = Arc::new(Mutex::new("disconnect"));
    let guard = AttachGuard {
        state: Arc::clone(state),
        session: Arc::clone(&session),
        client: client.into(),
        reason: Arc::clone(&reason),
    };
    let attached = NewEvent::new(
        kinds::SESSION_ATTACHED,
        format!("client:{client}"),
        json!({}),
    )
    .for_session(&p.id);
    state.record(attached).map_err(RpcError::amiri)?;
    let replay_bytes = replay.len() as u64;
    let task = tokio::spawn(forward(guard, replay, rx, tx.clone()));
    tasks.attachments.insert(p.id, (task, reason));
    Ok(json!(SessionAttachResult {
        info: session.info(),
        replay_bytes
    }))
}

pub fn detach(p: SessionIdParams, tasks: &mut ConnTasks) -> Result<Value, RpcError> {
    let (task, reason) = tasks
        .attachments
        .remove(&p.id)
        .ok_or_else(|| RpcError::new(codes::INVALID_REQUEST, "not attached on this connection"))?;
    set_reason(&reason, "detach");
    task.abort(); // the guard records session.detached
    Ok(json!({ "detached": true }))
}

pub async fn input(state: &Arc<State>, p: SessionInputParams) -> Result<Value, RpcError> {
    let data = B64
        .decode(p.data)
        .map_err(|e| RpcError::new(codes::INVALID_PARAMS, format!("data: {e}")))?;
    let session = live(state, &p.id)?;
    let n = data.len();
    blocking(move || {
        session
            .pty
            .write(&data)
            .map_err(|e| failed("Send input", e))
    })
    .await?;
    Ok(json!({ "written": n }))
}

pub async fn resize(
    state: &Arc<State>,
    p: SessionResizeParams,
    client: &str,
) -> Result<Value, RpcError> {
    size_ok(p.cols, p.rows)?;
    let session = live(state, &p.id)?;
    let (state, client) = (Arc::clone(state), client.to_string());
    blocking(move || {
        session
            .pty
            .resize(p.cols, p.rows)
            .map_err(|e| failed("Resize the terminal", e))?;
        session.set_size(p.cols, p.rows);
        let payload = json!({ "cols": p.cols, "rows": p.rows });
        let event = NewEvent::new(kinds::SESSION_RESIZED, format!("client:{client}"), payload)
            .for_session(&p.id);
        state.record(event).map_err(RpcError::amiri)?;
        Ok(json!({ "cols": p.cols, "rows": p.rows }))
    })
    .await
}

pub async fn kill(state: &Arc<State>, p: SessionIdParams, client: &str) -> Result<Value, RpcError> {
    let session = live(state, &p.id)?;
    let (state, client) = (Arc::clone(state), client.to_string());
    blocking(move || {
        let event = NewEvent::new(
            kinds::SESSION_KILL_REQUESTED,
            format!("client:{client}"),
            json!({}),
        )
        .for_session(&p.id);
        state.record(event).map_err(RpcError::amiri)?;
        session.pty.kill().map_err(|e| failed("End the shell", e))?;
        Ok(json!({ "killing": true }))
    })
    .await
}

/// Decrements the attach count and records `session.detached` however the task ends.
struct AttachGuard {
    state: Arc<State>,
    session: Arc<LiveSession>,
    client: String,
    reason: Reason,
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        self.session.output.detach();
        let reason = *self.reason.lock().unwrap_or_else(|e| e.into_inner());
        let event = NewEvent::new(
            kinds::SESSION_DETACHED,
            format!("client:{}", self.client),
            json!({ "reason": reason }),
        )
        .for_session(&self.session.id);
        if let Err(e) = self.state.record(event) {
            tracing::error!(error = %e, "could not record session.detached");
        }
    }
}

async fn forward(
    guard: AttachGuard,
    replay: Vec<u8>,
    mut rx: broadcast::Receiver<Arc<Vec<u8>>>,
    tx: mpsc::Sender<String>,
) {
    let id = guard.session.id.clone();
    if !send_replay(&tx, &id, &replay).await {
        return;
    }
    let mut exit = guard.session.exit.clone();
    loop {
        tokio::select! {
            r = rx.recv() => match r {
                Ok(data) => {
                    if !send_output(&tx, &id, &data, false).await {
                        return;
                    }
                }
                // Fell behind: start over from the scrollback (the client clears and redraws).
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let (snapshot, fresh) = guard.session.output.resync();
                    rx = fresh;
                    if !send_replay(&tx, &id, &snapshot).await {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            changed = exit.changed() => {
                if changed.is_err() || exit.borrow().is_some() {
                    // All output was delivered before the exit (PTY layer); drain it.
                    while let Ok(data) = rx.try_recv() {
                        send_output(&tx, &id, &data, false).await;
                    }
                    let code = exit.borrow().flatten();
                    set_reason(&guard.reason, "exited");
                    let n = Notification::new(notifications::SESSION_EXITED, json!(SessionExited { id: id.clone(), exit_code: code }));
                    send_line(&tx, &n).await;
                    return;
                }
            }
        }
    }
}

/// The scrollback as `session.output` notifications: `reset` on the first (the client
/// clears), each at most `REPLAY_CHUNK` bytes.
async fn send_replay(tx: &mpsc::Sender<String>, id: &str, replay: &[u8]) -> bool {
    if replay.is_empty() {
        return send_output(tx, id, &[], true).await;
    }
    for (i, chunk) in replay.chunks(REPLAY_CHUNK).enumerate() {
        if !send_output(tx, id, chunk, i == 0).await {
            return false;
        }
    }
    true
}

async fn send_output(tx: &mpsc::Sender<String>, id: &str, data: &[u8], reset: bool) -> bool {
    let n = Notification::new(
        notifications::SESSION_OUTPUT,
        json!(SessionOutput {
            id: id.into(),
            data: B64.encode(data),
            reset
        }),
    );
    send_line(tx, &n).await
}

async fn send_line(tx: &mpsc::Sender<String>, n: &Notification) -> bool {
    match serde_json::to_string(n) {
        Ok(line) => tx.send(line).await.is_ok(),
        Err(e) => {
            tracing::error!(error = %e, "notification serialize failed");
            true
        }
    }
}
