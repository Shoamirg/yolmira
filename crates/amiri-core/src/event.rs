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
    /// The person the event acts for (team mode: each AMIRI install has one). `None`
    /// only on events from before identity existed: the local person.
    #[serde(default)]
    pub person_id: Option<String>,
    /// The machine/install it came from (a person may use several).
    #[serde(default)]
    pub install_id: Option<String>,
    /// The agent that did it, if an agent did.
    #[serde(default)]
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub payload: Value,
}

/// An event before the store assigns `seq`, `id` and `ts`.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    pub kind: &'static str,
    pub actor: String,
    /// Filled with the local person and install by the daemon when left `None`.
    pub person_id: Option<String>,
    pub install_id: Option<String>,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub payload: Value,
}

impl NewEvent {
    pub fn new(kind: &'static str, actor: impl Into<String>, payload: Value) -> Self {
        Self {
            kind,
            actor: actor.into(),
            person_id: None,
            install_id: None,
            agent_id: None,
            session_id: None,
            payload,
        }
    }

    pub fn for_person(self, person_id: impl Into<String>) -> Self {
        Self {
            person_id: Some(person_id.into()),
            ..self
        }
    }

    pub fn from_install(self, install_id: impl Into<String>) -> Self {
        Self {
            install_id: Some(install_id.into()),
            ..self
        }
    }

    pub fn by_agent(self, agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: Some(agent_id.into()),
            ..self
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
    /// policies.toml or agents.toml changed on disk: the diff, and whether it loosens.
    pub const POLICY_CONFIG_CHANGED: &str = "policy.config_changed";
    /// A version of a config file is in force (first start, tightening, or confirmed),
    /// with its full content.
    pub const POLICY_CONFIG_ACCEPTED: &str = "policy.config_accepted";
    /// A person declined a loosening edit; the confirmed version stays in force.
    pub const POLICY_CONFIG_REJECTED: &str = "policy.config_rejected";
}

/// Actors (§7.2 `actor`).
pub mod actors {
    pub const AMIRI: &str = "amiri";
    pub const USER: &str = "user";
}
