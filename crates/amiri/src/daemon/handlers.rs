//! Method handlers. Mutating methods record their event (protocol `METHODS` table).
//! Store reads run on the blocking pool and in bounded pages.

use super::server::{to_line, Conn};
use super::session_handlers::{self as sh, ConnTasks};
use super::state::State;
use amiri_core::event::kinds;
use amiri_core::{AmiriError, Event, NewEvent};
use amiri_ipc::protocol::{
    codes, methods, notifications, DaemonStatus, EventsListParams, EventsSubscribeParams,
    Notification, Request, Response, RpcError,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

/// Largest `events.list` page a client may ask for.
pub const MAX_LIST_LIMIT: u32 = 10_000;
/// Page size for subscription backlog and lag refill.
const PAGE: u32 = 500;

pub enum Outcome {
    Reply(Response),
    /// Send the reply, then stop the daemon.
    ReplyThenStop(Response),
}

pub async fn dispatch(
    state: &Arc<State>,
    req: &Request,
    conn: &Conn,
    tx: &mpsc::Sender<String>,
    tasks: &mut ConnTasks,
) -> Outcome {
    let client = conn.client.as_str();
    let result = match req.method.as_str() {
        methods::SESSION_CREATE => match params(req) {
            Ok(p) => sh::create(state, p, client).await,
            Err(e) => Err(e),
        },
        methods::SESSION_LIST => sh::list(state).await,
        methods::SESSION_ATTACH => {
            params(req).and_then(|p| sh::attach(state, p, client, tx, tasks))
        }
        methods::SESSION_DETACH => params(req).and_then(|p| sh::detach(p, tasks)),
        methods::SESSION_INPUT => match params(req) {
            Ok(p) => sh::input(state, p).await,
            Err(e) => Err(e),
        },
        methods::SESSION_RESIZE => match params(req) {
            Ok(p) => sh::resize(state, p, client).await,
            Err(e) => Err(e),
        },
        methods::SESSION_KILL => match params(req) {
            Ok(p) => sh::kill(state, p, client).await,
            Err(e) => Err(e),
        },
        methods::DAEMON_STATUS => status(state),
        methods::DAEMON_STOP => return stop(state, req, client),
        methods::EVENTS_LIST => match params::<EventsListParams>(req) {
            Ok(p) => list(state, p).await,
            Err(e) => Err(e),
        },
        methods::EVENTS_SUBSCRIBE => params::<EventsSubscribeParams>(req).and_then(|p| {
            if tasks.events.is_some() {
                return Err(RpcError::new(
                    codes::INVALID_REQUEST,
                    "already subscribed on this connection",
                ));
            }
            tasks.events = Some(subscribe(state, p, tx));
            Ok(json!({ "subscribed": true }))
        }),
        methods::HELLO => Err(RpcError::new(
            codes::INVALID_REQUEST,
            "already authenticated",
        )),
        other => Err(RpcError::new(
            codes::METHOD_NOT_FOUND,
            format!("unknown method {other}"),
        )),
    };
    Outcome::Reply(match result {
        Ok(v) => Response::ok(req.id, v),
        Err(e) => Response::err(req.id, e),
    })
}

fn stop(state: &State, req: &Request, client: &str) -> Outcome {
    let event = NewEvent::new(
        kinds::DAEMON_STOPPING,
        format!("client:{client}"),
        json!({ "reason": "ipc_stop" }),
    );
    match state.record(event) {
        Ok(_) => Outcome::ReplyThenStop(Response::ok(req.id, json!({ "stopping": true }))),
        Err(e) => Outcome::Reply(Response::err(req.id, RpcError::amiri(e))),
    }
}

fn params<T: DeserializeOwned>(req: &Request) -> Result<T, RpcError> {
    let p = if req.params.is_null() {
        json!({})
    } else {
        req.params.clone()
    };
    serde_json::from_value(p).map_err(|e| RpcError::new(codes::INVALID_PARAMS, e.to_string()))
}

fn status(state: &State) -> Result<Value, RpcError> {
    let last_seq = state
        .with_store(|s| s.last_seq())
        .map_err(RpcError::amiri)?;
    Ok(json!(DaemonStatus {
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").into(),
        started_at: state.started_at.clone(),
        instance: state.instance.name.clone(),
        home: state.instance.home.to_string_lossy().into_owned(),
        clients: state.clients(),
        last_seq,
    }))
}

async fn list(state: &Arc<State>, p: EventsListParams) -> Result<Value, RpcError> {
    let limit = p.limit.min(MAX_LIST_LIMIT);
    let events = read_page(state, p.after, limit, p.kind_prefix)
        .await
        .map_err(RpcError::amiri)?;
    Ok(json!(events))
}

/// A store read on the blocking pool (SQLite I/O must not stall a runtime worker).
async fn read_page(
    state: &Arc<State>,
    after: i64,
    limit: u32,
    kind_prefix: Option<String>,
) -> Result<Vec<Event>, AmiriError> {
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        state.with_store(|s| s.list(after, limit, kind_prefix.as_deref()))
    })
    .await
    .unwrap_or_else(|e| {
        Err(AmiriError::new(
            "Read events",
            "amirid",
            format!("reader task failed: {e}"),
        ))
    })
}

/// Replays events after `p.after` (if given) page by page, then forwards new ones.
/// Subscribing to the broadcast first and de-duplicating by `seq` avoids a gap.
fn subscribe(
    state: &Arc<State>,
    p: EventsSubscribeParams,
    tx: &mpsc::Sender<String>,
) -> JoinHandle<()> {
    let live = state.subscribe();
    let (tx, state) = (tx.clone(), Arc::clone(state));
    tokio::spawn(async move {
        let last = match p.after {
            Some(after) => after,
            None => match state.with_store(|s| s.last_seq()) {
                Ok(seq) => seq,
                Err(e) => return tracing::error!(error = %e, "subscription start failed"),
            },
        };
        forward(state, live, tx, last).await;
    })
}

async fn forward(
    state: Arc<State>,
    mut live: broadcast::Receiver<Event>,
    tx: mpsc::Sender<String>,
    mut last: i64,
) {
    // Backlog, then whenever the broadcast lags: read from the log until caught up.
    let mut catch_up = true;
    loop {
        let batch = if catch_up {
            match read_page(&state, last, PAGE, None).await {
                Ok(page) => {
                    catch_up = page.len() == PAGE as usize;
                    page
                }
                Err(e) => {
                    return tracing::error!(error = %e, "event read failed; closing the subscription")
                }
            }
        } else {
            match live.recv().await {
                Ok(e) => vec![e],
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        skipped = n,
                        after = last,
                        "subscriber lagged; refilling from the event log"
                    );
                    catch_up = true;
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        };
        for e in batch {
            if e.seq <= last {
                continue;
            }
            last = e.seq;
            if tx.send(notify(&e)).await.is_err() {
                return;
            }
        }
    }
}

fn notify(e: &Event) -> String {
    serde_json::to_string(&Notification::new(notifications::EVENT, json!(e))).unwrap_or_else(
        |err| {
            tracing::error!(seq = e.seq, error = %err, "event serialize failed");
            to_line(&Response::err_unidentified(RpcError::new(
                codes::AMIRI_ERROR,
                "event serialize failed",
            )))
        },
    )
}
