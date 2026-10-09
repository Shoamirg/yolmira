//! Approvals from the command line (phase1-plan Slice D).
//!
//! - `amiri approve --list` shows the requests waiting for an answer.
//! - `amiri approve <id> --code <code> [--deny] [--session]` answers one. The code
//!   is shown only in the AMIRI window: approving takes a person who can see it, not
//!   just a process that can read the pipe token.
//! - A command whose own request waits (e.g. `amiri shell kill` under an ASK rule)
//!   says so and, in a terminal, lets the user type that code.

use super::commands::{client_error, connect_started};
use super::exit;
use super::output::Out;
use amiri_core::event::kinds;
use amiri_core::AmiriError;
use amiri_ipc::client::ASK_LIMIT;
use amiri_ipc::protocol::{
    methods, notifications, AnswerDecision, AnswerScope, EventsSubscribeParams, PendingAsk,
    PolicyAnswerParams,
};
use amiri_ipc::{Client, Instance};
use serde_json::{json, Value};
use std::io::IsTerminal;

pub async fn list(out: &Out, instance: Instance) -> Result<u8, AmiriError> {
    let mut c = connect_started(&instance).await?;
    let pending: Vec<PendingAsk> = c
        .call(methods::POLICY_PENDING, json!({}))
        .await
        .map_err(client_error)?;
    if out.json {
        out.value(String::new, json!(pending));
        return Ok(exit::OK);
    }
    if pending.is_empty() {
        out.value(|| "Nothing waits for approval.".into(), json!(null));
    }
    for a in &pending {
        let line = format!("{}  {}  {}  ({})", a.id, a.action, a.detail, a.rule);
        out.value(|| line, json!(null));
    }
    Ok(exit::OK)
}

pub async fn answer(
    out: &Out,
    instance: Instance,
    id: String,
    code: Option<String>,
    deny: bool,
    session: bool,
) -> Result<u8, AmiriError> {
    let mut c = connect_started(&instance).await?;
    let params = PolicyAnswerParams {
        id: id.clone(),
        decision: if deny {
            AnswerDecision::Deny
        } else {
            AnswerDecision::Allow
        },
        scope: if session {
            AnswerScope::Session
        } else {
            AnswerScope::Once
        },
        code,
    };
    c.call_value(methods::POLICY_ANSWER, json!(params))
        .await
        .map_err(client_error)?;
    let verb = if deny { "denied" } else { "approved" };
    out.value(
        || format!("{id} {verb}"),
        json!({ "id": id, "answer": verb }),
    );
    Ok(exit::OK)
}

/// A call the policy gate may hold until someone approves it. While it waits, the
/// user is told where to approve; in a terminal they can type the window's code.
pub async fn gated_call(
    instance: &Instance,
    c: &mut Client,
    method: &str,
    params: Value,
) -> Result<Value, AmiriError> {
    let watcher = watch_own_asks(instance, c.hello.connection).await;
    let result = c
        .call_value_within(method, params, ASK_LIMIT)
        .await
        .map_err(client_error);
    if let Some(w) = watcher {
        w.abort();
    }
    result
}

/// Follow `policy.ask` events; for one about this connection's request, prompt.
async fn watch_own_asks(
    instance: &Instance,
    connection: u64,
) -> Option<tokio::task::JoinHandle<()>> {
    let mut monitor = Client::connect(instance, "amiri-cli-approval").await.ok()?;
    monitor
        .call_value(
            methods::EVENTS_SUBSCRIBE,
            json!(EventsSubscribeParams { after: None }),
        )
        .await
        .ok()?;
    let instance = instance.clone();
    Some(tokio::spawn(async move {
        while let Ok(n) = monitor.next_notification().await {
            if n.method != notifications::EVENT || n.params["kind"] != kinds::POLICY_ASK {
                continue;
            }
            let Ok(ask) = serde_json::from_value::<PendingAsk>(n.params["payload"].clone()) else {
                continue;
            };
            if ask.connection == connection {
                prompt(&instance, ask).await;
            }
        }
    }))
}

async fn prompt(instance: &Instance, ask: PendingAsk) {
    eprintln!(
        "Waiting for approval: {} {} (rule: {}).\nApprove it in the AMIRI window, or type the code the window shows for it.",
        ask.action, ask.detail, ask.rule
    );
    if !std::io::stdin().is_terminal() {
        return;
    }
    loop {
        eprint!("Code (or Enter to keep waiting): ");
        let line = tokio::task::spawn_blocking(|| {
            let mut s = String::new();
            std::io::stdin().read_line(&mut s).map(|_| s)
        })
        .await;
        let Ok(Ok(line)) = line else { return };
        let code = line.trim().to_string();
        if code.is_empty() {
            continue;
        }
        let Ok(mut c) = Client::connect(instance, "amiri-cli-approval").await else {
            return;
        };
        let params = PolicyAnswerParams {
            id: ask.id.clone(),
            decision: AnswerDecision::Allow,
            scope: AnswerScope::Once,
            code: Some(code),
        };
        match c.call_value(methods::POLICY_ANSWER, json!(params)).await {
            Ok(_) => return,
            Err(e) => eprintln!("{}", client_error(e)),
        }
    }
}
