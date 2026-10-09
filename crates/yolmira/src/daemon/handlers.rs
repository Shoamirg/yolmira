//! Method handlers. Mutating methods record their event (protocol `METHODS` table).
//! Store reads run on the blocking pool and in bounded pages.

use super::outbox::Outbox;
use super::policy::{self, Check};
use super::project_handlers as ph;
use super::server::{to_line, Conn};
use super::session_handlers::{self as sh, ConnTasks};
use super::state::State;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use yolmira_core::event::kinds;
use yolmira_core::policy::actions;
use yolmira_core::{Event, NewEvent, YolmiraError};
use yolmira_ipc::protocol::{
    codes, methods, notifications, DaemonStatus, EventsListParams, EventsSubscribeParams,
    Notification, Request, Response, RpcError, SessionCreateParams, SessionIdParams,
};

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
    tx: &Arc<Outbox>,
    tasks: &mut ConnTasks,
) -> Outcome {
    let client = conn.client.as_str();
    let result = match req.method.as_str() {
        methods::SESSION_CREATE => match params::<SessionCreateParams>(req) {
            Ok(p) => match policy::check(state, create_check(&p, conn)).await {
                Ok(()) => sh::create(state, p, client).await,
                Err(e) => Err(e),
            },
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
        methods::SESSION_KILL => match params::<SessionIdParams>(req) {
            Ok(p) => match policy::check(state, kill_check(state, &p, conn)).await {
                Ok(()) => sh::kill(state, p, client).await,
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        },
        methods::DAEMON_STATUS => status(state),
        methods::DAEMON_STOP => {
            let check = Check {
                action: actions::DAEMON_STOP,
                project: None,
                detail: json!({ "running_shells": state.sessions.all().iter().filter(|s| !s.has_exited()).count() }),
                client,
                connection: conn.id,
            };
            return match policy::check(state, check).await {
                Ok(()) => stop(state, req, client),
                Err(e) => Outcome::Reply(Response::err(req.id, e)),
            };
        }
        methods::POLICY_PENDING => Ok(json!(state.policy.pending())),
        methods::POLICY_ANSWER => {
            params(req).and_then(|p| policy::answer(state, p, client, &conn.peer))
        }
        methods::POLICY_CODE => {
            params(req).and_then(|p| policy::code(state, p, client, &conn.peer))
        }
        methods::PROJECT_ADD => match params(req) {
            Ok(p) => ph::add(state, p, conn).await,
            Err(e) => Err(e),
        },
        methods::PROJECT_LIST => ph::list(state),
        methods::PROJECT_REMOVE => match params(req) {
            Ok(p) => ph::remove(state, p, conn).await,
            Err(e) => Err(e),
        },
        #[cfg(debug_assertions)]
        DEBUG_SPAWN_IN_AGENT_JOB => params(req).and_then(|p| spawn_in_agent_job(state, p)),
        #[cfg(debug_assertions)]
        DEBUG_OUTPUT_STATS => Ok(output_stats(state)),
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

fn create_check<'a>(p: &SessionCreateParams, conn: &'a Conn) -> Check<'a> {
    Check {
        action: actions::SESSION_CREATE,
        project: p.project.clone(),
        detail: json!({ "shell": p.shell, "cwd": p.cwd, "project": p.project }),
        client: &conn.client,
        connection: conn.id,
    }
}

/// The shell's own project decides which rules apply to killing it.
fn kill_check<'a>(state: &State, p: &SessionIdParams, conn: &'a Conn) -> Check<'a> {
    let session = state.sessions.get(&p.id);
    Check {
        action: actions::SESSION_KILL,
        project: session.as_ref().and_then(|s| s.project.clone()),
        detail: json!({
            "id": p.id,
            "shell": session.as_ref().map(|s| s.shell.clone()),
            "cwd": session.as_ref().map(|s| s.cwd.clone()),
            "pid": session.as_ref().and_then(|s| s.pty.pid()),
        }),
        client: &conn.client,
        connection: conn.id,
    }
}

