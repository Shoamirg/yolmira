//! `project.*` methods: the project registry (phase1-plan Slice D). A project is a
//! name for a folder; shells can start in it, and `[project.<name>]` rules apply to
//! what happens there. Registering and removing go through the policy gate.

use super::policy::{self, Check};
use super::server::Conn;
use super::sessions::validate_cwd;
use super::state::State;
use amiri_core::event::kinds;
use amiri_core::policy::actions;
use amiri_core::{AmiriError, NewEvent};
use amiri_ipc::protocol::{codes, ProjectAddParams, ProjectInfo, ProjectNameParams, RpcError};
use serde_json::{json, Value};
use std::sync::Arc;

const WHERE: &str = "amirid projects";
const MAX_NAME: usize = 40;

/// Names end up in `policies.toml` section headers and on the command line: keep
/// them plain.
fn valid_name(name: &str) -> Result<(), RpcError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(RpcError::new(
            codes::INVALID_PARAMS,
            format!("project name {name:?}: use 1-{MAX_NAME} letters, digits, '-' or '_'"),
        ))
    }
}

pub async fn add(state: &Arc<State>, p: ProjectAddParams, conn: &Conn) -> Result<Value, RpcError> {
    valid_name(&p.name)?;
    let path = validate_cwd(&p.path).map_err(|e| {
        RpcError::amiri(AmiriError {
            what: "Register the project".into(),
            ..e
        })
    })?;
    policy::check(
        state,
        Check {
            action: actions::PROJECT_REGISTER,
            project: Some(p.name.clone()),
            detail: json!({ "name": p.name, "path": path }),
            client: &conn.client,
            connection: conn.id,
        },
    )
    .await?;
    let event = NewEvent::new(
        kinds::PROJECT_REGISTERED,
        format!("client:{}", conn.client),
        json!({ "name": p.name, "path": path }),
    );
    state.record(event).map_err(RpcError::amiri)?;
    Ok(json!(ProjectInfo { name: p.name, path }))
}

pub fn list(state: &State) -> Result<Value, RpcError> {
    let projects: Vec<ProjectInfo> = state
        .with_store(|s| s.projects())
        .map_err(RpcError::amiri)?
        .into_iter()
        .map(|r| ProjectInfo {
            name: r.name,
            path: r.path,
        })
        .collect();
    Ok(json!(projects))
}

pub async fn remove(
    state: &Arc<State>,
    p: ProjectNameParams,
    conn: &Conn,
) -> Result<Value, RpcError> {
    valid_name(&p.name)?;
    let known = state
        .with_store(|s| s.project(&p.name))
        .map_err(RpcError::amiri)?;
    let Some(project) = known else {
        return Err(RpcError::amiri(
            AmiriError::new(
                "Remove the project",
                WHERE,
                format!("no project named {:?}", p.name),
            )
            .next_step("List projects with `amiri project list`."),
        ));
    };
    policy::check(
        state,
        Check {
            action: actions::PROJECT_REMOVE,
            project: Some(p.name.clone()),
            detail: json!({ "name": p.name, "path": project.path }),
            client: &conn.client,
            connection: conn.id,
        },
    )
    .await?;
    let event = NewEvent::new(
        kinds::PROJECT_REMOVED,
        format!("client:{}", conn.client),
        json!({ "name": p.name }),
    );
    state.record(event).map_err(RpcError::amiri)?;
    Ok(json!({ "removed": p.name }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_names_are_plain() {
        assert!(valid_name("web-api_2").is_ok());
        for bad in ["", "a b", "x/y", "проект", &"a".repeat(41), "]"] {
            assert!(valid_name(bad).is_err(), "{bad:?}");
        }
    }
}
