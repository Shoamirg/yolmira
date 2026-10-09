//! `yolmira doctor` v1 (phase1-plan Slice D): reports silent failure modes (ARCHITECTURE
//! §14), each with why and what to do. `gather` reads the machine; every check is a
//! pure function of the gathered `Facts`, so tests can inject broken machines.

pub mod agents;
mod gather;
mod legacy;
mod machine;

use crate::config_files::{evaluate, ConfigFile};
use agents::{AgentFact, Probe};
use serde::Serialize;
use yolmira_ipc::win::env::EnvSource;
use yolmira_store::{Inspection, LATEST_VERSION};

pub use gather::gather;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
    pub next_steps: Vec<String>,
}

impl Check {
    fn new(
        name: impl Into<String>,
        status: Status,
        detail: impl Into<String>,
        next: &[&str],
    ) -> Self {
        Self {
            name: name.into(),
            status,
            detail: detail.into(),
            next_steps: next.iter().map(|s| s.to_string()).collect(),
        }
    }
    pub fn ok(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(name, Status::Ok, detail, &[])
    }
    pub fn warn(name: impl Into<String>, detail: impl Into<String>, next: &[&str]) -> Self {
        Self::new(name, Status::Warn, detail, next)
    }
    pub fn fail(name: impl Into<String>, detail: impl Into<String>, next: &[&str]) -> Self {
        Self::new(name, Status::Fail, detail, next)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonFact {
    NotRunning,
    Running {
        version: String,
        pid: u32,
        /// The pipe's DACL is this user's only (`None`: couldn't be read).
        pipe_acl_ok: Option<bool>,
        /// Sessions the daemon itself reports RUNNING.
        live_running: Vec<String>,
    },
    /// Something else owns the daemon's pipe name.
    Impostor(String),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbFact {
    Missing,
    Inspected(Inspection),
    Unreadable(String),
}

/// A guarded config file: its confirmed and current text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFact {
    pub file: ConfigFile,
    pub confirmed: Option<String>,
    pub current: Option<String>,
    pub unreadable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub env: EnvSource,
    pub daemon: DaemonFact,
    pub db: DbFact,
    pub elevated: Option<bool>,
    /// HKLM `...\Policies\System\EnableLUA`; `None` when absent (UAC on by default).
    pub enable_lua: Option<u32>,
    pub agents: Result<Vec<AgentFact>, String>,
    pub config: Vec<ConfigFact>,
    /// The WebView2 Runtime's version, if installed.
    pub webview2: Option<String>,
    /// Folders left by the former name AMIRI (user's own daemon only).
    pub legacy: Vec<legacy::LegacyFact>,
}

pub fn checks(f: &Facts) -> Vec<Check> {
    let mut out = machine::checks(&f.env);
    out.extend(daemon(&f.daemon));
    out.push(database(&f.db));
    out.push(stale_sessions(&f.daemon, &f.db));
    out.push(elevation(f.elevated));
    out.push(uac(f.enable_lua));
    out.extend(f.config.iter().map(config_check));
    out.extend(agent_checks(&f.agents));
    out.push(webview2(f.webview2.as_deref()));
    out.extend(f.legacy.iter().map(legacy::check));
    out
}

fn daemon(d: &DaemonFact) -> Vec<Check> {
    match d {
        DaemonFact::NotRunning => vec![Check::ok("daemon", "not running (it starts when needed)")],
        DaemonFact::Running {
            version,
            pid,
            pipe_acl_ok,
            ..
        } => vec![
            Check::ok("daemon", format!("yolmirad {version}, pid {pid}")),
            match pipe_acl_ok {
                Some(true) => Check::ok("pipe_acl", "only this user can open the daemon's pipe"),
                Some(false) => Check::fail(
                    "pipe_acl",
                    "the daemon's pipe lets other users or processes in",
                    &["Stop the daemon (`yolmira daemon stop`) and start it again; report it if this persists."],
                ),
                None => Check::warn(
                    "pipe_acl",
                    "the pipe's permissions couldn't be read",
                    &["Run `yolmira doctor` again."],
                ),
            },
        ],
        DaemonFact::Impostor(why) => vec![Check::fail(
            "daemon",
            format!("the daemon's pipe belongs to another process: {why}"),
            &["Find and end the process that owns the pipe (it isn't Yolmira), then start Yolmira again."],
        )],
        DaemonFact::Error(why) => vec![Check::fail(
            "daemon",
            why.clone(),
            &["Run `yolmira daemon stop`, then `yolmira daemon start`; see the daemon log in the logs folder."],
        )],
    }
}

const DB_RESTORE: &str = "Stop the daemon, then move yolmira.db aside and rename the newest yolmira.db.vN.bak next to it to yolmira.db; with no backup, moving it aside starts a new, empty log.";

fn database(db: &DbFact) -> Check {
    match db {
        DbFact::Missing => Check::ok("database", "no database yet (created on first start)"),
        DbFact::Unreadable(why) => Check::fail("database", format!("can't read it: {why}"), &[DB_RESTORE]),
        DbFact::Inspected(i) if i.integrity != "ok" => Check::fail(
            "database",
            format!("integrity check failed: {}", i.integrity),
            &[DB_RESTORE],
        ),
        DbFact::Inspected(i) if i.version > LATEST_VERSION => Check::fail(
            "database",
            format!("schema {} was written by a newer Yolmira (this one knows {LATEST_VERSION})", i.version),
            &["Use the newer Yolmira, or restore a backup made by this version."],
        ),
        DbFact::Inspected(i) if i.version < LATEST_VERSION => Check::ok(
            "database",
            format!("ok, schema {}; migrates to {LATEST_VERSION} on the next start (a backup is made first)", i.version),
        ),
        DbFact::Inspected(i) => Check::ok("database", format!("ok, schema {}", i.version)),
    }
}

fn stale_sessions(d: &DaemonFact, db: &DbFact) -> Check {
    let DbFact::Inspected(i) = db else {
        return Check::ok("sessions_stale", "no sessions recorded");
    };
    match d {
        DaemonFact::Running { live_running, .. } => {
            let stale: Vec<_> = i.running.iter().filter(|id| !live_running.contains(id)).collect();
            if stale.is_empty() {
                return Check::ok("sessions_stale", "every session recorded RUNNING is running");
            }
            Check::warn(
                "sessions_stale",
                format!("{} session(s) recorded RUNNING aren't running: {stale:?}", stale.len()),
                &["Restart the daemon (`yolmira daemon stop`, then `yolmira daemon start`): it marks them ended."],
            )
        }
        _ if i.running.is_empty() => Check::ok("sessions_stale", "no session recorded RUNNING"),
        _ => Check::warn(
            "sessions_stale",
            format!(
                "{} session(s) recorded RUNNING while the daemon is down (it stopped without ending them)",
                i.running.len()
            ),
            &["Start the daemon (`yolmira daemon start`): it marks them ended. Their shells are gone."],
        ),
    }
}

fn elevation(elevated: Option<bool>) -> Check {
    match elevated {
        Some(false) => Check::ok("elevation", "not elevated"),
        Some(true) => Check::warn(
            "elevation",
            "running as administrator: yolmirad refuses to start elevated without --allow-elevated (ARCHITECTURE §6.5)",
            &["Run Yolmira from a normal (not administrator) window."],
        ),
        None => Check::warn("elevation", "couldn't read this process's token", &["Run `yolmira doctor` again."]),
    }
}

fn uac(enable_lua: Option<u32>) -> Check {
    match enable_lua {
        Some(0) => Check::fail(
            "uac",
            "UAC is off (EnableLUA=0): every program, agents included, runs with full administrator rights",
            &[
                r#"In an administrator window: reg add "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System" /v EnableLUA /t REG_DWORD /d 1 /f"#,
                "Then restart Windows.",
            ],
        ),
        _ => Check::ok("uac", "UAC is on"),
    }
}

fn agent_checks(agents: &Result<Vec<AgentFact>, String>) -> Vec<Check> {
    match agents {
        Err(e) => vec![Check::fail(
            "agents_config",
            e.clone(),
            &["Fix agents.toml in the config folder, or delete it to use the built-in list."],
        )],
        Ok(list) => list.iter().map(agent).collect(),
    }
}

fn agent(a: &AgentFact) -> Check {
    let name = format!("agent_{}", a.spec.name);
    match &a.probe {
        Probe::NotFound if a.spec.missing == agents::Missing::Info => {
            Check::ok(name, format!("{} not installed (optional)", a.spec.command))
        }
        Probe::NotFound => Check::warn(
            name,
            format!("{} isn't on the PATH Yolmira gives agents", a.spec.command),
            &["Install it, or add its folder to your user PATH, then restart Yolmira."],
        ),
        Probe::Found { path, version: None, output } => Check::warn(
            name,
            format!("found at {path}, but no version in its output: {output:?}"),
            &["Check that it runs: `<it> --version`."],
        ),
        Probe::Found { path, version: Some(v), .. } => match &a.spec.tested {
            Some(range) if !range.contains(v) => Check::warn(
                name,
                format!("{v} at {path}; Yolmira is tested with {range} (risk R4: its output format may differ)"),
                &["Expect differences; report problems. The tested range is in agents.toml."],
            ),
            _ => Check::ok(name, format!("{v} at {path}")),
        },
    }
}

/// policies.toml / agents.toml: an unconfirmed loosening edit doesn't apply.
fn config_check(c: &ConfigFact) -> Check {
    let name = format!("config_{}", c.file.file_name().trim_end_matches(".toml"));
    if let Some(e) = &c.unreadable {
        return Check::fail(
            name,
            format!("can't read it: {e}"),
            &["Check the file's permissions."],
        );
    }
    if c.confirmed == c.current {
        return Check::ok(name, "the confirmed version is in force");
    }
    let e = evaluate(c.file, c.confirmed.as_deref(), c.current.as_deref());
    if let Some(why) = e.invalid {
        return Check::fail(
            name,
            format!("{} doesn't parse ({why}); Yolmira applies the confirmed version, failing closed", c.file.file_name()),
            &["Fix the file, or restore the confirmed version (`yolmira events --kind policy.config_accepted` shows it)."],
        );
    }
    if e.loosenings.is_empty() {
        return Check::ok(name, "an edit that only tightens; it applies");
    }
    Check::warn(
        name,
        format!(
            "an unconfirmed edit loosens: {}. Until a person confirms it in the Yolmira window, it doesn't apply. Diff:\n{}",
            e.loosenings.join("; "),
            e.diff
        ),
        &["If you made this edit, confirm it in the Yolmira window; if you didn't, an agent or another program may have: undo it."],
    )
}

fn webview2(version: Option<&str>) -> Check {
    match version {
        Some(v) => Check::ok("webview2", format!("WebView2 Runtime {v}")),
        None => Check::fail(
            "webview2",
            "the Microsoft Edge WebView2 Runtime isn't installed: the Yolmira window can't open",
            &["Install the WebView2 Runtime (Evergreen) from Microsoft's WebView2 page."],
        ),
    }
}

#[cfg(test)]
mod tests;
