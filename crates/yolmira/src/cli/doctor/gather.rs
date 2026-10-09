//! Reads the machine for `yolmira doctor`. Nothing here changes anything: the daemon is
//! not started, the database is opened read-only, agent CLIs are only asked their
//! version.

use super::agents::{self, AgentFact};
use super::{ConfigFact, DaemonFact, DbFact, Facts};
use crate::config_files::{self, ConfigFile};
use serde_json::json;
use yolmira_ipc::protocol::{methods, SessionInfo};
use yolmira_ipc::win::env::{build_child_env, registry_dword, registry_string, EnvSource};
use yolmira_ipc::win::security::{
    is_elevated, object_sddl, owner_only_sddl, sddl_aces, SecurityDescriptor,
};
use yolmira_ipc::{Client, ClientError, Instance};
use yolmira_store::Store;

const UAC_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";
/// The WebView2 Runtime's EdgeUpdate client key (machine-wide, then per-user).
const WEBVIEW2_KEYS: [(bool, &str); 2] = [
    (
        true,
        r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
    ),
    (
        false,
        r"Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
    ),
];

pub async fn gather(instance: &Instance) -> Facts {
    let env = EnvSource::system();
    let child_env = build_child_env(&env);
    let path_var = child_env
        .iter()
        .find(|(k, _)| k == "PATH")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let config = config_facts(instance);
    let agents = config
        .iter()
        .find(|c| c.file == ConfigFile::Agents)
        .ok_or_else(|| "agents.toml state unknown".to_string())
        .and_then(|c| config_files::guarded_agents(c.confirmed.as_deref(), c.current.as_deref()))
        .map(|(specs, _)| {
            specs
                .into_iter()
                .map(|spec| {
                    let probe = agents::probe(&spec, &path_var, &child_env);
                    AgentFact { spec, probe }
                })
                .collect()
        });
    Facts {
        daemon: daemon(instance).await,
        db: database(instance),
        elevated: is_elevated().ok(),
        enable_lua: registry_dword(true, UAC_KEY, "EnableLUA").ok().flatten(),
        agents,
        config,
        webview2: webview2(),
        legacy: super::legacy::gather(instance),
        env,
    }
}

async fn daemon(instance: &Instance) -> DaemonFact {
    let mut c = match Client::connect(instance, "yolmira-doctor").await {
        Ok(c) => c,
        Err(ClientError::NotRunning) => return DaemonFact::NotRunning,
        Err(ClientError::Impostor(why)) => return DaemonFact::Impostor(why),
        Err(e) => return DaemonFact::Error(e.to_string()),
    };
    let live_running = c
        .call::<Vec<SessionInfo>>(methods::SESSION_LIST, json!({}))
        .await
        .map(|l| {
            l.into_iter()
                .filter(|s| s.status == "RUNNING")
                .map(|s| s.id)
                .collect()
        })
        .unwrap_or_default();
    DaemonFact::Running {
        version: c.hello.daemon_version.clone(),
        pid: c.hello.pid,
        pipe_acl_ok: pipe_acl_ok(instance),
        live_running,
    }
}

/// The pipe's DACL must be exactly the owner-only one the daemon sets (either the
/// normal or the elevated variant).
fn pipe_acl_ok(instance: &Instance) -> Option<bool> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
    use windows_sys::Win32::Security::Authorization::SE_KERNEL_OBJECT;
    use windows_sys::Win32::Storage::FileSystem::READ_CONTROL;
    let pipe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL)
        .open(instance.pipe_name())
        .ok()?;
    let actual = sddl_aces(&object_sddl(&pipe, SE_KERNEL_OBJECT).ok()?);
    let expected = |elevated| {
        SecurityDescriptor::from_sddl(&owner_only_sddl(&instance.sid, elevated))
            .and_then(|d| d.to_sddl())
            .map(|s| sddl_aces(&s))
            .ok()
    };
    Some(
        [false, true]
            .into_iter()
            .any(|e| expected(e).as_ref() == Some(&actual)),
    )
}

fn database(instance: &Instance) -> DbFact {
    let path = instance.db_path();
    if !path.exists() {
        return DbFact::Missing;
    }
    match Store::inspect(&path) {
        Ok(i) => DbFact::Inspected(i),
        Err(e) => DbFact::Unreadable(e.to_string()),
    }
}

/// Each guarded file's confirmed text (from the event log, read-only) and current
/// text. Never confirmed yet: the daemon trusts the file on its first start, so the
/// current text counts as confirmed.
fn config_facts(instance: &Instance) -> Vec<ConfigFact> {
    let events =
        Store::read_events(&instance.db_path(), config_files::ACCEPTED_KIND).unwrap_or_default();
    ConfigFile::ALL
        .into_iter()
        .map(|file| {
            let current = file.read(&instance.config_dir());
            let current = match current {
                Ok(c) => c,
                Err(e) => {
                    return ConfigFact {
                        file,
                        confirmed: None,
                        current: None,
                        unreadable: Some(e),
                    }
                }
            };
            let confirmed =
                config_files::confirmed_in(&events, file).unwrap_or_else(|| current.clone());
            ConfigFact {
                file,
                confirmed,
                current,
                unreadable: None,
            }
        })
        .collect()
}

fn webview2() -> Option<String> {
    WEBVIEW2_KEYS.iter().find_map(|(machine, key)| {
        registry_string(*machine, key, "pv")
            .ok()
            .flatten()
            .filter(|v| !v.is_empty() && v != "0.0.0.0")
    })
}
