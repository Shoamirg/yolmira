//! Tauri commands: the webview's only way to reach the daemon (ARCHITECTURE §6.5).
//! Each one validates its input and maps to one daemon method.

use crate::daemon::{Daemon, DaemonState};
use crate::errors::invalid;
use amiri_core::AmiriError;
use amiri_ipc::protocol::{
    methods, AnswerDecision, AnswerScope, PendingAsk, PolicyAnswerParams, ProjectInfo,
    SessionCreateParams, SessionInfo,
};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::{AppHandle, State};

type Res<T> = Result<T, AmiriError>;
type DaemonRef<'a> = State<'a, Arc<Daemon>>;

/// The kinds the UI offers. WSL uses the default distribution.
const SHELL_KINDS: &[&str] = &["cmd", "powershell", "pwsh", "wsl"];
/// Same bounds as the daemon's (`size_ok`); checked here for a clear error.
const MAX_SIZE: u16 = 1000;
/// One keystroke burst or paste; larger pastes are split by the frontend.
const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_ID_LEN: usize = 64;
const MAX_PROJECT_NAME: usize = 40;
/// A Windows path, generously.
const MAX_PATH_LEN: usize = 32_767;

fn valid_id(what: &str, id: &str) -> Res<()> {
    let ok = !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(invalid(what, format!("session id {id:?} is malformed")))
    }
}

/// Same rules as the daemon's (project_handlers.rs); checked here for a clear error.
fn valid_project_name(what: &str, name: &str) -> Res<()> {
    let ok = !name.is_empty()
        && name.len() <= MAX_PROJECT_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(invalid(
            what,
            format!("project name {name:?}: use letters, digits, '-' or '_'"),
        ))
    }
}

fn valid_size(what: &str, cols: u16, rows: u16) -> Res<()> {
    if (1..=MAX_SIZE).contains(&cols) && (1..=MAX_SIZE).contains(&rows) {
        Ok(())
    } else {
        Err(invalid(
            what,
            format!("terminal size {cols}x{rows} is out of range"),
        ))
    }
}

fn decode<T: serde::de::DeserializeOwned>(what: &str, v: Value) -> Res<T> {
    serde_json::from_value(v).map_err(|e| invalid(what, format!("daemon reply: {e}")))
}

#[tauri::command]
pub fn daemon_state(daemon: DaemonRef<'_>) -> DaemonState {
    daemon.state()
}

/// The banner's Start button. The supervisor connects within a second.
#[tauri::command]
pub async fn daemon_start(daemon: DaemonRef<'_>) -> Res<()> {
    daemon.start().await.map(|_| ())
}

/// A session list and the daemon process it came from.
#[derive(Debug, Serialize)]
pub struct SessionSnapshot {
    daemon_pid: u32,
    sessions: Vec<SessionInfo>,
}

/// The list carries the pid of the daemon that answered: the UI shows a session's
/// status only while that is the daemon it is connected to (never a stale RUNNING).
#[tauri::command]
pub async fn session_list(daemon: DaemonRef<'_>) -> Res<SessionSnapshot> {
    const WHAT: &str = "List shells";
    let (v, daemon_pid) = daemon
        .call_served(WHAT, methods::SESSION_LIST, json!({}))
        .await?;
    Ok(SessionSnapshot {
        daemon_pid,
        sessions: decode(WHAT, v)?,
    })
}

#[tauri::command]
pub async fn session_create(
    daemon: DaemonRef<'_>,
    kind: String,
    cols: u16,
    rows: u16,
    project: Option<String>,
) -> Res<SessionInfo> {
    const WHAT: &str = "Open a shell";
    if !SHELL_KINDS.contains(&kind.as_str()) {
        return Err(invalid(WHAT, format!("unknown shell kind {kind:?}")));
    }
    valid_size(WHAT, cols, rows)?;
    if let Some(p) = &project {
        valid_project_name(WHAT, p)?;
    }
    let params = SessionCreateParams {
        shell: json!({ "kind": kind }),
        cwd: None,
        cols,
        rows,
        project,
    };
    // Creating a shell may ask (a policy rule); it must not block the control link.
    let v = daemon
        .call_gated(WHAT, methods::SESSION_CREATE, json!(params))
        .await?;
    decode(WHAT, v)
}

/// `generation` is chosen by the frontend for this attachment; the feed's events carry
/// it, so the frontend can drop events from an attachment it has replaced.
#[tauri::command]
pub async fn session_attach(
    app: AppHandle,
    daemon: DaemonRef<'_>,
    id: String,
    generation: u32,
) -> Res<Value> {
    valid_id("Attach to the shell", &id)?;
    daemon.attach(&app, id, generation).await
}

/// Flow control: the terminal has written `credit` bytes of this attachment's output.
#[tauri::command]
pub fn session_ack(daemon: DaemonRef<'_>, id: String, generation: u32, credit: u32) -> Res<()> {
    valid_id("Acknowledge shell output", &id)?;
    daemon.ack(&id, generation, credit as usize);
    Ok(())
}

#[tauri::command]
pub fn session_detach(daemon: DaemonRef<'_>, id: String) -> Res<()> {
    valid_id("Detach from the shell", &id)?;
    daemon.detach(&id);
    Ok(())
}

