//! A sequential daemon client: one outstanding call at a time; notifications that
//! arrive meanwhile are queued for `next_notification`.
//!
//! Before sending anything, the client checks who serves the pipe: the pipe name is
//! machine-global and predictable, so another user could create it first.

use crate::framing::{read_line, MAX_LINE};
use crate::paths::Instance;
use crate::protocol::{
    methods, HelloParams, HelloResult, Incoming, Notification, Request, RpcError, PROTOCOL_VERSION,
};
use crate::win::security::{current_identity, process_identity};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::VecDeque;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;

const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_PIPE_BUSY: i32 = 231;
const BUSY_RETRY: Duration = Duration::from_millis(50);
const BUSY_RETRIES: u32 = 100;
/// Upper bound for one request/response round trip; the daemon answers in milliseconds.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest a gated call can wait: the daemon's ASK limit (600 s) plus a margin.
pub const ASK_LIMIT: Duration = Duration::from_secs(630);

#[derive(Debug)]
pub enum ClientError {
    /// No daemon is listening on this instance's pipe.
    NotRunning,
    /// Something other than this user's daemon owns the pipe name. Nothing was sent.
    Impostor(String),
    /// The daemon refused the `hello` or a call.
    Rpc(RpcError),
    /// The daemon closed the connection.
    Closed,
    /// No answer within `CALL_TIMEOUT`.
    Timeout,
    Io(io::Error),
    Protocol(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRunning => write!(f, "the AMIRI daemon is not running"),
            Self::Impostor(why) => {
                write!(f, "the daemon's pipe is owned by another process: {why}")
            }
            Self::Rpc(e) => write!(f, "daemon error {}: {}", e.code, e.message),
            Self::Closed => write!(f, "the daemon closed the connection"),
            Self::Timeout => write!(
                f,
                "the daemon did not answer within {} s",
                CALL_TIMEOUT.as_secs()
            ),
            Self::Io(e) => write!(f, "pipe I/O failed: {e}"),
            Self::Protocol(m) => write!(f, "protocol error: {m}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<io::Error> for ClientError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

pub struct Client {
    reader: BufReader<ReadHalf<NamedPipeClient>>,
    writer: WriteHalf<NamedPipeClient>,
    next_id: u64,
    queued: VecDeque<Notification>,
    pub hello: HelloResult,
}

/// Open the instance's pipe (retrying while every instance is busy) and verify that
/// this user's process at our integrity level or higher serves it.
pub async fn open_pipe(instance: &Instance) -> Result<NamedPipeClient, ClientError> {
    let name = instance.pipe_name();
    for _ in 0..BUSY_RETRIES {
        match ClientOptions::new().open(&name) {
            Ok(pipe) => {
                verify_server(&pipe, instance)?;
                return Ok(pipe);
            }
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                tokio::time::sleep(BUSY_RETRY).await
            }
            Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => {
                return Err(ClientError::NotRunning)
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(ClientError::Protocol(
        "all pipe instances stayed busy".into(),
    ))
}

fn verify_server(pipe: &NamedPipeClient, instance: &Instance) -> Result<(), ClientError> {
    let mut pid = 0u32;
    // SAFETY: valid pipe handle owned by `pipe`; `pid` is an out-param.
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) } == 0 {
        return Err(ClientError::Impostor(format!(
            "server process unknown: {}",
            io::Error::last_os_error()
        )));
    }
    // Another user's process can't be opened by us at all: that's an impostor too.
    let server = process_identity(pid)
        .map_err(|e| ClientError::Impostor(format!("server pid {pid} not inspectable: {e}")))?;
    let me = current_identity()?;
    if server.sid != instance.sid {
        return Err(ClientError::Impostor(format!(
            "server pid {pid} runs as {}, not {}",
            server.sid, instance.sid
        )));
    }
    if server.integrity < me.integrity {
        return Err(ClientError::Impostor(format!(
            "server pid {pid} runs at integrity {:#x}, below this client's {:#x}",
            server.integrity, me.integrity
        )));
    }
    Ok(())
}

impl Client {
    /// Connect and authenticate with the token from the instance's `run\token`.
    pub async fn connect(instance: &Instance, client_name: &str) -> Result<Self, ClientError> {
        let pipe = open_pipe(instance).await?;
        let token = std::fs::read_to_string(instance.token_path())?
            .trim()
            .to_string();
        Self::handshake(pipe, &token, client_name).await
    }

    /// Hello with an explicit token (tests use this to present a wrong one).
    pub async fn handshake(
        pipe: NamedPipeClient,
        token: &str,
        client_name: &str,
    ) -> Result<Self, ClientError> {
        let (r, w) = tokio::io::split(pipe);
        let mut c = Self {
            reader: BufReader::new(r),
            writer: w,
            next_id: 1,
            queued: VecDeque::new(),
            hello: HelloResult {
                daemon_version: String::new(),
                protocol_version: 0,
                pid: 0,
                instance: None,
                connection: 0,
            },
        };
        let params = HelloParams {
            token: token.into(),
            protocol_version: PROTOCOL_VERSION,
            client: client_name.into(),
        };
        c.hello = c
            .call(
                methods::HELLO,
                serde_json::to_value(params).map_err(io::Error::other)?,
            )
            .await?;
        Ok(c)
    }

    /// Call `method` and decode the result.
    pub async fn call<T: DeserializeOwned>(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<T, ClientError> {
        let value = self.call_value(method, params).await?;
        serde_json::from_value(value)
            .map_err(|e| ClientError::Protocol(format!("{method} result: {e}")))
    }

    pub async fn call_value(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
        self.call_value_within(method, params, CALL_TIMEOUT).await
    }

    /// A call that may legitimately take longer than `CALL_TIMEOUT`: one the policy
    /// gate holds until someone approves it (up to `ASK_LIMIT`).
    pub async fn call_value_within(
        &mut self,
        method: &str,
        params: Value,
        limit: Duration,
    ) -> Result<Value, ClientError> {
        tokio::time::timeout(limit, self.round_trip(method, params))
            .await
            .unwrap_or(Err(ClientError::Timeout))
    }

    async fn round_trip(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let line =
            serde_json::to_string(&Request::new(id, method, params)).map_err(io::Error::other)?;
        self.writer
            .write_all(format!("{line}\n").as_bytes())
            .await?;
        self.writer.flush().await?;
        loop {
            match self.read_incoming().await? {
                Incoming::Notification(n) => self.queued.push_back(n),
                Incoming::Response(r) if r.id == Some(id) || r.id.is_none() => {
                    return match (r.result, r.error) {
                        (_, Some(e)) => Err(ClientError::Rpc(e)),
                        (Some(v), None) => Ok(v),
                        (None, None) => Ok(Value::Null),
                    };
                }
                Incoming::Response(r) => {
                    return Err(ClientError::Protocol(format!(
                        "unexpected response id {:?}",
                        r.id
                    )))
                }
            }
        }
    }

    /// The next notification (queued first, then from the pipe). No timeout: used to follow.
    pub async fn next_notification(&mut self) -> Result<Notification, ClientError> {
        if let Some(n) = self.queued.pop_front() {
            return Ok(n);
        }
        match self.read_incoming().await? {
            Incoming::Notification(n) => Ok(n),
            Incoming::Response(r) => Err(ClientError::Protocol(format!(
                "unsolicited response id {:?}",
                r.id
            ))),
        }
    }

    async fn read_incoming(&mut self) -> Result<Incoming, ClientError> {
        let line = read_line(&mut self.reader, MAX_LINE)
            .await?
            .ok_or(ClientError::Closed)?;
        Incoming::parse(&line)
            .map_err(|e| ClientError::Protocol(format!("bad line from daemon: {e}")))
    }
}
