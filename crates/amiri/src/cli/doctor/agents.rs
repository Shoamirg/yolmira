//! The agent CLIs `amiri doctor` checks, as data (`agents.toml`): built in, and
//! extended or overridden by the user's `agents.toml` in the config folder.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const BUILT_IN: &str = include_str!("agents.toml");
const USER_FILE: &str = "agents.toml";
/// A version query that takes longer is reported as not answering.
const VERSION_LIMIT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Missing {
    Warn,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AgentSpec {
    pub name: String,
    pub command: String,
    #[serde(default = "default_version_args")]
    pub version_args: Vec<String>,
    #[serde(default)]
    pub tested: Option<Range>,
    pub missing: Missing,
}

fn default_version_args() -> Vec<String> {
    vec!["--version".into()]
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    agent: Vec<AgentSpec>,
}

pub type Version = (u64, u64, u64);

/// Tested versions: `"x.y.z"` or `"x.y.z..x.y.z"` (inclusive).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Range {
    text: String,
    min: Version,
    max: Version,
}

impl TryFrom<String> for Range {
    type Error = String;
    fn try_from(text: String) -> Result<Self, String> {
        let (a, b) = text.split_once("..").unwrap_or((&text, &text));
        let parse = |s: &str| {
            parse_version(s).ok_or(format!("{text:?} is not \"x.y.z\" or \"x.y.z..x.y.z\""))
        };
        let (min, max) = (parse(a)?, parse(b)?);
        if min > max {
            return Err(format!("{text:?}: the range is backwards"));
        }
        Ok(Self { text, min, max })
    }
}

impl Range {
    pub fn contains(&self, v: &str) -> bool {
        parse_version(v).is_some_and(|v| self.min <= v && v <= self.max)
    }
}

impl std::fmt::Display for Range {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// The first `x.y.z` in `s` (digits only, so "2.1.281 (Claude Code)" gives 2.1.281).
pub fn parse_version(s: &str) -> Option<Version> {
    s.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find_map(|tok| {
            let mut parts = tok.split('.').filter(|p| !p.is_empty());
            let v = (
                parts.next()?.parse().ok()?,
                parts.next()?.parse().ok()?,
                parts.next()?.parse().ok()?,
            );
            Some(v)
        })
}

fn format_version(v: Version) -> String {
    format!("{}.{}.{}", v.0, v.1, v.2)
}

/// The built-in list with the user's entries applied (same name replaces).
pub fn load(config_dir: &Path) -> Result<Vec<AgentSpec>, String> {
    let parse = |text: &str, from: &str| {
        toml::from_str::<File>(text)
            .map(|f| f.agent)
            .map_err(|e| format!("{from}: {}", e.message()))
    };
    let mut list = parse(BUILT_IN, "built-in agents.toml")?;
    let user = config_dir.join(USER_FILE);
    if let Ok(text) = std::fs::read_to_string(&user) {
        for spec in parse(&text, &user.display().to_string())? {
            match list.iter_mut().find(|s| s.name == spec.name) {
                Some(existing) => *existing = spec,
                None => list.push(spec),
            }
        }
    }
    Ok(list)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    NotFound,
    Found {
        path: String,
        version: Option<String>,
        /// The first line of its output, for a report when no version is found.
        output: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFact {
    pub spec: AgentSpec,
    pub probe: Probe,
}

/// Find the command on `path_var` (the PATH agents get) or as an absolute path.
pub fn locate(command: &str, path_var: &str) -> Option<PathBuf> {
    let direct = Path::new(command);
    if direct.is_absolute() {
        return direct.is_file().then(|| direct.to_path_buf());
    }
    let exts = ["", ".exe", ".cmd", ".bat", ".com"];
    path_var
        .split(';')
        .filter(|d| !d.trim().is_empty())
        .flat_map(|dir| {
            exts.iter()
                .map(move |e| Path::new(dir.trim()).join(format!("{command}{e}")))
        })
        .find(|p| p.is_file() && p.extension().is_some())
}

/// Run its version query with the agents' environment, within `VERSION_LIMIT`.
pub fn probe(spec: &AgentSpec, path_var: &str, env: &[(String, String)]) -> Probe {
    let Some(path) = locate(&spec.command, path_var) else {
        return Probe::NotFound;
    };
    let output = run_bounded(&path, &spec.version_args, env).unwrap_or_else(|e| e);
    Probe::Found {
        path: path.display().to_string(),
        version: parse_version(&output).map(format_version),
        output: output
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(200)
            .collect(),
    }
}

fn run_bounded(exe: &Path, args: &[String], env: &[(String, String)]) -> Result<String, String> {
    let mut child = Command::new(exe)
        .args(args)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't start it: {e}"))?;
    let (tx, rx) = std::sync::mpsc::channel();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut text = String::new();
        if let Some(mut s) = stdout {
            let _ = s.read_to_string(&mut text);
        }
        if let Some(mut s) = stderr {
            let _ = s.read_to_string(&mut text);
        }
        let _ = tx.send(text);
    });
    match rx.recv_timeout(VERSION_LIMIT) {
        Ok(text) => {
            let _ = child.wait();
            Ok(text)
        }
        Err(_) => {
            let _ = child.kill();
            Err(format!("no answer within {} s", VERSION_LIMIT.as_secs()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_from_typical_output() {
        assert_eq!(parse_version("2.1.281 (Claude Code)"), Some((2, 1, 281)));
        assert_eq!(parse_version("codex-cli 0.48.0"), Some((0, 48, 0)));
        assert_eq!(parse_version("no version here 1.2"), None);
    }

    #[test]
    fn ranges_hold_one_version_or_a_span() {
        let one = Range::try_from("2.1.281".to_string()).unwrap();
        assert!(one.contains("2.1.281") && !one.contains("2.1.282"));
        let span = Range::try_from("1.0.0..1.4.9".to_string()).unwrap();
        assert!(span.contains("1.2.3") && !span.contains("1.5.0") && !span.contains("0.9.9"));
        assert!(Range::try_from("2.0.0..1.0.0".to_string()).is_err());
        assert!(Range::try_from("latest".to_string()).is_err());
    }

    #[test]
    fn the_built_in_list_has_claude_and_codex_and_a_user_file_overrides_and_adds() {
        let dir = tempfile::tempdir().unwrap();
        let names = |l: &[AgentSpec]| l.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&load(dir.path()).unwrap()), ["claude", "codex"]);
        std::fs::write(
            dir.path().join(USER_FILE),
            "[[agent]]\nname = \"claude\"\ncommand = \"claude\"\ntested = \"2.2.0\"\nmissing = \"info\"\n\n[[agent]]\nname = \"other\"\ncommand = \"other\"\nmissing = \"info\"\n",
        )
        .unwrap();
        let list = load(dir.path()).unwrap();
        assert_eq!(names(&list), ["claude", "codex", "other"]);
        assert_eq!(list[0].tested.as_ref().unwrap().to_string(), "2.2.0");
        assert_eq!(list[0].missing, Missing::Info);
    }

    #[test]
    fn a_broken_user_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(USER_FILE), "[[agent]]\nname = 1\n").unwrap();
        assert!(load(dir.path()).is_err());
    }
}