/// Debug builds only, for tests: run a program inside a new agent job, as Phase 4
/// will run agents (ARCHITECTURE §9.6). It lets a test play an agent that tries to
/// approve its own request. Anyone with the token can already start shells, so this
/// gives a client nothing new; release builds don't have it.
#[cfg(debug_assertions)]
pub const DEBUG_SPAWN_IN_AGENT_JOB: &str = "debug.spawn_in_agent_job";

/// Debug builds only, for tests: the output budgets and their high-water marks
/// (`outbox`), so a test can check that slow clients stayed within them.
#[cfg(debug_assertions)]
pub const DEBUG_OUTPUT_STATS: &str = "debug.output_stats";

#[cfg(debug_assertions)]
fn output_stats(state: &State) -> Value {
    let (limit, used, high, conn_high, resyncs) = state.output_budget.stats();
    json!({
        "daemon_limit": limit,
        "daemon_used": used,
        "daemon_high_water": high,
        "connection_limit": super::outbox::OUTPUT_BUDGET,
        "connection_high_water": conn_high,
        "resyncs": resyncs,
    })
}

#[cfg(debug_assertions)]
#[derive(serde::Deserialize)]
struct SpawnInJobParams {
    exe: String,
    args: Vec<String>,
}

#[cfg(debug_assertions)]
fn spawn_in_agent_job(state: &State, p: SpawnInJobParams) -> Result<Value, RpcError> {
    use yolmira_ipc::win::process::{SpawnOptions, CREATE_NO_WINDOW};
    let fail = |e: std::io::Error| RpcError::new(codes::YOLMIRA_ERROR, e.to_string());
    let job = state.agent_jobs.create().map_err(fail)?;
    let process = job
        .spawn(&SpawnOptions {
            exe: p.exe.into(),
            args: p.args,
            env: None,
            cwd: None,
            flags: CREATE_NO_WINDOW,
        })
        .map_err(fail)?;
    Ok(json!({ "pid": process.pid() }))
}

fn stop(state: &State, req: &Request, client: &str) -> Outcome {
    let event = NewEvent::new(
        kinds::DAEMON_STOPPING,
        format!("client:{client}"),
        json!({ "reason": "ipc_stop" }),
    );
    match state.record(event) {
        Ok(_) => Outcome::ReplyThenStop(Response::ok(req.id, json!({ "stopping": true }))),
        Err(e) => Outcome::Reply(Response::err(req.id, RpcError::yolmira(e))),
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
        .map_err(RpcError::yolmira)?;
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
        .map_err(RpcError::yolmira)?;
    Ok(json!(events))
}

/// A store read on the blocking pool (SQLite I/O must not stall a runtime worker).
async fn read_page(
    state: &Arc<State>,
    after: i64,
    limit: u32,
    kind_prefix: Option<String>,
) -> Result<Vec<Event>, YolmiraError> {
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        state.with_store(|s| s.list(after, limit, kind_prefix.as_deref()))
    })
    .await
    .unwrap_or_else(|e| {
        Err(YolmiraError::new(
            "Read events",
            "yolmirad",
            format!("reader task failed: {e}"),
        ))
    })
}

/// Replays events after `p.after` (if given) page by page, then forwards new ones.
/// Subscribing to the broadcast first and de-duplicating by `seq` avoids a gap.
fn subscribe(state: &Arc<State>, p: EventsSubscribeParams, tx: &Arc<Outbox>) -> JoinHandle<()> {
    let live = state.subscribe();
    let (tx, state) = (Arc::clone(tx), Arc::clone(state));
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
    tx: Arc<Outbox>,
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
            if !tx.send(notify(&e)).await {
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
                codes::YOLMIRA_ERROR,
                "event serialize failed",
            )))
        },
    )
}
