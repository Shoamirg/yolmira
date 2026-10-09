//! `amiri project add | list | remove` (phase1-plan Slice D). A project names a
//! folder; `amiri shell new --project <name>` starts there.

use super::approve::gated_call;
use super::commands::{client_error, connect_started};
use super::exit;
use super::output::Out;
use amiri_core::AmiriError;
use amiri_ipc::protocol::{methods, ProjectAddParams, ProjectInfo, ProjectNameParams};
use amiri_ipc::Instance;
use serde_json::json;

pub async fn add(
    out: &Out,
    instance: Instance,
    name: String,
    path: String,
) -> Result<u8, AmiriError> {
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
        .map_err(|e| AmiriError::new("Register the project", "cli", e.to_string()))?;
    out.value(|| format!("{} {}", p.name, p.path), v);
    Ok(exit::OK)
}

pub async fn list(out: &Out, instance: Instance) -> Result<u8, AmiriError> {
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
            || "No projects yet: `amiri project add <name> <folder>`.".into(),
            json!(null),
        );
    }
    for p in &all {
        let line = format!("{:<20} {}", p.name, p.path);
        out.value(|| line, json!(null));
    }
    Ok(exit::OK)
}

pub async fn remove(out: &Out, instance: Instance, name: String) -> Result<u8, AmiriError> {
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
