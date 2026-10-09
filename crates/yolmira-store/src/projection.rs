//! Projections: tables derived from events, updated in the event's transaction and
//! rebuildable by replaying the log.

use crate::Result;
use rusqlite::{params, Connection, Transaction};
use yolmira_core::{event::kinds, Event};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub id: String,
    pub shell_kind: Option<String>,
    pub project: Option<String>,
    pub cwd: Option<String>,
    pub status: String,
    pub exit_code: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRow {
    pub name: String,
    pub path: String,
    pub created_at: String,
    pub updated_at: String,
}

fn text(e: &Event, k: &str) -> Option<String> {
    e.payload
        .get(k)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

pub fn apply(tx: &Transaction<'_>, e: &Event) -> Result<()> {
    match e.kind.as_str() {
        kinds::PROJECT_REGISTERED => {
            // Registering a name again moves the project and keeps its first date.
            tx.execute(
                "INSERT INTO projects (name, path, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)
                 ON CONFLICT(name) DO UPDATE SET path = excluded.path, updated_at = excluded.updated_at",
                params![text(e, "name"), text(e, "path"), e.ts],
            )?;
            return Ok(());
        }
        kinds::PROJECT_REMOVED => {
            tx.execute(
                "DELETE FROM projects WHERE name = ?1",
                params![text(e, "name")],
            )?;
            return Ok(());
        }
        _ => {}
    }
    let Some(sid) = e.session_id.as_deref() else {
        return Ok(());
    };
    match e.kind.as_str() {
        kinds::SESSION_CREATED => {
            tx.execute(
                "INSERT OR REPLACE INTO sessions (id, channel, shell_kind, cwd, project, status, created_at, updated_at)
                 VALUES (?1, 'pty', ?2, ?3, ?4, 'RUNNING', ?5, ?5)",
                params![sid, text(e, "shell_kind"), text(e, "cwd"), text(e, "project"), e.ts],
            )?;
        }
        kinds::SESSION_EXITED => {
            let code = e.payload.get("exit_code").and_then(|v| v.as_i64());
            tx.execute(
                "UPDATE sessions SET status = 'EXITED', exit_code = ?2, updated_at = ?3 WHERE id = ?1",
                params![sid, code, e.ts],
            )?;
        }
        kinds::SESSION_UNKNOWN => {
            tx.execute(
                "UPDATE sessions SET status = 'UNKNOWN', updated_at = ?2 WHERE id = ?1",
                params![sid, e.ts],
            )?;
        }
        _ => {}
    }
    Ok(())
}

pub fn clear(tx: &Transaction<'_>) -> Result<()> {
    tx.execute("DELETE FROM sessions", [])?;
    tx.execute("DELETE FROM projects", [])?;
    Ok(())
}

pub fn projects(conn: &Connection) -> Result<Vec<ProjectRow>> {
    let mut stmt =
        conn.prepare("SELECT name, path, created_at, updated_at FROM projects ORDER BY name")?;
    let rows = stmt.query_map([], |r| {
        Ok(ProjectRow {
            name: r.get(0)?,
            path: r.get(1)?,
            created_at: r.get(2)?,
            updated_at: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn sessions(conn: &Connection) -> Result<Vec<SessionRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, shell_kind, cwd, status, exit_code, created_at, updated_at, project FROM sessions ORDER BY created_at, id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(SessionRow {
            id: r.get(0)?,
            shell_kind: r.get(1)?,
            cwd: r.get(2)?,
            status: r.get(3)?,
            exit_code: r.get(4)?,
            created_at: r.get(5)?,
            updated_at: r.get(6)?,
            project: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
