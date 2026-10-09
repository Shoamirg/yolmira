//! `yolmira shell new | list | kill` (attach is in `attach.rs`).

use super::approve::gated_call;
use super::attach::{self, DEFAULT_SIZE};
use super::commands::{client_error, connect_started};
use super::console::window_size;
use super::exit;
use super::output::Out;
use serde_json::json;
use yolmira_core::YolmiraError;
use yolmira_ipc::protocol::{methods, SessionIdParams, SessionInfo};
use yolmira_ipc::Instance;

pub async fn new(
    out: &Out,
    instance: Instance,
    kind: &str,
    distro: Option<String>,
    cwd: Option<String>,
    project: Option<String>,
    attach_after: bool,
) -> Result<u8, YolmiraError> {
    let mut c = connect_started(&instance).await?;
    let (cols, rows) = if attach_after {
        window_size().unwrap_or(DEFAULT_SIZE)
    } else {
        DEFAULT_SIZE
    };
    let shell = match distro {
        Some(d) => json!({ "kind": kind, "distro": d }),
        None => json!({ "kind": kind }),
    };
    let params =
        json!({ "shell": shell, "cwd": cwd, "cols": cols, "rows": rows, "project": project });
    let created = gated_call(&instance, &mut c, methods::SESSION_CREATE, params).await?;
    let info: SessionInfo = serde_json::from_value(created)
        .map_err(|e| YolmiraError::new("Start a shell", "cli", e.to_string()))?;
    out.value(
        || format!("{} {} {}", info.id, info.shell, info.cwd),
        json!(info),
    );
    if attach_after {
        return attach::attach(out, instance, info.id).await;
    }
    Ok(exit::OK)
}

pub async fn list(out: &Out, instance: Instance) -> Result<u8, YolmiraError> {
    let mut c = connect_started(&instance).await?;
    let all: Vec<SessionInfo> = c
        .call(methods::SESSION_LIST, json!({}))
        .await
        .map_err(client_error)?;
    if out.json {
        out.value(String::new, json!(all));
        return Ok(exit::OK);
    }
    for s in &all {
        let code = s
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "-".into());
        let line = format!(
            "{}  {:<10} {:<8} exit={:<4} attached={} {}",
            s.id, s.shell, s.status, code, s.attached, s.cwd
        );
        out.value(|| line, json!(null));
    }
    Ok(exit::OK)
}

pub async fn kill(out: &Out, instance: Instance, id: String) -> Result<u8, YolmiraError> {
    let mut c = connect_started(&instance).await?;
    let params = json!(SessionIdParams { id: id.clone() });
    gated_call(&instance, &mut c, methods::SESSION_KILL, params).await?;
    out.value(
        || format!("{id} ending"),
        json!({ "id": id, "state": "ending" }),
    );
    Ok(exit::OK)
}

pub use attach::attach;
