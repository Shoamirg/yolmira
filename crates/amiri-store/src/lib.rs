//! SQLite store: append-only events plus projections updated in the same transaction
//! (ARCHITECTURE D4, §7.2). Callers serialize access; `Connection` is not `Sync`.

mod migrate;
mod projection;

use amiri_core::{time::now_rfc3339, Event, NewEvent};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

pub use migrate::{MigrationReport, LATEST_VERSION};
pub use projection::{ProjectRow, SessionRow};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("event payload is not valid JSON: {0}")]
    Payload(#[from] serde_json::Error),
    #[error("backup before migration failed: {0}")]
    Backup(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

pub struct Store {
    conn: Connection,
    path: PathBuf,
    migration: MigrationReport,
}

impl Store {
    /// Open (or create) the database, back it up if migrations are pending, and migrate.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let migration = migrate::run(&conn, path)?;
        Ok(Self {
            conn,
            path: path.to_path_buf(),
            migration,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn migration_report(&self) -> &MigrationReport {
        &self.migration
    }

    /// Append one event and apply it to the projections, atomically.
    pub fn append(&mut self, new: NewEvent) -> Result<Event> {
        let event = Event {
            seq: 0,
            id: uuid::Uuid::now_v7().to_string(),
            ts: now_rfc3339(),
            kind: new.kind.to_string(),
            actor: new.actor,
            person_id: new.person_id,
            agent_id: new.agent_id,
            session_id: new.session_id,
            payload: new.payload,
        };
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO events (id, ts, kind, actor, person_id, agent_id, session_id, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                event.id,
                event.ts,
                event.kind,
                event.actor,
                event.person_id,
                event.agent_id,
                event.session_id,
                serde_json::to_string(&event.payload)?
            ],
        )?;
        let event = Event {
            seq: tx.last_insert_rowid(),
            ..event
        };
        projection::apply(&tx, &event)?;
        tx.commit()?;
        Ok(event)
    }

    /// Events with `seq > after`, oldest first. `kind_prefix` filters e.g. `"daemon."`.
    pub fn list(&self, after: i64, limit: u32, kind_prefix: Option<&str>) -> Result<Vec<Event>> {
        // Exact prefix match: LIKE would treat `_` and `%` in the prefix as wildcards.
        let prefix = kind_prefix.unwrap_or("");
        let mut stmt = self.conn.prepare(
            "SELECT seq, id, ts, kind, actor, session_id, payload, person_id, agent_id FROM events
             WHERE seq > ?1 AND substr(kind, 1, length(?2)) = ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![after, prefix, limit], RawEvent::from_row)?;
        rows.map(|r| r.map_err(StoreError::from).and_then(RawEvent::into_event))
            .collect()
    }

    pub fn last_seq(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |r| r.get(0))?)
    }

    pub fn sessions(&self) -> Result<Vec<SessionRow>> {
        projection::sessions(&self.conn)
    }

    pub fn session(&self, id: &str) -> Result<Option<SessionRow>> {
        Ok(projection::sessions(&self.conn)?
            .into_iter()
            .find(|s| s.id == id))
    }

    pub fn projects(&self) -> Result<Vec<ProjectRow>> {
        projection::projects(&self.conn)
    }

    pub fn project(&self, name: &str) -> Result<Option<ProjectRow>> {
        Ok(projection::projects(&self.conn)?
            .into_iter()
            .find(|p| p.name == name))
    }

    /// Drop the projections and replay every event into them.
    pub fn rebuild_projections(&mut self) -> Result<()> {
        let events = self.list(0, u32::MAX, None)?;
        let tx = self.conn.transaction()?;
        projection::clear(&tx)?;
        for event in &events {
            projection::apply(&tx, event)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// `PRAGMA integrity_check` result; `"ok"` when healthy.
    pub fn integrity_check(&self) -> Result<String> {
        Ok(self
            .conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))?)
    }
}

/// A row as stored; the payload is parsed separately so a bad payload is a `Payload` error.
struct RawEvent {
    seq: i64,
    id: String,
    ts: String,
    kind: String,
    actor: String,
    session_id: Option<String>,
    payload: String,
    person_id: Option<String>,
    agent_id: Option<String>,
}

impl RawEvent {
    fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            seq: r.get(0)?,
            id: r.get(1)?,
            ts: r.get(2)?,
            kind: r.get(3)?,
            actor: r.get(4)?,
            session_id: r.get(5)?,
            payload: r.get(6)?,
            person_id: r.get(7)?,
            agent_id: r.get(8)?,
        })
    }

    fn into_event(self) -> Result<Event> {
        let payload = serde_json::from_str(&self.payload)?;
        Ok(Event {
            seq: self.seq,
            id: self.id,
            ts: self.ts,
            kind: self.kind,
            actor: self.actor,
            person_id: self.person_id,
            agent_id: self.agent_id,
            session_id: self.session_id,
            payload,
        })
    }
}

#[cfg(test)]
mod tests;
