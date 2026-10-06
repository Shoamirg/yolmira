//! Named-pipe server (phase1-plan slice A): owner-only DACL with a mandatory label
//! (Medium, or High when elevated), remote clients rejected, first instance owned.

use super::auth::authenticate;
use super::handlers::{self, Outcome};
use super::session_handlers::ConnTasks;
use super::state::State;
use amiri_ipc::framing::{read_line, MAX_LINE};
use amiri_ipc::protocol::{codes, Request, Response, RpcError};
use amiri_ipc::win::security::{owner_only_sddl, SecurityDescriptor};
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{mpsc, Semaphore};

const OUTBOX: usize = 256;
/// Connections that haven't completed `hello` yet. More are dropped on arrival.
const MAX_UNAUTHENTICATED: usize = 16;
const RETRY: Duration = Duration::from_millis(200);

pub struct PipeFactory {
    name: String,
    sd: SecurityDescriptor,
}

impl PipeFactory {
    pub fn new(state: &State) -> io::Result<Self> {
        Ok(Self {
            name: state.instance.pipe_name(),
            sd: SecurityDescriptor::from_sddl(&owner_only_sddl(
                &state.instance.sid,
                state.elevated,
            ))?,
        })
    }

    /// `first`: fail if any process already owns the name (no squatting, single daemon).
    pub fn create(&mut self, first: bool) -> io::Result<NamedPipeServer> {
        let mut opts = ServerOptions::new();
        opts.first_pipe_instance(first).reject_remote_clients(true);
        // SAFETY: the SECURITY_ATTRIBUTES points at a descriptor owned by `self.sd`,
        // which outlives the call; the kernel copies it during creation.
        unsafe {
            opts.create_with_security_attributes_raw(&self.name, self.sd.attributes_mut_void())
        }
    }

    /// Keep trying until a new instance exists: without one, no client can connect.
    async fn create_next(&mut self) -> NamedPipeServer {
        loop {
            match self.create(false) {
                Ok(p) => return p,
                Err(e) => {
                    tracing::error!(error = %e, "could not create a pipe instance; retrying");
                    tokio::time::sleep(RETRY).await;
                }
            }
        }
    }
}

/// An authenticated connection: the client's name and its id for this daemon run.
pub struct Conn {
    pub client: String,
    pub id: u64,
}

/// Accept connections until the task is dropped.
pub async fn serve(state: Arc<State>, mut factory: PipeFactory, first: NamedPipeServer) {
    let gate = Arc::new(Semaphore::new(MAX_UNAUTHENTICATED));
    let mut server = first;
    loop {
        let connected = server.connect().await;
        // Replace the instance first, so the name is always being listened on.
        let current = std::mem::replace(&mut server, factory.create_next().await);
        if let Err(e) = connected {
            tracing::warn!(error = %e, "pipe connect failed");
            continue;
        }
        match Arc::clone(&gate).try_acquire_owned() {
            Ok(permit) => {
                tokio::spawn(connection(Arc::clone(&state), current, permit));
            }
            Err(_) => {
                state.record_auth_failure("too_many_unauthenticated", None);
                drop(current);
            }
        }
    }
}

async fn connection(
    state: Arc<State>,
    pipe: NamedPipeServer,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    let (r, mut w) = tokio::io::split(pipe);
    let mut reader = BufReader::new(r);
    let (tx, mut rx) = mpsc::channel::<String>(OUTBOX);
    let writer = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if w.write_all(format!("{line}\n").as_bytes()).await.is_err()
                || w.flush().await.is_err()
            {
                break;
            }
        }
    });
    let client = authenticate(&state, &mut reader, &tx).await;
    drop(permit);
    let mut tasks = ConnTasks::default();
    if let Some((client, connection)) = client {
        state.client_connected();
        let conn = Conn {
            client,
            id: connection,
        };
        serve_requests(&state, &mut reader, &tx, &conn, &mut tasks).await;
        state.client_disconnected();
    }
    // Subscription and attachment tasks hold senders; stop them so the writer finishes.
    tasks.abort_all();
    drop(tx);
    if let Err(e) = writer.await {
        tracing::debug!(error = %e, "connection writer ended abnormally");
    }
}

async fn serve_requests<R: tokio::io::AsyncBufRead + Unpin>(
    state: &Arc<State>,
    reader: &mut R,
    tx: &mpsc::Sender<String>,
    conn: &Conn,
    tasks: &mut ConnTasks,
) {
    let client = conn.client.as_str();
    tracing::debug!(client, connection = conn.id, "client authenticated");
    loop {
        let line = match read_line(reader, MAX_LINE).await {
            Ok(Some(line)) => line,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(client, error = %e, "closing connection: bad frame");
                return;
            }
        };
        let reply = match serde_json::from_str::<Request>(&line) {
            Ok(req) => match handlers::dispatch(state, &req, conn, tx, tasks).await {
                Outcome::Reply(r) => r,
                Outcome::ReplyThenStop(r) => {
                    send(tx, &r).await;
                    super::shutdown::request("ipc_stop");
                    return;
                }
            },
            Err(e) => Response::err_unidentified(RpcError::new(codes::PARSE_ERROR, e.to_string())),
        };
        if !send(tx, &reply).await {
            return;
        }
    }
}

/// Queue a response; `false` if the connection is gone.
pub async fn send(tx: &mpsc::Sender<String>, r: &Response) -> bool {
    tx.send(to_line(r)).await.is_ok()
}

pub fn to_line(r: &Response) -> String {
    serde_json::to_string(r).unwrap_or_else(|e| {
        let id = r.id.map_or("null".to_string(), |i| i.to_string());
        format!(
            r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32603,"message":"serialize: {e}"}}}}"#
        )
    })
}
