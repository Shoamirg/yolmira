//! Command implementations.

use super::output::Out;
use super::{
    approve, exit, identity, project, shell, Cli, Command, DaemonAction, IdentityAction,
    ProjectAction, ShellAction,
};
use serde_json::json;
use std::time::Duration;
use yolmira_core::{Event, YolmiraError};
use yolmira_ipc::protocol::{
    methods, notifications, DaemonStatus, EventsListParams, EventsSubscribeParams,
};
use yolmira_ipc::win::process::wait_pid_exit;
use yolmira_ipc::{ensure_daemon, Client, ClientError, Instance, LaunchOptions, Launched};

const CLIENT: &str = "yolmira-cli";
const START_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn run(cli: Cli) -> u8 {
    let out = Out { json: cli.json };
    let instance = match Instance::resolve(cli.instance.clone(), cli.home.clone()) {
        Ok(i) => i,
        Err(e) => {
            out.error(
                &YolmiraError::new("Resolve the Yolmira instance", "cli", e.to_string())
                    .next_step("Check --instance and --home."),
            );
            return exit::ERROR;
        }
    };
    let result = match cli.command {
        Command::Daemon {
            action: DaemonAction::Start { allow_elevated },
        } => start(&out, instance, allow_elevated).await,
        Command::Daemon {
            action: DaemonAction::Stop,
        } => stop(&out, &instance).await,
        Command::Daemon {
            action: DaemonAction::Status,
        } => status(&out, &instance).await,
        Command::Events {
            after,
            limit,
            kind,
            follow,
        } => events(&out, instance, after, limit, kind, follow).await,
        Command::Doctor => Ok(doctor(&out, &instance).await),
        Command::Approve {
            id,
            list,
            code,
            deny,
            session,
        } => match (list, id) {
            (true, _) | (false, None) => approve::list(&out, instance).await,
            (false, Some(id)) => approve::answer(&out, instance, id, code, deny, session).await,
        },
        Command::Identity { action } => match action {
            IdentityAction::Show => identity::show(&out, &instance),
            IdentityAction::Import { file, replace } => {
                identity::import(&out, &instance, file, replace)
            }
        },
        Command::Project { action } => match action {
            ProjectAction::Add { name, path } => project::add(&out, instance, name, path).await,
            ProjectAction::List => project::list(&out, instance).await,
            ProjectAction::Remove { name } => project::remove(&out, instance, name).await,
        },
        Command::Shell { action } => match action {
            ShellAction::New {
                kind,
                distro,
                cwd,
                project,
                attach,
            } => shell::new(&out, instance, &kind, distro, cwd, project, attach).await,
            ShellAction::List => shell::list(&out, instance).await,
            ShellAction::Attach { id } => shell::attach(&out, instance, id).await,
            ShellAction::Kill { id } => shell::kill(&out, instance, id).await,
        },
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            out.error(&e);
            exit::ERROR
        }
    }
}

/// Start the daemon if needed, then connect.
pub(super) async fn connect_started(instance: &Instance) -> Result<Client, YolmiraError> {
    ensure_daemon(launch_options(instance.clone(), false)?).await?;
    Client::connect(instance, CLIENT)
        .await
        .map_err(client_error)
}

/// Reads the machine without changing it; never starts the daemon.
async fn doctor(out: &Out, instance: &Instance) -> u8 {
    use super::doctor::{checks, gather, Status};
    let results = checks(&gather(instance).await);
    if out.json {
        out.value(String::new, json!({ "checks": results }));
    } else {
        for c in &results {
            let tag = match c.status {
                Status::Ok => "ok  ",
                Status::Warn => "WARN",
                Status::Fail => "FAIL",
            };
            out.value(|| format!("{tag} {}: {}", c.name, c.detail), json!(null));
            for step in &c.next_steps {
                out.value(|| format!("     next: {step}"), json!(null));
            }
        }
    }
    if results.iter().any(|c| c.status == Status::Fail) {
        exit::ERROR
    } else {
        exit::OK
    }
}

fn launch_options(instance: Instance, allow_elevated: bool) -> Result<LaunchOptions, YolmiraError> {
    let daemon_exe = std::env::current_exe()
        .map(|p| p.with_file_name("yolmirad.exe"))
        .map_err(|e| {
            YolmiraError::new("Find yolmirad.exe", "cli", e.to_string())
                .next_step("Reinstall Yolmira.")
        })?;
    Ok(LaunchOptions {
        instance,
        daemon_exe,
        allow_elevated,
        timeout: START_TIMEOUT,
        client_name: CLIENT.into(),
    })
}

async fn start(out: &Out, instance: Instance, allow_elevated: bool) -> Result<u8, YolmiraError> {
    let launched = ensure_daemon(launch_options(instance, allow_elevated)?).await?;
    let (state, hello) = match &launched {
        Launched::Started(h) => ("started", h),
        Launched::AlreadyRunning(h) => ("already_running", h),
    };
    out.value(
        || format!("yolmirad {} (pid {})", state.replace('_', " "), hello.pid),
        json!({ "state": state, "pid": hello.pid, "daemon_version": hello.daemon_version }),
    );
    Ok(exit::OK)
}

