//! Projections: tables derived from events, updated in the event's transaction and
//! rebuildable by replaying the log.

use crate::Result;
use amiri_core::{event::kinds, Event};
use rusqlite::{params, Connection, Transaction};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub id: String,
    pub shell_kind: Option<String>,
    pub cwd: Option<String>,
    pub status: String,
    pub exit_code: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

pub fn apply(tx: &Transaction<'_>, e: &Event) -> Result<()> {
    let Some(sid) = e.session_id.as_deref() else {
        return Ok(());
    };
    let text = |k: &str| {
        e.payload
            .get(k)
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    match e.kind.as_str() {
        kinds::SESSION_CREATED => {
            tx.execute(
                "INSERT OR REPLACE INTO sessions (id, channel, shell_kind, cwd, status, created_at, updated_at)
                 VALUES (?1, 'pty', ?2, ?3, 'RUNNING', ?4, ?4)",
                params![sid, text("shell_kind"), text("cwd"), e.ts],
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
    Ok(())
}

pub fn sessions(conn: &Connection) -> Result<Vec<SessionRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, shell_kind, cwd, status, exit_code, created_at, updated_at FROM sessions ORDER BY created_at, id",
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
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
