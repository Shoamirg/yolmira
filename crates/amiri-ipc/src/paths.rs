//! Where one daemon instance keeps its pipe, locks, token, database and logs.
//! The default instance serves the user; named instances isolate tests.

use crate::win::security::current_user_sid;
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    /// `None` for the user's real daemon.
    pub name: Option<String>,
    pub home: PathBuf,
    pub sid: String,
}

impl Instance {
    /// `home` defaults to `%LOCALAPPDATA%\AMIRI` (ARCHITECTURE §7.1).
    pub fn resolve(name: Option<String>, home: Option<PathBuf>) -> io::Result<Self> {
        if let Some(n) = &name {
            validate_name(n)?;
        }
        let home = match home {
            Some(h) => h,
            None => {
                let base = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is not set")
                })?;
                let root = PathBuf::from(base).join("AMIRI");
                match &name {
                    Some(n) => root.join("instances").join(n),
                    None => root,
                }
            }
        };
        Ok(Self {
            name,
            home,
            sid: current_user_sid()?,
        })
    }

    fn suffix(&self) -> String {
        self.name
            .as_deref()
            .map(|n| format!("-{n}"))
            .unwrap_or_default()
    }

    pub fn pipe_name(&self) -> String {
        format!(r"\\.\pipe\amiri-{}{}", self.sid, self.suffix())
    }

    /// Held by the daemon for its lifetime: one daemon per user and instance.
    pub fn daemon_mutex(&self) -> String {
        format!(r"Local\AMIRI-amirid-{}{}", self.sid, self.suffix())
    }

    /// Held by a launcher while it starts the daemon, so concurrent starts yield one daemon.
    pub fn start_mutex(&self) -> String {
        format!(r"Local\AMIRI-start-{}{}", self.sid, self.suffix())
    }

    pub fn token_path(&self) -> PathBuf {
        self.home.join("run").join("token")
    }

    pub fn db_path(&self) -> PathBuf {
        self.home.join("amiri.db")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.home.join("logs")
    }

    /// Arguments that make `amirid` serve this instance.
    pub fn daemon_args(&self) -> Vec<String> {
        let mut args = vec![
            "--home".to_string(),
            self.home.to_string_lossy().into_owned(),
        ];
        if let Some(n) = &self.name {
            args.extend(["--instance".to_string(), n.clone()]);
        }
        args
    }
}

fn validate_name(n: &str) -> io::Result<()> {
    let ok = !n.is_empty()
        && n.len() <= 40
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid instance name {n:?}"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_instances_get_their_own_pipe_locks_and_home() {
        let a = Instance::resolve(None, Some("C:\\a".into())).unwrap();
        let b = Instance::resolve(Some("t1".into()), Some("C:\\b".into())).unwrap();
        assert_ne!(a.pipe_name(), b.pipe_name());
        assert_ne!(a.daemon_mutex(), b.daemon_mutex());
        assert!(a.pipe_name().starts_with(r"\\.\pipe\amiri-S-1-5-"));
        assert_eq!(b.daemon_args(), vec!["--home", "C:\\b", "--instance", "t1"]);
    }

    #[test]
    fn rejects_names_that_could_escape_the_namespace() {
        for bad in ["", "a\\b", "a b", "..", &"x".repeat(41)] {
            assert!(
                Instance::resolve(Some(bad.to_string()), Some("C:\\x".into())).is_err(),
                "{bad:?}"
            );
        }
    }
}
