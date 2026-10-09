//! Finding agent CLIs and asking their version, for `yolmira doctor`. Which CLIs, and
//! the tested versions, are data: `crate::agents_config`.

pub use crate::agents_config::{parse_version, AgentSpec, Missing, Range, Version};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// A version query that takes longer is reported as not answering.
const VERSION_LIMIT: Duration = Duration::from_secs(15);

fn format_version(v: Version) -> String {
    format!("{}.{}.{}", v.0, v.1, v.2)
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
