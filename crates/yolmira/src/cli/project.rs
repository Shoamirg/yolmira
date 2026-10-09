//! `yolmira project add | list | remove` (phase1-plan Slice D). A project names a
//! folder; `yolmira shell new --project <name>` starts there.

use super::approve::gated_call;
use super::commands::{client_error, connect_started};
use super::exit;
use super::output::Out;
use serde_json::json;
use yolmira_core::YolmiraError;
use yolmira_ipc::protocol::{methods, ProjectAddParams, ProjectInfo, ProjectNameParams};
use yolmira_ipc::Instance;

pub async fn add(
    out: &Out,
    instance: Instance,
    name: String,
    path: String,
) -> Result<u8, YolmiraError> {
    // The daemon checks the folder; an absolute path keeps "." meaning this folder.
    let path = std::path::absolute(&path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or(path);
    let mut c = connect_started(&instance).await?;
    let v = gated_call(
        &instance,
        &mut c,
        methods::PROJECT_ADD,
        json!(ProjectAddParams { name, path }),
    )
    .await?;
    let p: ProjectInfo = serde_json::from_value(v.clone())
        .map_err(|e| YolmiraError::new("Register the project", "cli", e.to_string()))?;
    out.value(|| format!("{} {}", p.name, p.path), v);
    Ok(exit::OK)
}

pub async fn list(out: &Out, instance: Instance) -> Result<u8, YolmiraError> {
    let mut c = connect_started(&instance).await?;
    let all: Vec<ProjectInfo> = c
        .call(methods::PROJECT_LIST, json!({}))
        .await
        .map_err(client_error)?;
    if out.json {
        out.value(String::new, json!(all));
        return Ok(exit::OK);
    }
    if all.is_empty() {
        out.value(
            || "No projects yet: `yolmira project add <name> <folder>`.".into(),
            json!(null),
        );
    }
    for p in &all {
        let line = format!("{:<20} {}", p.name, p.path);
        out.value(|| line, json!(null));
    }
    Ok(exit::OK)
}

pub async fn remove(out: &Out, instance: Instance, name: String) -> Result<u8, YolmiraError> {
    let mut c = connect_started(&instance).await?;
    gated_call(
        &instance,
        &mut c,
        methods::PROJECT_REMOVE,
        json!(ProjectNameParams { name: name.clone() }),
    )
    .await?;
    out.value(
        || format!("{name} removed (its folder is untouched)"),
        json!({ "removed": name }),
    );
    Ok(exit::OK)
}
