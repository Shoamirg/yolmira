//! Numbered SQL migrations embedded in the binary. The database is backed up before
//! any pending migration runs on an existing database (ARCHITECTURE §7.2).

use crate::{Result, StoreError};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/0001_events.sql")),
    (2, include_str!("../migrations/0002_sessions.sql")),
    (3, include_str!("../migrations/0003_projects.sql")),
    (4, include_str!("../migrations/0004_identity.sql")),
    (5, include_str!("../migrations/0005_install.sql")),
];

pub const LATEST_VERSION: i64 = MIGRATIONS[MIGRATIONS.len() - 1].0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub from_version: i64,
    pub to_version: i64,
    pub backup: Option<PathBuf>,
}

pub fn run(conn: &Connection, db_path: &Path) -> Result<MigrationReport> {
    let from: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let pending: Vec<_> = MIGRATIONS.iter().filter(|(v, _)| *v > from).collect();
    let backup = if from > 0 && !pending.is_empty() {
        Some(backup(conn, db_path, from)?)
    } else {
        None
    };
    for (version, sql) in pending {
        conn.execute_batch(&format!(
            "BEGIN; {sql}\nPRAGMA user_version = {version}; COMMIT;"
        ))?;
    }
    let to = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    Ok(MigrationReport {
        from_version: from,
        to_version: to,
        backup,
    })
}

fn backup(conn: &Connection, db_path: &Path, version: i64) -> Result<PathBuf> {
    let name = format!(
        "{}.v{version}.bak",
        db_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "yolmira.db".into())
    );
    let target = db_path.with_file_name(name);
    if target.exists() {
        std::fs::remove_file(&target).map_err(|e| StoreError::Backup(e.to_string()))?;
    }
    conn.execute("VACUUM INTO ?1", [target.to_string_lossy()])?;
    Ok(target)
}

#[cfg(test)]
pub(crate) fn apply_up_to(conn: &Connection, version: i64) {
    for (v, sql) in MIGRATIONS.iter().filter(|(v, _)| *v <= version) {
        conn.execute_batch(&format!("BEGIN; {sql}\nPRAGMA user_version = {v}; COMMIT;"))
            .unwrap();
    }
}
