//! Command implementations.

use super::output::Out;
use super::{
    approve, exit, project, shell, Cli, Command, DaemonAction, ProjectAction, ShellAction,
};
use amiri_core::{AmiriError, Event};
use amiri_ipc::protocol::{
    methods, notifications, DaemonStatus, EventsListParams, EventsSubscribeParams,
};
use amiri_ipc::win::process::wait_pid_exit;
use amiri_ipc::{ensure_daemon, Client, ClientError, Instance, LaunchOptions, Launched};
use serde_json::json;
use std::time::Duration;

const CLIENT: &str = "amiri-cli";
const START_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn run(cli: Cli) -> u8 {
    let out = Out { json: cli.json };
    let instance = match Instance::resolve(cli.instance.clone(), cli.home.clone()) {
        Ok(i) => i,
        Err(e) => {
            out.error(
                &AmiriError::new("Resolve the AMIRI instance", "cli", e.to_string())
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
        Command::Doctor => Ok(doctor(&out)),
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
pub(super) async fn connect_started(instance: &Instance) -> Result<Client, AmiriError> {
    ensure_daemon(launch_options(instance.clone(), false)?).await?;
    Client::connect(instance, CLIENT)
        .await
        .map_err(client_error)
}

fn doctor(out: &Out) -> u8 {
    use super::doctor::{checks, Status};
    let results = checks(&amiri_ipc::win::env::EnvSource::system());
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

fn launch_options(instance: Instance, allow_elevated: bool) -> Result<LaunchOptions, AmiriError> {
    let daemon_exe = std::env::current_exe()
        .map(|p| p.with_file_name("amirid.exe"))
        .map_err(|e| {
            AmiriError::new("Find amirid.exe", "cli", e.to_string()).next_step("Reinstall AMIRI.")
        })?;
    Ok(LaunchOptions {
        instance,
        daemon_exe,
        allow_elevated,
        timeout: START_TIMEOUT,
        client_name: CLIENT.into(),
    })
}

async fn start(out: &Out, instance: Instance, allow_elevated: bool) -> Result<u8, AmiriError> {
    let launched = ensure_daemon(launch_options(instance, allow_elevated)?).await?;
    let (state, hello) = match &launched {
        Launched::Started(h) => ("started", h),
        Launched::AlreadyRunning(h) => ("already_running", h),
    };
    out.value(
        || format!("amirid {} (pid {})", state.replace('_', " "), hello.pid),
        json!({ "state": state, "pid": hello.pid, "daemon_version": hello.daemon_version }),
    );
    Ok(exit::OK)
}

/// Connect without starting the daemon; `Ok(None)` if it isn't running.
async fn connect_existing(instance: &Instance) -> Result<Option<Client>, AmiriError> {
    match Client::connect(instance, CLIENT).await {
        Ok(c) => Ok(Some(c)),
        Err(ClientError::NotRunning) => Ok(None),
        Err(e) => Err(client_error(e)),
    }
}

async fn stop(out: &Out, instance: &Instance) -> Result<u8, AmiriError> {
    let Some(mut c) = connect_existing(instance).await? else {
        out.value(
            || "amirid is not running".into(),
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
        .map_err(|e| AmiriError::new("Stop the AMIRI daemon", "cli", e.to_string()))?
        .map_err(|e| AmiriError::new("Stop the AMIRI daemon", "cli", e.to_string()))?;
    if !exited {
        return Err(AmiriError::new(
            "Stop the AMIRI daemon",
            "cli",
            format!(
                "amirid (pid {pid}) accepted the stop but is still running after {} s",
                STOP_TIMEOUT.as_secs()
            ),
        )
        .next_step(format!("End amirid.exe (pid {pid}) in Task Manager.")));
    }
    out.value(
        || format!("amirid (pid {pid}) stopped"),
        json!({ "state": "stopped", "pid": pid }),
    );
    Ok(exit::OK)
}

async fn status(out: &Out, instance: &Instance) -> Result<u8, AmiriError> {
    let Some(mut c) = connect_existing(instance).await? else {
        out.value(
            || "amirid is not running".into(),
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
                "amirid running: pid {}, version {}, since {}, {} client(s), {} events",
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
) -> Result<u8, AmiriError> {
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
                    || "amirid stopped; no more events".into(),
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
            AmiriError::new("Read an event", "cli", e.to_string())
                .next_step("Update amiri and amirid together.")
        })?;
        if kind.as_deref().is_none_or(|k| e.kind.starts_with(k)) {
            out.event(&e);
        }
    }
}

pub(super) fn client_error(e: ClientError) -> AmiriError {
    let base = AmiriError::new("Talk to the AMIRI daemon", "cli", e.to_string());
    match e {
        ClientError::Rpc(r) => match r.data {
            Some(inner) => *inner,
            None => base.next_step("Run `amiri doctor`."),
        },
        ClientError::NotRunning => base.next_step("Start it with `amiri daemon start`."),
        _ => base.next_step("Run `amiri doctor`."),
    }
}
