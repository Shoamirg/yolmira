//! Connection limits and stalled clients (Phase 1 exit checks, the output-budget fix):
//! a client that never reads stalls only its own connection and is disconnected after
//! the stall limit, while the UI keeps getting and answering approval prompts; one
//! process can't take every connection, and the verified UI always gets in.

mod support;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use support::*;
use yolmira_ipc::client::ASK_LIMIT;
use yolmira_ipc::protocol::{methods, notifications, SessionInfo, PROTOCOL_VERSION};

/// Kill asks (so there is an approval prompt to answer); stopping the daemon doesn't.
const KILL_ASKS: &str = "[global]\n\"session.kill\" = \"ask\"\n\"daemon.stop\" = \"allow\"\n";
/// Daemon-side limits these tests check (`daemon::slots`, `daemon::outbox`).
const PER_PROCESS: usize = 16;
const MAX_CONNECTIONS: usize = 128;
const STALL_LIMIT: Duration = Duration::from_secs(30);

/// A daemon that treats this test binary as the Yolmira UI (debug builds only).
fn env_with_test_ui() -> TestEnv {
    let env = TestEnv::new();
    env.write_policies(KILL_ASKS);
    env.trust_this_test_as_ui();
    env.start();
    env
}

async fn new_shell(env: &TestEnv, kind: &str) -> SessionInfo {
    let mut c = env.client().await;
    c.call(
        methods::SESSION_CREATE,
        json!({ "shell": { "kind": kind }, "cols": 80, "rows": 25 }),
    )
    .await
    .expect("create")
}

/// The UI's side of an approval: wait for the `policy.ask` event naming this kill,
/// then allow it. Returns how long the prompt took to arrive.
async fn ui_allows_kill(env: &TestEnv, ui: &mut yolmira_ipc::Client, shell: &str) -> Duration {
    let mut requester = env.client().await;
    let id = shell.to_string();
    let started = std::time::Instant::now();
    let call = tokio::spawn(async move {
        requester
            .call_value_within(methods::SESSION_KILL, json!({ "id": id }), ASK_LIMIT)
            .await
    });
    let ask = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let n = ui.next_notification().await.expect("the UI's event feed");
            if n.method == notifications::EVENT
                && n.params["kind"] == "policy.ask"
                && n.params["payload"]["detail"]["id"] == shell
            {
                return n.params["payload"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
            }
        }
    })
    .await
    .expect("the UI got the approval prompt in time");
    let arrived = started.elapsed();
    ui.call_value(
        methods::POLICY_ANSWER,
        json!({ "id": ask, "decision": "allow" }),
    )
    .await
    .expect("the UI answers");
    call.await
        .unwrap()
        .expect("the kill goes ahead once allowed");
    arrived
}

