//! `session.*` methods. Attachments are per connection: a task forwards a session's
//! output to that client until it detaches, disconnects, or the shell exits.
//! Blocking work (process creation, pipe writes, SQLite) runs on the blocking pool.

use super::outbox::{Offer, Outbox};
use super::sessions::{self, CreateRequest, LiveSession};
use super::state::State;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use yolmira_core::event::kinds;
use yolmira_core::{NewEvent, YolmiraError};
use yolmira_exec::ShellKind;
use yolmira_ipc::protocol::{
    codes, notifications, Notification, RpcError, SessionAttachResult, SessionCreateParams,
    SessionExited, SessionIdParams, SessionInfo, SessionInputParams, SessionOutput,
    SessionResizeParams,
};

const WHERE: &str = "yolmirad sessions";
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
    RpcError::yolmira(
        YolmiraError::new(what, WHERE, e.to_string())
            .next_step("Retry; if it persists, run `yolmira doctor`."),
    )
}

fn live(state: &State, id: &str) -> Result<Arc<LiveSession>, RpcError> {
    match state.sessions.get(id) {
        Some(s) if !s.has_exited() => Ok(s),
        _ => Err(RpcError::yolmira(
            YolmiraError::new(
                "Find the session",
                WHERE,
                format!("no running session {id}"),
            )
            .next_step("List sessions with `yolmira shell list`."),
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
        project: p.project,
    };
    let state = Arc::clone(state);
    blocking(move || {
        sessions::create(&state, req)
            .map(|s| json!(s.info()))
            .map_err(RpcError::yolmira)
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
            .map_err(RpcError::yolmira)?;
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
                    project: r.project,
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
    connection: u64,
    tx: &Arc<Outbox>,
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
    let replay_bytes = session.output.attach() as u64;
    let reason: Reason = Arc::new(Mutex::new("disconnect"));
    let guard = AttachGuard {
        state: Arc::clone(state),
        session: Arc::clone(&session),
        client: client.into(),
        connection,
        reason: Arc::clone(&reason),
    };
    let attached = NewEvent::new(
        kinds::SESSION_ATTACHED,
        format!("client:{client}"),
        json!({}),
    )
    .for_session(&p.id);
    state.record(attached).map_err(RpcError::yolmira)?;
    let task = tokio::spawn(forward(guard, Arc::clone(tx)));
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
        state.record(event).map_err(RpcError::yolmira)?;
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
        state.record(event).map_err(RpcError::yolmira)?;
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
    connection: u64,
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

/// Forwards a session's output to one client. Output never waits for the client: when
/// the client's outbox is over budget (or the broadcast lagged), its queued output for
/// this session is dropped and it is marked behind. While behind, nothing is queued for
/// it; once a whole snapshot fits, it gets one fresh scrollback snapshot (`reset`) and a
/// new subscription taken atomically with it, so its output resumes without a gap.
async fn forward(guard: AttachGuard, out: Arc<Outbox>) {
    let id: Arc<str> = guard.session.id.as_str().into();
    // The attach replay is the first snapshot, taken like any resync.
    let Some(mut rx) = catch_up(&guard, &out, &id, false).await else {
        return;
    };
    let mut behind = false;
    let mut times_behind = 0u64;
    // Chunks queued since the last snapshot.
    let mut delivered = 0u64;
    let mut exit = guard.session.exit.clone();
    loop {
        if behind {
            match catch_up(&guard, &out, &id, true).await {
                Some(fresh) => {
                    rx = fresh;
                    behind = false;
                    delivered = 0;
                }
                None => return,
            }
        }
        tokio::select! {
            r = rx.recv() => match r {
                Ok(data) => {
                    let line = output_line(&id, &data, false);
                    let offered = line.len();
                    match out.offer_output(&id, vec![line]) {
                        Offer::Queued => delivered += 1,
                        Offer::Full => {
                            times_behind += 1;
                            log_behind(&guard, &out, &id, Behind::Full { offered }, times_behind);
                            out.drop_output(&id);
                            behind = true;
                        }
                        Offer::Closed => return,
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    times_behind += 1;
                    log_behind(&guard, &out, &id, Behind::Lagged { skipped, delivered }, times_behind);
                    out.drop_output(&id);
                    behind = true;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            changed = exit.changed() => {
                if changed.is_err() || exit.borrow().is_some() {
                    // All output was delivered before the exit (PTY layer); pass it on.
                    while let Ok(data) = rx.try_recv() {
                        let line = output_line(&id, &data, false);
                        let offered = line.len();
                        if out.offer_output(&id, vec![line]) != Offer::Queued {
                            times_behind += 1;
                            log_behind(&guard, &out, &id, Behind::Full { offered }, times_behind);
                            out.drop_output(&id);
                            if catch_up(&guard, &out, &id, true).await.is_none() {
                                return;
                            }
                            break; // the snapshot holds everything up to the exit
                        }
                    }
                    let code = exit.borrow().flatten();
                    set_reason(&guard.reason, "exited");
                    let n = Notification::new(notifications::SESSION_EXITED, json!(SessionExited { id: id.to_string(), exit_code: code }));
                    send_line(&out, &n).await;
                    return;
                }
            }
        }
    }
}

/// Why a client fell behind on a session's output.
enum Behind {
    /// A budget refused `offered` bytes (the connection's, or the daemon-wide one beyond
    /// the connection's floor: the logged `queued` and `daemon_used` tell which).
    Full { offered: usize },
    /// The forwarder lagged the shell by `skipped` chunks, having queued `delivered` since
    /// its last snapshot (0: it lagged before passing on anything after the snapshot).
    Lagged { skipped: u64, delivered: u64 },
}

/// Count and log a client falling behind, with what it had queued. A client that keeps up
/// should never fall behind (`output_budget` tests), so the first time per attachment is
/// logged at info; later times at debug, so a slow UI can't flood the log.
fn log_behind(guard: &AttachGuard, out: &Outbox, id: &str, why: Behind, times: u64) {
    let (cause, offered, skipped, delivered) = match why {
        Behind::Full { offered } => ("full", offered, 0, 0),
        Behind::Lagged { skipped, delivered } => ("lagged", 0, skipped, delivered),
    };
    let lagged = matches!(why, Behind::Lagged { .. });
    out.global().count_behind(lagged, lagged && delivered == 0);
    let (queued, daemon_used) = (out.queued_output(), out.global().used());
    let connection = guard.connection;
    if times == 1 {
        tracing::info!(
            connection,
            session = id,
            cause,
            offered,
            skipped,
            delivered,
            queued,
            daemon_used,
            times,
            "client fell behind on output"
        );
    } else {
        tracing::debug!(
            connection,
            session = id,
            cause,
            offered,
            skipped,
            delivered,
            queued,
            daemon_used,
            times,
            "client fell behind on output"
        );
    }
}

/// Room for the largest snapshot: the whole scrollback, base64, in `REPLAY_CHUNK` pieces.
fn snapshot_room() -> usize {
    let cap = yolmira_exec::ring::DEFAULT_CAPACITY;
    cap.div_ceil(3) * 4 + cap.div_ceil(REPLAY_CHUNK) * 256
}

/// Queue one snapshot of the scrollback (`resync`: a client that fell behind; otherwise
/// the attach replay) with a new subscription. Waits until a whole snapshot fits the
/// connection's and the daemon's budgets, and for one of the daemon's few snapshot
/// permits (bounding the copies being encoded at once). Never queues more than one
/// snapshot, and never one that doesn't fit. `None` when the connection is gone.
async fn catch_up(
    guard: &AttachGuard,
    out: &Outbox,
    id: &Arc<str>,
    resync: bool,
) -> Option<broadcast::Receiver<Arc<Vec<u8>>>> {
    loop {
        if !out.wait_output_room(snapshot_room()).await {
            return None;
        }
        let _permit = out.global().snapshot_permit().await;
        let (snapshot, fresh) = guard.session.output.resync();
        match out.offer_output(id, replay_lines(id, &snapshot)) {
            Offer::Queued => {
                if resync {
                    out.global().count_resync();
                }
                return Some(fresh);
            }
            // Another session on this connection, or another connection, took the room.
            Offer::Full => continue,
            Offer::Closed => return None,
        }
    }
}

/// The scrollback as `session.output` lines: `reset` on the first (the client clears),
/// each at most `REPLAY_CHUNK` bytes.
fn replay_lines(id: &str, replay: &[u8]) -> Vec<String> {
    if replay.is_empty() {
        return vec![output_line(id, &[], true)];
    }
    replay
        .chunks(REPLAY_CHUNK)
        .enumerate()
        .map(|(i, chunk)| output_line(id, chunk, i == 0))
        .collect()
}

fn output_line(id: &str, data: &[u8], reset: bool) -> String {
    let n = Notification::new(
        notifications::SESSION_OUTPUT,
        json!(SessionOutput {
            id: id.into(),
            data: B64.encode(data),
            reset
        }),
    );
    serde_json::to_string(&n).unwrap_or_else(|e| {
        tracing::error!(error = %e, "output serialize failed");
        String::new()
    })
}

async fn send_line(out: &Outbox, n: &Notification) -> bool {
    match serde_json::to_string(n) {
        Ok(line) => out.send(line).await,
        Err(e) => {
            tracing::error!(error = %e, "notification serialize failed");
            true
        }
    }
}
