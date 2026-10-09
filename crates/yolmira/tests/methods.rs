//! `every_mutating_method_emits_event` (phase1-plan Slice D): the automated check for
//! "the event log shows all actions". It walks the protocol's METHODS table: every
//! mutating method is called and must record its declared event; a mutating method
//! the table gains later without a recipe here fails the test, so nothing new slips
//! past the log unnoticed.

mod support;

use serde_json::json;
use std::time::Duration;
use support::*;
use yolmira_ipc::client::ASK_LIMIT;
use yolmira_ipc::protocol::{codes, methods, PendingAsk, SessionInfo, METHODS};
use yolmira_ipc::{Client, ClientError};

/// project.register asks, so policy.answer has something to answer.
const POLICIES: &str =
    "[global]\n\"session.kill\" = \"allow\"\n\"daemon.stop\" = \"allow\"\n\"project.register\" = \"ask\"\n";

fn last_seq(env: &TestEnv) -> i64 {
    env.events()
        .last()
        .and_then(|e| e["seq"].as_i64())
        .unwrap_or(0)
}

fn kinds_after(env: &TestEnv, seq: i64) -> Vec<String> {
    env.events()
        .into_iter()
        .filter(|e| e["seq"].as_i64().unwrap_or(0) > seq)
        .map(|e| e["kind"].as_str().unwrap_or("").to_string())
        .collect()
}

/// Call one mutating method the way a client would. `None`: no recipe yet.
async fn call(env: &TestEnv, name: &str, shell: &SessionInfo, dir: &str) -> Option<()> {
    let mut c = env.client().await;
    let id = json!({ "id": shell.id });
    let r = match name {
        methods::SESSION_CREATE => c
            .call_value(
                name,
                json!({ "shell": { "kind": "cmd" }, "cols": 80, "rows": 25 }),
            )
            .await
            .map(|_| ()),
        methods::SESSION_ATTACH => {
            let r = c.call_value(name, id).await.map(|_| ());
            // Keep the connection while the attached event is recorded.
            tokio::time::sleep(Duration::from_millis(200)).await;
            r
        }
        methods::SESSION_DETACH => {
            c.call_value(methods::SESSION_ATTACH, id.clone())
                .await
                .ok()?;
            c.call_value(name, id).await.map(|_| ())
        }
        methods::SESSION_INPUT => c
            .call_value(name, json!({ "id": shell.id, "data": "IA0K" }))
            .await
            .map(|_| ()),
        methods::SESSION_RESIZE => c
            .call_value(name, json!({ "id": shell.id, "cols": 100, "rows": 30 }))
            .await
            .map(|_| ()),
        methods::SESSION_KILL => c.call_value(name, id).await.map(|_| ()),
        methods::PROJECT_ADD => {
            let dir = dir.to_string();
            answer_next_ask(env, async move {
                c.call_value_within(
                    methods::PROJECT_ADD,
                    json!({ "name": "p1", "path": dir }),
                    ASK_LIMIT,
                )
                .await
                .map(|_| ())
            })
            .await
        }
        methods::POLICY_ANSWER => {
            let dir = dir.to_string();
            answer_next_ask(env, async move {
                c.call_value_within(
                    methods::PROJECT_ADD,
                    json!({ "name": "p2", "path": dir }),
                    ASK_LIMIT,
                )
                .await
                .map(|_| ())
            })
            .await
        }
        methods::PROJECT_REMOVE => c
            .call_value(name, json!({ "name": "p1" }))
            .await
            .map(|_| ()),
        methods::DAEMON_STOP => c.call_value(name, json!({})).await.map(|_| ()),
        _ => return None,
    };
    r.unwrap_or_else(|e| panic!("{name}: {e:?}"));
    Some(())
}

/// Run a request that will ask, and answer the ask as the (test) UI.
async fn answer_next_ask(
    env: &TestEnv,
    request: impl std::future::Future<Output = Result<(), ClientError>> + Send + 'static,
) -> Result<(), ClientError> {
    let waiting = tokio::spawn(request);
    let mut ui: Client = env.client().await;
    let ask = loop {
        let pending: Vec<PendingAsk> = ui.call(methods::POLICY_PENDING, json!({})).await?;
        if let Some(a) = pending.into_iter().next() {
            break a;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    ui.call_value(
        methods::POLICY_ANSWER,
        json!({ "id": ask.id, "decision": "allow" }),
    )
    .await?;
    waiting.await.expect("request task")
}

#[test]
fn every_mutating_method_emits_event() {
    let env = TestEnv::new();
    env.write_policies(POLICIES);
    let me = std::env::current_exe().unwrap();
    std::fs::write(
        env.instance().config_dir().join("debug-trusted-ui.txt"),
        me.to_string_lossy().as_bytes(),
    )
    .unwrap();
    env.start();
    let dir = env.home().join("project");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.to_str().unwrap().to_string();

    // Reads exist (none answers "unknown method").
    env.block_on(Duration::from_secs(60), async {
        let mut c = env.client().await;
        for m in METHODS.iter().filter(|m| !m.mutating) {
            if m.name == methods::HELLO {
                continue;
            }
            if let Err(ClientError::Rpc(e)) = c.call_value(m.name, json!({ "id": "x" })).await {
                assert_ne!(e.code, codes::METHOD_NOT_FOUND, "{} isn't served", m.name);
            }
            if m.name == methods::EVENTS_SUBSCRIBE {
                c = env.client().await; // a subscribed connection only streams
            }
        }
    });

    // Mutating methods: each records its event; daemon.stop last.
    let mut order: Vec<_> = METHODS.iter().filter(|m| m.mutating).collect();
    order.sort_by_key(|m| m.name == methods::DAEMON_STOP);
    let mut missing = Vec::new();
    for m in order {
        let shell: SessionInfo = env.block_on(Duration::from_secs(60), async {
            let mut c = env.client().await;
            c.call(
                methods::SESSION_CREATE,
                json!({ "shell": { "kind": "cmd" }, "cols": 80, "rows": 25 }),
            )
            .await
            .expect("a shell to act on")
        });
        let before = last_seq(&env);
        let done = env.block_on(Duration::from_secs(60), call(&env, m.name, &shell, &dir));
        if done.is_none() {
            missing.push(m.name);
            continue;
        }
        if m.name == methods::DAEMON_STOP {
            env.wait_until("daemon stopped", Duration::from_secs(10), || {
                env.running_pid().is_none()
            });
        }
        let kinds = kinds_after(&env, before);
        match (m.event, m.unlogged) {
            (Some(event), _) => assert!(
                kinds.iter().any(|k| k == event),
                "{} must record {event}; recorded {kinds:?}",
                m.name
            ),
            (None, Some(why)) => assert!(!why.is_empty(), "{}: unlogged needs a reason", m.name),
            (None, None) => panic!("{} is mutating but names no event and no reason", m.name),
        }
    }
    assert!(
        missing.is_empty(),
        "mutating methods without a recipe in methods.rs: {missing:?} (add one: the log must show every action)"
    );
}