#[test]
fn a_never_reading_client_is_disconnected_and_the_ui_still_answers_prompts() {
    let env = env_with_test_ui();
    env.block_on(Duration::from_secs(180), async {
        // A shell flooding output, with a client attached (and subscribed to events)
        // that never reads again.
        let flood = new_shell(&env, "cmd").await;
        let mut stuck = env.client().await;
        let stuck_conn = stuck.hello.connection;
        stuck
            .call_value(methods::EVENTS_SUBSCRIBE, json!({}))
            .await
            .expect("subscribe");
        stuck
            .call_value(methods::SESSION_ATTACH, json!({ "id": flood.id }))
            .await
            .expect("attach");
        let mut input = env.client().await;
        let cmd = "for /L %i in (1,1,100000000) do @echo flood-line-%i\r";
        input
            .call_value(
                methods::SESSION_INPUT,
                json!({ "id": flood.id, "data": B64.encode(cmd) }),
            )
            .await
            .expect("input");
        tokio::time::sleep(Duration::from_secs(3)).await; // its pipe is full by now

        // The UI, on its own connection, still gets prompts and answers them.
        let mut ui = env.client().await;
        ui.call_value(methods::EVENTS_SUBSCRIBE, json!({}))
            .await
            .expect("UI subscribes");
        let victim = new_shell(&env, "cmd").await;
        let took = ui_allows_kill(&env, &mut ui, &victim.id).await;
        println!("approval prompt reached the UI in {took:?} while a client was stuck");

        // After the stall limit the stuck client is disconnected, and that is logged.
        let deadline = std::time::Instant::now() + STALL_LIMIT + Duration::from_secs(30);
        let stalled = loop {
            let found = env.events().into_iter().find(|e| {
                e["kind"] == "ipc.client_stalled" && e["payload"]["connection"] == stuck_conn
            });
            if let Some(e) = found {
                break e;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the stuck client was not disconnected within {:?}",
                STALL_LIMIT + Duration::from_secs(30)
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        };
        println!("stalled: {}", stalled["payload"]);
        assert_eq!(stalled["payload"]["why"], "not reading", "{stalled}");

        // And the UI still works afterwards.
        let other = new_shell(&env, "cmd").await;
        ui_allows_kill(&env, &mut ui, &other.id).await;
        drop(stuck);
    });
}

/// One process opening `count` connections (`support/hold-connections.ps1`).
struct Holder {
    child: Child,
    accepted: usize,
}

impl Holder {
    fn start(env: &TestEnv, count: usize) -> Self {
        let instance = env.instance();
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            r"\tests\support\hold-connections.ps1"
        );
        let pipe = instance
            .pipe_name()
            .trim_start_matches(r"\\.\pipe\")
            .to_string();
        let mut child = Command::new(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", script])
            .args(["-Pipe", &pipe])
            .arg("-TokenFile")
            .arg(instance.token_path())
            .args(["-ProtocolVersion", &PROTOCOL_VERSION.to_string()])
            .args(["-Count", &count.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start the connection holder");
        let mut line = String::new();
        BufReader::new(child.stdout.take().expect("stdout"))
            .read_line(&mut line)
            .expect("holder output");
        let accepted = line
            .trim()
            .strip_prefix("accepted ")
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("holder said {line:?}"));
        Self { child, accepted }
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        if let Some(mut stdin) = self.child.stdin.take() {
            let _ = stdin.write_all(b"\n");
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn ui_connects(env: &TestEnv) {
    let mut ui = env.client().await;
    let _: Vec<Value> = ui
        .call(methods::SESSION_LIST, json!({}))
        .await
        .expect("the UI is served");
}

#[test]
fn one_process_opening_128_connections_gets_its_share_and_the_ui_still_connects() {
    let env = env_with_test_ui();
    let holder = Holder::start(&env, MAX_CONNECTIONS);
    assert_eq!(
        holder.accepted, PER_PROCESS,
        "one process gets at most its per-process share"
    );
    env.block_on(Duration::from_secs(60), ui_connects(&env));
    let refusals = env
        .events()
        .into_iter()
        .filter(|e| {
            e["kind"] == "ipc.auth_failed"
                && e["payload"]["reason"] == "too_many_connections_from_process"
        })
        .count();
    assert!(refusals >= 1, "refusals are logged");
    drop(holder);
}

#[test]
fn every_slot_taken_by_other_processes_the_ui_still_connects() {
    let env = env_with_test_ui();
    let holders: Vec<Holder> = (0..MAX_CONNECTIONS / PER_PROCESS)
        .map(|_| Holder::start(&env, PER_PROCESS))
        .collect();
    let total: usize = holders.iter().map(|h| h.accepted).sum();
    assert_eq!(
        total,
        MAX_CONNECTIONS,
        "{} processes fill the pool",
        holders.len()
    );
    let late = Holder::start(&env, 1);
    assert_eq!(late.accepted, 0, "the pool is full for everyone else");
    env.block_on(Duration::from_secs(60), ui_connects(&env));
    drop(late);
    drop(holders);
}
