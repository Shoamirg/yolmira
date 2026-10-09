//! Daemon protocol: newline-delimited JSON-RPC 2.0 over a named pipe.
//! The first request on every connection must be `hello` with the token.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use yolmira_core::{Event, YolmiraError};

pub const PROTOCOL_VERSION: u32 = 1;

/// `yolmirad` exit codes (locale-neutral; the launcher interprets them).
pub mod exit {
    pub const OK: u32 = 0;
    pub const USAGE: u32 = 2;
    pub const ELEVATED: u32 = 3;
    pub const ALREADY_RUNNING: u32 = 4;
    pub const STARTUP_FAILED: u32 = 5;
}

/// JSON-RPC error codes.
pub mod codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const YOLMIRA_ERROR: i64 = -32000;
    pub const UNAUTHORIZED: i64 = -32001;
    /// A policy rule or a client's answer said no (ARCHITECTURE §6.3).
    pub const POLICY_DENIED: i64 = -32002;
    /// The calling process may not do this (e.g. approve from inside an agent job).
    pub const FORBIDDEN: i64 = -32003;
}

pub mod methods {
    pub const HELLO: &str = "hello";
    pub const DAEMON_STATUS: &str = "daemon.status";
    pub const DAEMON_STOP: &str = "daemon.stop";
    pub const EVENTS_LIST: &str = "events.list";
    pub const EVENTS_SUBSCRIBE: &str = "events.subscribe";
    pub const SESSION_CREATE: &str = "session.create";
    pub const SESSION_LIST: &str = "session.list";
    pub const SESSION_ATTACH: &str = "session.attach";
    pub const SESSION_DETACH: &str = "session.detach";
    pub const SESSION_INPUT: &str = "session.input";
    pub const SESSION_RESIZE: &str = "session.resize";
    pub const SESSION_KILL: &str = "session.kill";
    pub const POLICY_PENDING: &str = "policy.pending";
    pub const POLICY_ANSWER: &str = "policy.answer";
    /// The one-time approval code of a waiting request; for the UI only.
    pub const POLICY_CODE: &str = "policy.code";
    pub const PROJECT_ADD: &str = "project.add";
    pub const PROJECT_LIST: &str = "project.list";
    pub const PROJECT_REMOVE: &str = "project.remove";
}

/// Notification methods (daemon → client).
pub mod notifications {
    pub const EVENT: &str = "event";
    /// Terminal output for an attached session (`SessionOutput`).
    pub const SESSION_OUTPUT: &str = "session.output";
    /// An attached session's shell exited (`SessionExited`).
    pub const SESSION_EXITED: &str = "session.exited";
}

/// Every method, and the event a state-changing one must record. Slice D's
/// `every_mutating_method_emits_event` test walks this table.
pub struct MethodSpec {
    pub name: &'static str,
    pub mutating: bool,
    pub event: Option<&'static str>,
    /// A mutating method deliberately not logged, and why.
    pub unlogged: Option<&'static str>,
}

const fn read(name: &'static str) -> MethodSpec {
    MethodSpec {
        name,
        mutating: false,
        event: None,
        unlogged: None,
    }
}

const fn logged(name: &'static str, event: &'static str) -> MethodSpec {
    MethodSpec {
        name,
        mutating: true,
        event: Some(event),
        unlogged: None,
    }
}

use yolmira_core::event::kinds;

