//! Who Yolmira acts for (team mode, decisions.md 2026-10-09), in two parts:
//!
//! - **person** (`identity.toml` in the config folder, `%APPDATA%\Yolmira`): one per
//!   person, created on the first install. Copy the file (or `yolmira identity import`)
//!   to another machine and that machine acts for the same person.
//! - **install** (`install.toml` in the local data folder, `%LOCALAPPDATA%\Yolmira`):
//!   one per machine and install, created automatically, never copied: a local
//!   folder doesn't roam with a profile, and import never touches it.
//!
//! Every event and approval records both, plus the agent if one acted (Phase 2+).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PERSON_FILE: &str = "identity.toml";
pub const INSTALL_FILE: &str = "install.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    /// UUID v7, created once per person; team mode matches people by it.
    pub person_id: String,
    /// Shown to people; editable in the file.
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Install {
    /// UUID v7, created once per machine and install.
    pub install_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub person: Person,
    pub install: Install,
}

impl Identity {
    /// `config_dir`: the person file's folder; `home`: the local data folder.
    pub fn load_or_create(config_dir: &Path, home: &Path) -> Result<Self, String> {
        Ok(Self {
            person: load_or_create(&config_dir.join(PERSON_FILE), new_person)?,
            install: load_or_create(&home.join(INSTALL_FILE), new_install)?,
        })
    }
}

fn new_person() -> Person {
    Person {
        person_id: uuid::Uuid::now_v7().to_string(),
        display_name: std::env::var("USERNAME").unwrap_or_else(|_| "me".into()),
    }
}

fn new_install() -> Install {
    Install {
        install_id: uuid::Uuid::now_v7().to_string(),
    }
}

trait HasId {
    fn id(&self) -> &str;
}
impl HasId for Person {
    fn id(&self) -> &str {
        &self.person_id
    }
}
impl HasId for Install {
    fn id(&self) -> &str {
        &self.install_id
    }
}

/// Parse and check one identity file.
fn read<T: for<'de> Deserialize<'de> + HasId>(path: &Path, text: &str) -> Result<T, String> {
    let v: T = toml::from_str(text).map_err(|e| {
        format!(
            "{} is not valid ({}); fix it or restore it from a backup",
            path.display(),
            e.message()
        )
    })?;
    if uuid::Uuid::parse_str(v.id()).is_err() {
        return Err(format!("{}: the id is not a UUID", path.display()));
    }
    Ok(v)
}

/// Read the file, or create it the first time. An unreadable file is an error, never
/// a new id: a new id would split one history in two.
fn load_or_create<T>(path: &Path, new: fn() -> T) -> Result<T, String>
where
    T: Serialize + for<'de> Deserialize<'de> + HasId,
{
    match std::fs::read_to_string(path) {
        Ok(text) => read(path, &text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let v = new();
            write(path, &v)?;
            Ok(v)
        }
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

fn write<T: Serialize>(path: &Path, v: &T) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let text = toml::to_string(v).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("write {}: {e}", path.display()))
}

/// What `import` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Imported {
    /// No person here yet, or the same person: now this one.
    Done(Person),
    /// A different person is already here; nothing changed (pass `replace`).
    Conflict { current: Person, incoming: Person },
}

/// Make this machine act for the person in `source` (another machine's
/// `identity.toml`). The install id is untouched. Takes effect when the daemon
/// starts next.
pub fn import(config_dir: &Path, source: &Path, replace: bool) -> Result<Imported, String> {
    let text =
        std::fs::read_to_string(source).map_err(|e| format!("read {}: {e}", source.display()))?;
    let incoming: Person = read(source, &text)?;
    let target: PathBuf = config_dir.join(PERSON_FILE);
    if let Ok(existing) = std::fs::read_to_string(&target) {
        let current: Person = read(&target, &existing)?;
        if current.person_id != incoming.person_id && !replace {
            return Ok(Imported::Conflict { current, incoming });
        }
    }
    write(&target, &incoming)?;
    Ok(Imported::Done(incoming))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn created_once_then_stable() {
        let (config, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let first = Identity::load_or_create(config.path(), home.path()).unwrap();
        assert_ne!(first.person.person_id, first.install.install_id);
        assert_eq!(
            Identity::load_or_create(config.path(), home.path()).unwrap(),
            first
        );
    }

    #[test]
    fn broken_files_are_errors_not_new_ids() {
        let (config, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(config.path().join(PERSON_FILE), "person_id = 42").unwrap();
        assert!(Identity::load_or_create(config.path(), home.path()).is_err());
        std::fs::write(
            config.path().join(PERSON_FILE),
            "person_id = \"x\"\ndisplay_name = \"a\"\n",
        )
        .unwrap();
        assert!(Identity::load_or_create(config.path(), home.path()).is_err());
        let config2 = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(INSTALL_FILE), "install_id = \"nope\"\n").unwrap();
        assert!(Identity::load_or_create(config2.path(), home.path()).is_err());
    }

    #[test]
    fn import_moves_the_person_not_the_install() {
        let (config_a, home_a) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (config_b, home_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = Identity::load_or_create(config_a.path(), home_a.path()).unwrap();
        let b = Identity::load_or_create(config_b.path(), home_b.path()).unwrap();
        let source = config_a.path().join(PERSON_FILE);
        // B already has a different person: nothing changes without `replace`.
        assert!(matches!(
            import(config_b.path(), &source, false).unwrap(),
            Imported::Conflict { .. }
        ));
        assert_eq!(
            import(config_b.path(), &source, true).unwrap(),
            Imported::Done(a.person.clone())
        );
        let b2 = Identity::load_or_create(config_b.path(), home_b.path()).unwrap();
        assert_eq!(b2.person, a.person);
        assert_eq!(b2.install, b.install, "the install id stays this machine's");
        assert_ne!(b2.install, a.install);
    }
}