/// Connect without starting the daemon; `Ok(None)` if it isn't running.
async fn connect_existing(instance: &Instance) -> Result<Option<Client>, YolmiraError> {
    match Client::connect(instance, CLIENT).await {
        Ok(c) => Ok(Some(c)),
        Err(ClientError::NotRunning) => Ok(None),
        Err(e) => Err(client_error(e)),
    }
}

async fn stop(out: &Out, instance: &Instance) -> Result<u8, YolmiraError> {
    let Some(mut c) = connect_existing(instance).await? else {
        out.value(
            || "yolmirad is not running".into(),
            json!({ "state": "not_running" }),
        );
        return Ok(exit::NOT_RUNNING);
    };
    let pid = c.hello.pid;
    // Stopping ends every shell: the policy may ask for approval first.
    approve::gated_call(instance, &mut c, methods::DAEMON_STOP, json!({})).await?;
    // Return only once the process is gone, so an immediate `start` gets a fresh daemon.
    let exited = tokio::task::spawn_blocking(move || wait_pid_exit(pid, STOP_TIMEOUT))
        .await
        .map_err(|e| YolmiraError::new("Stop the Yolmira daemon", "cli", e.to_string()))?
        .map_err(|e| YolmiraError::new("Stop the Yolmira daemon", "cli", e.to_string()))?;
    if !exited {
        return Err(YolmiraError::new(
            "Stop the Yolmira daemon",
            "cli",
            format!(
                "yolmirad (pid {pid}) accepted the stop but is still running after {} s",
                STOP_TIMEOUT.as_secs()
            ),
        )
        .next_step(format!("End yolmirad.exe (pid {pid}) in Task Manager.")));
    }
    out.value(
        || format!("yolmirad (pid {pid}) stopped"),
        json!({ "state": "stopped", "pid": pid }),
    );
    Ok(exit::OK)
}

async fn status(out: &Out, instance: &Instance) -> Result<u8, YolmiraError> {
    let Some(mut c) = connect_existing(instance).await? else {
        out.value(
            || "yolmirad is not running".into(),
            json!({ "state": "not_running" }),
        );
        return Ok(exit::NOT_RUNNING);
    };
    let s: DaemonStatus = c
        .call(methods::DAEMON_STATUS, json!({}))
        .await
        .map_err(client_error)?;
    out.value(
        || {
            format!(
                "yolmirad running: pid {}, version {}, since {}, {} client(s), {} events",
                s.pid, s.version, s.started_at, s.clients, s.last_seq
            )
        },
        json!({ "state": "running", "status": s }),
    );
    Ok(exit::OK)
}

async fn events(
    out: &Out,
    instance: Instance,
    after: i64,
    limit: u32,
    kind: Option<String>,
    follow: bool,
) -> Result<u8, YolmiraError> {
    ensure_daemon(launch_options(instance.clone(), false)?).await?;
    let mut c = Client::connect(&instance, CLIENT)
        .await
        .map_err(client_error)?;
    let list: Vec<Event> = c
        .call(
            methods::EVENTS_LIST,
            json!(EventsListParams {
                after,
                limit,
                kind_prefix: kind.clone()
            }),
        )
        .await
        .map_err(client_error)?;
    let last = list.last().map(|e| e.seq).unwrap_or(after);
    list.iter().for_each(|e| out.event(e));
    if !follow {
        return Ok(exit::OK);
    }
    c.call_value(
        methods::EVENTS_SUBSCRIBE,
        json!(EventsSubscribeParams { after: Some(last) }),
    )
    .await
    .map_err(client_error)?;
    loop {
        let n = match c.next_notification().await {
            Ok(n) => n,
            Err(ClientError::Closed) => {
                out.value(
                    || "yolmirad stopped; no more events".into(),
                    json!({ "state": "daemon_stopped" }),
                );
                return Ok(exit::OK);
            }
            Err(e) => return Err(client_error(e)),
        };
        if n.method != notifications::EVENT {
            continue;
        }
        let e: Event = serde_json::from_value(n.params).map_err(|e| {
            YolmiraError::new("Read an event", "cli", e.to_string())
                .next_step("Update yolmira and yolmirad together.")
        })?;
        if kind.as_deref().is_none_or(|k| e.kind.starts_with(k)) {
            out.event(&e);
        }
    }
}

pub(super) fn client_error(e: ClientError) -> YolmiraError {
    let base = YolmiraError::new("Talk to the Yolmira daemon", "cli", e.to_string());
    match e {
        ClientError::Rpc(r) => match r.data {
            Some(inner) => *inner,
            None => base.next_step("Run `yolmira doctor`."),
        },
        ClientError::NotRunning => base.next_step("Start it with `yolmira daemon start`."),
        _ => base.next_step("Run `yolmira doctor`."),
    }
}