/// Keystrokes are passed through and never logged (phase1-plan Slice B).
#[tauri::command]
pub async fn session_input(daemon: DaemonRef<'_>, id: String, data: String) -> Res<()> {
    const WHAT: &str = "Send input to the shell";
    valid_id(WHAT, &id)?;
    if data.len() > MAX_INPUT_BYTES {
        return Err(invalid(WHAT, format!("{} bytes in one input", data.len())));
    }
    let params = json!({ "id": id, "data": B64.encode(data.as_bytes()) });
    daemon
        .call(WHAT, methods::SESSION_INPUT, params)
        .await
        .map(|_| ())
}

#[tauri::command]
pub async fn session_resize(daemon: DaemonRef<'_>, id: String, cols: u16, rows: u16) -> Res<()> {
    const WHAT: &str = "Resize the shell";
    valid_id(WHAT, &id)?;
    valid_size(WHAT, cols, rows)?;
    let params = json!({ "id": id, "cols": cols, "rows": rows });
    daemon
        .call(WHAT, methods::SESSION_RESIZE, params)
        .await
        .map(|_| ())
}

#[tauri::command]
pub async fn session_kill(daemon: DaemonRef<'_>, id: String) -> Res<()> {
    const WHAT: &str = "Close the shell";
    valid_id(WHAT, &id)?;
    // Killing asks by default: wait on its own connection for the answer.
    daemon
        .call_gated(WHAT, methods::SESSION_KILL, json!({ "id": id }))
        .await
        .map(|_| ())
}

#[tauri::command]
pub async fn project_list(daemon: DaemonRef<'_>) -> Res<Vec<ProjectInfo>> {
    const WHAT: &str = "List projects";
    let v = daemon.call(WHAT, methods::PROJECT_LIST, json!({})).await?;
    decode(WHAT, v)
}

/// The daemon checks that the folder exists; the policy may ask first.
#[tauri::command]
pub async fn project_add(daemon: DaemonRef<'_>, name: String, path: String) -> Res<ProjectInfo> {
    const WHAT: &str = "Register the project";
    valid_project_name(WHAT, &name)?;
    if path.is_empty() || path.len() > MAX_PATH_LEN {
        return Err(invalid(WHAT, "the folder path is empty or too long"));
    }
    let v = daemon
        .call_gated(
            WHAT,
            methods::PROJECT_ADD,
            json!({ "name": name, "path": path }),
        )
        .await?;
    decode(WHAT, v)
}

#[tauri::command]
pub async fn project_remove(daemon: DaemonRef<'_>, name: String) -> Res<()> {
    const WHAT: &str = "Remove the project";
    valid_project_name(WHAT, &name)?;
    daemon
        .call_gated(WHAT, methods::PROJECT_REMOVE, json!({ "name": name }))
        .await
        .map(|_| ())
}

/// Requests waiting for an answer (the approval panel).
#[tauri::command]
pub async fn policy_pending(daemon: DaemonRef<'_>) -> Res<Vec<PendingAsk>> {
    const WHAT: &str = "List requests waiting for approval";
    let v = daemon
        .call(WHAT, methods::POLICY_PENDING, json!({}))
        .await?;
    decode(WHAT, v)
}

/// The one-time code of a waiting request, shown so a person can approve it from
/// the CLI. The daemon gives it only to this program (decisions.md 2026-10-09).
#[tauri::command]
pub async fn policy_code(daemon: DaemonRef<'_>, id: String) -> Res<String> {
    const WHAT: &str = "Show the approval code";
    valid_id(WHAT, &id)?;
    let v = daemon
        .call(WHAT, methods::POLICY_CODE, json!({ "id": id }))
        .await?;
    v["code"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| invalid(WHAT, "the daemon's reply has no code"))
}

#[tauri::command]
pub async fn policy_answer(
    daemon: DaemonRef<'_>,
    id: String,
    allow: bool,
    for_session: bool,
) -> Res<()> {
    const WHAT: &str = "Answer the request";
    valid_id(WHAT, &id)?;
    let params = PolicyAnswerParams {
        id,
        decision: if allow {
            AnswerDecision::Allow
        } else {
            AnswerDecision::Deny
        },
        scope: if allow && for_session {
            AnswerScope::Session
        } else {
            AnswerScope::Once
        },
        // The daemon recognises this program; no code needed.
        code: None,
    };
    daemon
        .call(WHAT, methods::POLICY_ANSWER, json!(params))
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_restricted_to_uuid_characters() {
        assert!(valid_id("x", "0199a6d2-7c1e-7b3a-9f00-123456789abc").is_ok());
        for bad in ["", "a b", "..\\x", "a/b", &"a".repeat(65)] {
            assert!(valid_id("x", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn project_names_match_the_daemon_rules() {
        assert!(valid_project_name("x", "web-api_2").is_ok());
        for bad in ["", "a b", "проект", "x/y", &"a".repeat(41)] {
            assert!(valid_project_name("x", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn sizes_must_be_positive_and_bounded() {
        assert!(valid_size("x", 120, 30).is_ok());
        assert!(valid_size("x", 0, 30).is_err());
        assert!(valid_size("x", 120, 1001).is_err());
    }
}
