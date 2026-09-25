//! `amiri shell new | list | kill` (attach is in `attach.rs`).

use super::attach::{self, DEFAULT_SIZE};
use super::commands::{client_error, connect_started};
use super::console::window_size;
use super::exit;
use super::output::Out;
use amiri_core::AmiriError;
use amiri_ipc::protocol::{methods, SessionIdParams, SessionInfo};
use amiri_ipc::Instance;
use serde_json::json;

pub async fn new(
    out: &Out,
    instance: Instance,
    kind: &str,
    distro: Option<String>,
    cwd: Option<String>,
    attach_after: bool,
) -> Result<u8, AmiriError> {
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
    let params = json!({ "shell": shell, "cwd": cwd, "cols": cols, "rows": rows });
    let info: SessionInfo = c
        .call(methods::SESSION_CREATE, params)
        .await
        .map_err(client_error)?;
    out.value(
        || format!("{} {} {}", info.id, info.shell, info.cwd),
        json!(info),
    );
    if attach_after {
        return attach::attach(out, instance, info.id).await;
    }
    Ok(exit::OK)
}

pub async fn list(out: &Out, instance: Instance) -> Result<u8, AmiriError> {
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

pub async fn kill(out: &Out, instance: Instance, id: String) -> Result<u8, AmiriError> {
    let mut c = connect_started(&instance).await?;
    c.call_value(
        methods::SESSION_KILL,
        json!(SessionIdParams { id: id.clone() }),
    )
    .await
    .map_err(client_error)?;
    out.value(
        || format!("{id} ending"),
        json!({ "id": id, "state": "ending" }),
    );
    Ok(exit::OK)
}

pub use attach::attach;
