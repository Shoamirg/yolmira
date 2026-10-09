//! Who this AMIRI install acts for (team mode, decisions.md 2026-10-09): one person
//! per install, with a stable id, recorded on every event. Agents get their own ids
//! when they arrive (Phase 2+). Kept in `identity.toml` in the config folder.

use serde::{Deserialize, Serialize};
use std::path::Path;

const FILE: &str = "identity.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    /// UUID v7, created once; never changes (team mode matches people by it).
    pub person_id: String,
    /// Shown to people; editable in the file.
    pub display_name: String,
}

/// Read the person, or create one on first start. An unreadable file stops the daemon
/// rather than minting a new id: a new id would split one person's history in two.
pub fn load_or_create(config_dir: &Path) -> Result<Person, String> {
    let path = config_dir.join(FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let p: Person = toml::from_str(&text).map_err(|e| {
                format!(
                    "{} is not valid ({}); fix it or restore it from a backup. Deleting it gives this install a new person id",
                    path.display(),
                    e.message()
                )
            })?;
            if uuid::Uuid::parse_str(&p.person_id).is_err() {
                return Err(format!("{}: person_id is not a UUID", path.display()));
            }
            Ok(p)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let p = Person {
                person_id: uuid::Uuid::now_v7().to_string(),
                display_name: std::env::var("USERNAME").unwrap_or_else(|_| "me".into()),
            };
            std::fs::create_dir_all(config_dir)
                .map_err(|e| format!("create {}: {e}", config_dir.display()))?;
            let text = toml::to_string(&p).map_err(|e| e.to_string())?;
            std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))?;
            Ok(p)
        }
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn created_once_then_stable() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create(dir.path()).unwrap();
        assert!(uuid::Uuid::parse_str(&first.person_id).is_ok());
        assert_eq!(load_or_create(dir.path()).unwrap(), first);
    }

    #[test]
    fn a_broken_file_is_an_error_not_a_new_person() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), "person_id = 42").unwrap();
        assert!(load_or_create(dir.path()).is_err());
        std::fs::write(
            dir.path().join(FILE),
            "person_id = \"x\"\ndisplay_name = \"a\"\n",
        )
        .unwrap();
        assert!(load_or_create(dir.path()).is_err());
    }
}