pub const METHODS: &[MethodSpec] = &[
    read(methods::HELLO),
    read(methods::DAEMON_STATUS),
    logged(methods::DAEMON_STOP, kinds::DAEMON_STOPPING),
    read(methods::EVENTS_LIST),
    read(methods::EVENTS_SUBSCRIBE),
    logged(methods::SESSION_CREATE, kinds::SESSION_CREATED),
    read(methods::SESSION_LIST),
    logged(methods::SESSION_ATTACH, kinds::SESSION_ATTACHED),
    logged(methods::SESSION_DETACH, kinds::SESSION_DETACHED),
    MethodSpec {
        name: methods::SESSION_INPUT,
        mutating: true,
        event: None,
        unlogged: Some("keystrokes may contain passwords and secrets (phase1-plan Slice B)"),
    },
    logged(methods::SESSION_RESIZE, kinds::SESSION_RESIZED),
    logged(methods::SESSION_KILL, kinds::SESSION_KILL_REQUESTED),
    read(methods::POLICY_PENDING),
    logged(methods::POLICY_ANSWER, kinds::POLICY_ANSWERED),
    read(methods::POLICY_CODE),
    logged(methods::PROJECT_ADD, kinds::PROJECT_REGISTERED),
    read(methods::PROJECT_LIST),
    logged(methods::PROJECT_REMOVE, kinds::PROJECT_REMOVED),
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Request {
    pub fn new(id: u64, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Box<YolmiraError>>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn yolmira(e: YolmiraError) -> Self {
        Self {
            code: codes::YOLMIRA_ERROR,
            message: e.what.clone(),
            data: Some(Box::new(e)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    /// `None` (serialized as `null`) only when the request couldn't be parsed.
    pub id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id),
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: u64, error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id),
            result: None,
            error: Some(error),
        }
    }

    /// Error for a request whose id couldn't be read (JSON-RPC: `"id": null`).
    pub fn err_unidentified(error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: None,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Notification {
    pub fn new(method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
        }
    }
}

/// What a client reads: a response (has `id`) or a notification.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Response(Response),
    Notification(Notification),
}

impl Incoming {
    /// Notifications carry `method`; responses don't.
    pub fn parse(line: &str) -> serde_json::Result<Self> {
        let v: Value = serde_json::from_str(line)?;
        if v.get("method").is_some() {
            serde_json::from_value(v).map(Self::Notification)
        } else {
            serde_json::from_value(v).map(Self::Response)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloParams {
    pub token: String,
    pub protocol_version: u32,
    pub client: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloResult {
    pub daemon_version: String,
    pub protocol_version: u32,
    pub pid: u32,
    pub instance: Option<String>,
    /// This connection's id in the daemon. `policy.ask` events name the connection
    /// whose request waits, so a client can recognise (and prompt for) its own.
    #[serde(default)]
    pub connection: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub version: String,
    pub started_at: String,
    pub instance: Option<String>,
    pub home: String,
    pub clients: u32,
    pub last_seq: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventsListParams {
    #[serde(default)]
    pub after: i64,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub kind_prefix: Option<String>,
}

fn default_limit() -> u32 {
    1000
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventsSubscribeParams {
    /// Replay events after this `seq` first; `None` = only new events.
    #[serde(default)]
    pub after: Option<i64>,
}

/// Params of the `event` notification.
pub type EventNotification = Event;

/// Shell kinds as the protocol names them (`yolmira_exec::ShellKind` has the same shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCreateParams {
    /// `{"kind": "cmd"}`, `{"kind": "powershell"}`, `{"kind": "pwsh"}`, `{"kind": "wsl", "distro": …}`.
    pub shell: Value,
    /// Working directory; the user's profile folder if omitted. With `project`, the
    /// project's folder unless given.
    #[serde(default)]
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
    /// A registered project: the shell starts in its folder, and its rules apply.
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub shell: String,
    pub cwd: String,
    pub pid: Option<u32>,
    /// `RUNNING`, `EXITED`, `UNKNOWN` (the projection's status).
    pub status: String,
    pub exit_code: Option<i64>,
    pub attached: u32,
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub project: Option<String>,
}

/// A request waiting for a client's answer (`policy.pending`, and the `policy.ask`
/// event's payload).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingAsk {
    pub id: String,
    pub action: String,
    #[serde(default)]
    pub project: Option<String>,
    /// What exactly would happen (e.g. which shell is killed).
    pub detail: Value,
    /// The rule that asked.
    pub rule: String,
    /// The client whose request waits, and its connection (`HelloResult::connection`).
    pub client: String,
    pub connection: u64,
    /// Who asked (team mode): the person the request acts for, and the agent if one
    /// made it. The task owner answers in team mode.
    #[serde(default)]
    pub person_id: Option<String>,
    #[serde(default)]
    pub install_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    pub asked_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnswerDecision {
    Allow,
    Deny,
}

/// How far an ALLOW reaches: this request only, or the same action (and project)
/// until the daemon stops. Never "forever": permanent rules live in `policies.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AnswerScope {
    #[default]
    Once,
    Session,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyAnswerParams {
    pub id: String,
    pub decision: AnswerDecision,
    #[serde(default)]
    pub scope: AnswerScope,
    /// The one-time code the UI shows for this request. Required from any client
    /// but the UI itself.
    #[serde(default)]
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyIdParams {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAddParams {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectNameParams {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionIdParams {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAttachResult {
    pub info: SessionInfo,
    /// Size of the scrollback that follows as `session.output` notifications (the first
    /// with `reset`), before live output.
    pub replay_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInputParams {
    pub id: String,
    /// Bytes for the terminal, base64.
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionResizeParams {
    pub id: String,
    pub cols: u16,
    pub rows: u16,
}

/// Params of the `session.output` notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOutput {
    pub id: String,
    /// base64.
    pub data: String,
    /// The client fell behind: clear the screen and write `data` (the scrollback) instead.
    #[serde(default)]
    pub reset: bool,
}

/// Params of the `session.exited` notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionExited {
    pub id: String,
    pub exit_code: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incoming_tells_responses_from_notifications() {
        let r = Incoming::parse(r#"{"jsonrpc":"2.0","id":3,"result":1}"#).unwrap();
        assert!(matches!(
            r,
            Incoming::Response(Response { id: Some(3), .. })
        ));
        let n = Incoming::parse(r#"{"jsonrpc":"2.0","method":"event","params":{}}"#).unwrap();
        assert!(matches!(n, Incoming::Notification(_)));
        let e =
            Incoming::parse(r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"x"}}"#)
                .unwrap();
        assert!(matches!(e, Incoming::Response(Response { id: None, .. })));
        let json = serde_json::to_value(Response::err_unidentified(RpcError::new(
            codes::PARSE_ERROR,
            "x",
        )))
        .unwrap();
        assert!(json["id"].is_null() && json.get("id").is_some(), "{json}");
    }

    #[test]
    fn every_mutating_method_names_its_event() {
        // Each mutating method either records an event or says why it doesn't.
        assert!(METHODS
            .iter()
            .filter(|m| m.mutating)
            .all(|m| m.event.is_some() != m.unlogged.is_some()));
    }
}
