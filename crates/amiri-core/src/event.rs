//! Events: the append-only source of truth (ARCHITECTURE D4, §7.2).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A stored event. `seq` is the global order; `id` is a UUID v7.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub seq: i64,
    pub id: String,
    pub ts: String,
    pub kind: String,
    pub actor: String,
    pub session_id: Option<String>,
    pub payload: Value,
}

/// An event before the store assigns `seq`, `id` and `ts`.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    pub kind: &'static str,
    pub actor: String,
    pub session_id: Option<String>,
    pub payload: Value,
}

impl NewEvent {
    pub fn new(kind: &'static str, actor: impl Into<String>, payload: Value) -> Self {
        Self {
            kind,
            actor: actor.into(),
            session_id: None,
            payload,
        }
    }

    pub fn for_session(self, session_id: impl Into<String>) -> Self {
        Self {
            session_id: Some(session_id.into()),
            ..self
        }
    }
}

/// Event kinds. One constant per kind so a typo is a compile error, not a silent new kind.
pub mod kinds {
    pub const DAEMON_STARTED: &str = "daemon.started";
    pub const DAEMON_STOPPING: &str = "daemon.stopping";
    pub const DAEMON_STOPPED: &str = "daemon.stopped";
    pub const IPC_AUTH_FAILED: &str = "ipc.auth_failed";
    pub const SESSION_CREATED: &str = "session.created";
    pub const SESSION_EXITED: &str = "session.exited";
    pub const SESSION_UNKNOWN: &str = "session.unknown";
    pub const SESSION_ATTACHED: &str = "session.attached";
    pub const SESSION_DETACHED: &str = "session.detached";
    pub const SESSION_RESIZED: &str = "session.resized";
    pub const SESSION_KILL_REQUESTED: &str = "session.kill_requested";
    pub const PROJECT_REGISTERED: &str = "project.registered";
    pub const PROJECT_REMOVED: &str = "project.removed";
    /// A request waits for a client's answer (ASK).
    pub const POLICY_ASK: &str = "policy.ask";
    /// A client answered a `policy.ask`.
    pub const POLICY_ANSWERED: &str = "policy.answered";
    /// An answer (or a request for an approval code) was refused: from inside an
    /// agent job, a wrong code, or not the UI.
    pub const POLICY_ANSWER_REFUSED: &str = "policy.answer_refused";
    /// Every policy verdict, automatic or answered (ARCHITECTURE §6.3).
    pub const POLICY_DECISION: &str = "policy.decision";
}

/// Actors (§7.2 `actor`).
pub mod actors {
    pub const AMIRI: &str = "amiri";
    pub const USER: &str = "user";
}
