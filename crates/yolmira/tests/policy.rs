//! Slice D: the policy gate end to end (phase1-plan §1 Slice D), and who may answer.
//! Every instance has its own `policies.toml` under its test home, never the user's.

mod support;

use serde_json::{json, Value};
use std::time::Duration;
use support::shell::Shell;
use support::*;
use yolmira_ipc::client::ASK_LIMIT;
use yolmira_ipc::protocol::{codes, methods, PendingAsk, SessionInfo};
use yolmira_ipc::ClientError;

/// Kill asks; stopping the daemon (test cleanup) doesn't.
const KILL_ASKS: &str = "[global]\n\"session.kill\" = \"ask\"\n\"daemon.stop\" = \"allow\"\n";
const STEP: Duration = Duration::from_secs(30);

/// A daemon that treats this test binary as the Yolmira UI (debug builds only, via a
/// file in the instance's own config folder), so the test can play the UI: answer
/// without a code and read codes.
fn env_with_test_ui() -> TestEnv {
    let env = TestEnv::new();
    env.write_policies(KILL_ASKS);
    let me = std::env::current_exe().expect("test exe");
    let config = env.instance().config_dir();
    std::fs::write(
        config.join("debug-trusted-ui.txt"),
        me.to_string_lossy().as_bytes(),
    )
    .expect("trusted UI file");
    env.start();
    env
}

async fn new_cmd(env: &TestEnv) -> SessionInfo {
    let mut c = env.client().await;
    c.call(
        methods::SESSION_CREATE,
        json!({ "shell": { "kind": "cmd" }, "cols": 80, "rows": 25 }),
    )
    .await
    .expect("create")
}

/// Start a kill on its own connection; it blocks in ASK. Returns the waiting request.
async fn kill_waiting(
    env: &TestEnv,
    id: &str,
) -> (
    tokio::task::JoinHandle<Result<Value, ClientError>>,
    PendingAsk,
) {
    let mut requester = env.client().await;
    let id_owned = id.to_string();
    let call = tokio::spawn(async move {
        requester
            .call_value_within(methods::SESSION_KILL, json!({ "id": id_owned }), ASK_LIMIT)
            .await
    });
    let mut ui = env.client().await;
    let deadline = std::time::Instant::now() + STEP;
    loop {
        let pending: Vec<PendingAsk> = ui.call(methods::POLICY_PENDING, json!({})).await.unwrap();
        if let Some(ask) = pending.into_iter().find(|a| a.detail["id"] == id) {
            assert_eq!(ask.action, "session.kill");
            return (call, ask);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no ask for the kill within {STEP:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[test]
fn ask_round_trip() {
    let env = env_with_test_ui();
    env.block_on(Duration::from_secs(120), async {
        // Allowed: the waiting kill goes ahead.
        let shell = new_cmd(&env).await;
        let (call, ask) = kill_waiting(&env, &shell.id).await;
        let mut ui = env.client().await;
        ui.call_value(
            methods::POLICY_ANSWER,
            json!({ "id": ask.id, "decision": "allow" }),
        )
        .await
        .expect("the UI answers without a code");
        call.await
            .unwrap()
            .expect("the kill goes ahead once allowed");

        // Denied: the caller is told, and the shell keeps running.
        let other = new_cmd(&env).await;
        let (call, ask) = kill_waiting(&env, &other.id).await;
        ui.call_value(
            methods::POLICY_ANSWER,
            json!({ "id": ask.id, "decision": "deny" }),
        )
        .await
        .unwrap();
        match call.await.unwrap() {
            Err(ClientError::Rpc(e)) => assert_eq!(e.code, codes::POLICY_DENIED),
            other => panic!("expected a policy denial, got {other:?}"),
        }
        let list: Vec<SessionInfo> = ui.call(methods::SESSION_LIST, json!({})).await.unwrap();
        let still = list.iter().find(|s| s.id == other.id).expect("listed");
        assert_eq!(still.status, "RUNNING");
    });
    // All of it is in the log, in order.
    let decisions: Vec<Value> = env
        .events()
        .into_iter()
        .filter(|e| e["kind"] == "policy.decision" && e["payload"]["action"] == "session.kill")
        .collect();
    assert_eq!(decisions.len(), 2, "{decisions:?}");
    assert_eq!(decisions[0]["payload"]["result"], "allow");
    assert_eq!(decisions[1]["payload"]["result"], "deny");
    // The kill's own events (session.create has its own decision event).
    let kinds: Vec<String> = env
        .events()
        .iter()
        .filter(|e| {
            e["payload"]["action"] == "session.kill" || e["kind"] == "session.kill_requested"
        })
        .map(|e| e["kind"].as_str().unwrap_or("").to_string())
        .collect();
    let pos = |k: &str| {
        kinds
            .iter()
            .position(|x| x == k)
            .unwrap_or_else(|| panic!("no {k}"))
    };
    assert!(pos("policy.ask") < pos("policy.answered"));
    assert!(pos("policy.answered") < pos("policy.decision"));
    assert!(pos("policy.decision") < pos("session.kill_requested"));
}

/// `yolmira approve` from a terminal: refused without the code, refused with a wrong one
/// (three wrong codes deny the request), accepted with the code the UI shows.
#[test]
fn cli_approval_needs_the_code() {
    let env = env_with_test_ui();
    env.block_on(Duration::from_secs(180), async {
        let mut ui = env.client().await;
        let shell = new_cmd(&env).await;
        let (call, ask) = kill_waiting(&env, &shell.id).await;
        let cli = |args: &[&str]| {
            let mut a = vec!["approve", ask.id.as_str()];
            a.extend_from_slice(args);
            env.yolmira(&a)
        };
        assert!(!cli(&[]).status.success(), "approving without a code");
        assert!(
            !cli(&["--code", "00000000"]).status.success(),
            "approving with a wrong code"
        );
        let code: Value = ui
            .call(methods::POLICY_CODE, json!({ "id": ask.id }))
            .await
            .unwrap();
        let code = code["code"].as_str().expect("code").to_string();
        let ok = cli(&["--code", &code]);
        assert!(
            ok.status.success(),
            "{}",
            String::from_utf8_lossy(&ok.stdout)
        );
        call.await
            .unwrap()
            .expect("approved from the CLI with the code");

        // Three wrong codes: denied, so a code can't be guessed.
        let other = new_cmd(&env).await;
        let (call, ask) = kill_waiting(&env, &other.id).await;
        for _ in 0..3 {
            let out = env.yolmira(&["approve", ask.id.as_str(), "--code", "12345678"]);
            assert!(!out.status.success());
        }
        match call.await.unwrap() {
            Err(ClientError::Rpc(e)) => assert_eq!(e.code, codes::POLICY_DENIED),
            other => panic!("three wrong codes must deny, got {other:?}"),
        }
    });
    let refused: Vec<Value> = env
        .events()
        .into_iter()
        .filter(|e| e["kind"] == "policy.answer_refused")
        .collect();
    let reasons: Vec<&str> = refused
        .iter()
        .map(|e| e["payload"]["reason"].as_str().unwrap())
        .collect();
    assert_eq!(
        reasons,
        [
            "no_code",
            "wrong_code",
            "wrong_code",
            "wrong_code",
            "wrong_code"
        ]
    );
    // The code itself is never written to the log.
    let log = serde_json::to_string(&env.events()).unwrap();
    assert!(!log.contains("\"code\":\""), "a code reached the event log");
}

/// The reviewer's case: an agent runs as the user, can read the pipe token, and even
/// has the right code. From inside an Yolmira agent job it still can't approve.
#[test]
fn agent_job_process_cannot_approve() {
    let env = env_with_test_ui();
    let agent_pid = env.block_on(Duration::from_secs(120), async {
        let mut ui = env.client().await;
        let shell = new_cmd(&env).await;
        let (call, ask) = kill_waiting(&env, &shell.id).await;
        let code: Value = ui
            .call(methods::POLICY_CODE, json!({ "id": ask.id }))
            .await
            .unwrap();
        let code = code["code"].as_str().unwrap().to_string();
        // The simulated agent: `yolmira approve <id> --code <right code>`, started by the
        // daemon inside an agent job, as Phase 4 will start agents.
        let args: Vec<String> = env
            .yolmira_args(&["approve", &ask.id, "--code", &code])
            .into_iter()
            .collect();
        let spawned: Value = ui
            .call(
                "debug.spawn_in_agent_job",
                json!({ "exe": YOLMIRA, "args": args }),
            )
            .await
            .expect("debug builds can spawn into an agent job");
        let pid = spawned["pid"].as_u64().expect("pid") as u32;
        let watch = yolmira_ipc::win::process::ProcessWatch::open(pid);
        if let Ok(w) = watch {
            let code = w
                .wait(STEP)
                .expect("wait")
                .expect("the agent's approve finishes");
            assert_ne!(code, 0, "approve from inside an agent job must fail");
        }
        // Still waiting: the agent's answer didn't count.
        let pending: Vec<PendingAsk> = ui.call(methods::POLICY_PENDING, json!({})).await.unwrap();
        assert!(
            pending.iter().any(|a| a.id == ask.id),
            "the request must still wait"
        );
        // Clean up: the UI denies it.
        ui.call_value(
            methods::POLICY_ANSWER,
            json!({ "id": ask.id, "decision": "deny" }),
        )
        .await
        .unwrap();
        let _ = call.await;
        pid
    });
    let refused: Vec<Value> = env
        .events()
        .into_iter()
        .filter(|e| e["kind"] == "policy.answer_refused")
        .collect();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0]["payload"]["reason"], "agent_job");
    assert_eq!(refused[0]["payload"]["pid"], agent_pid);
    let answered: Vec<Value> = env
        .events()
        .into_iter()
        .filter(|e| e["kind"] == "policy.answered")
        .collect();
    assert_eq!(answered.len(), 1, "only the UI's deny counts");
    assert_eq!(answered[0]["payload"]["decision"], "deny");
}

/// A client that isn't the UI can't read approval codes (and the attempt is logged).
#[test]
fn only_the_ui_gets_codes() {
    let env = TestEnv::new();
    env.write_policies(KILL_ASKS);
    env.start(); // no trusted test UI: this test binary is just a client
    env.block_on(Duration::from_secs(120), async {
        let shell = new_cmd(&env).await;
        let (call, ask) = kill_waiting(&env, &shell.id).await;
        let mut c = env.client().await;
        match c
            .call_value(methods::POLICY_CODE, json!({ "id": ask.id }))
            .await
        {
            Err(ClientError::Rpc(e)) => assert_eq!(e.code, codes::FORBIDDEN),
            other => panic!("a non-UI client got a code: {other:?}"),
        }
        // Nor may it answer without one.
        match c
            .call_value(
                methods::POLICY_ANSWER,
                json!({ "id": ask.id, "decision": "allow" }),
            )
            .await
        {
            Err(ClientError::Rpc(e)) => assert_eq!(e.code, codes::FORBIDDEN),
            other => panic!("a non-UI client answered without a code: {other:?}"),
        }
        call.abort();
    });
    let reasons: Vec<String> = env
        .events()
        .into_iter()
        .filter(|e| e["kind"] == "policy.answer_refused")
        .map(|e| e["payload"]["reason"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(reasons, ["not_the_ui", "no_code"]);
}

/// `project_shell_starts_in_repo`: the shell's working folder is the project's,
/// including a non-ASCII path.
#[test]
fn project_shell_starts_in_repo() {
    let env = TestEnv::new();
    env.start();
    let dir = env.home().join("Проект oʻzbek");
    std::fs::create_dir_all(&dir).unwrap();
    let added = env.yolmira(&["project", "add", "web", dir.to_str().unwrap()]);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stdout)
    );
    let listed = last_json(&env.yolmira(&["project", "list"]));
    assert_eq!(listed[0]["name"], "web");
    env.block_on(Duration::from_secs(120), async {
        let mut c = env.client().await;
        let info: SessionInfo = c
            .call(
                methods::SESSION_CREATE,
                json!({ "shell": { "kind": "cmd" }, "cols": 80, "rows": 25, "project": "web" }),
            )
            .await
            .expect("shell in the project");
        assert_eq!(info.project.as_deref(), Some("web"));
        assert_eq!(info.cwd, dir.to_str().unwrap());
        let mut p = Shell::attach(&env, &info.id, c).await.expect("attach");
        p.send("cd\r").await;
        p.expect("Проект oʻzbek").await;
    });
    // An unknown project is an error with a next step, not a shell somewhere else.
    let out = env.yolmira(&["shell", "new", "--project", "nope"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("project"));
}

/// policies.toml where kills are allowed and stopping the daemon asks (the default).
const STOP_ASKS: &str = "[global]\n\"session.kill\" = \"allow\"\n";
const STOP_ALLOWED: &str = "[global]\n\"session.kill\" = \"allow\"\n\"daemon.stop\" = \"allow\"\n";

/// Run a program inside a new agent job (debug builds) and wait for it to end.
async fn as_agent(ui: &mut yolmira_ipc::Client, exe: &str, args: Vec<String>) -> u32 {
    let spawned: Value = ui
        .call(
            "debug.spawn_in_agent_job",
            json!({ "exe": exe, "args": args }),
        )
        .await
        .expect("debug builds can spawn into an agent job");
    let pid = spawned["pid"].as_u64().expect("pid") as u32;
    if let Ok(w) = yolmira_ipc::win::process::ProcessWatch::open(pid) {
        let _ = w.wait(STEP);
    }
    pid
}

async fn pending(ui: &mut yolmira_ipc::Client) -> Vec<PendingAsk> {
    ui.call(methods::POLICY_PENDING, json!({})).await.unwrap()
}

/// The reviewer's case (2026-10-09): an agent edits policies.toml to allow
/// daemon.stop. The edit is logged with its diff and doesn't apply: stopping still
/// asks; the agent can't confirm its own edit, even with the code; once a person
/// confirms it, it applies.
#[test]
fn agent_edit_allowing_daemon_stop_still_asks() {
    let env = TestEnv::new();
    env.write_policies(STOP_ASKS);
    let me = std::env::current_exe().unwrap();
    let config = env.instance().config_dir();
    std::fs::write(
        config.join("debug-trusted-ui.txt"),
        me.to_string_lossy().as_bytes(),
    )
    .unwrap();
    env.start();
    let edited = env.home().join("edited-policies.toml");
    std::fs::write(&edited, STOP_ALLOWED).unwrap();
    let policies = env.instance().policies_path();
    env.block_on(Duration::from_secs(120), async {
        let mut ui = env.client().await;
        // The agent overwrites the file.
        let cmd = r"C:\Windows\System32\cmd.exe".to_string();
        let copy = vec![
            "/d".to_string(),
            "/c".to_string(),
            "copy".to_string(),
            "/y".to_string(),
            edited.display().to_string(),
            policies.display().to_string(),
        ];
        as_agent(&mut ui, &cmd, copy).await;
        assert_eq!(
            std::fs::read_to_string(&policies).unwrap(),
            STOP_ALLOWED,
            "the agent's edit is on disk"
        );

        // Stopping still asks.
        let mut requester = env.client().await;
        let stop = tokio::spawn(async move {
            requester
                .call_value_within(methods::DAEMON_STOP, json!({}), ASK_LIMIT)
                .await
        });
        let deadline = std::time::Instant::now() + STEP;
        let (confirm, stop_ask) = loop {
            let p = pending(&mut ui).await;
            let confirm = p.iter().find(|a| a.action == "config.policies").cloned();
            let stop_ask = p.iter().find(|a| a.action == "daemon.stop").cloned();
            if let (Some(c), Some(s)) = (confirm, stop_ask) {
                break (c, s);
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no confirmation and stop ask: {p:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert!(confirm.detail["diff"]
            .as_str()
            .unwrap()
            .contains("+\"daemon.stop\" = \"allow\""));

        // The agent tries to confirm its own edit with the right code: refused.
        let code: Value = ui
            .call(methods::POLICY_CODE, json!({ "id": confirm.id }))
            .await
            .unwrap();
        let args = env.yolmira_args(&[
            "approve",
            &confirm.id,
            "--code",
            code["code"].as_str().unwrap(),
        ]);
        as_agent(&mut ui, YOLMIRA, args).await;
        assert!(
            pending(&mut ui).await.iter().any(|a| a.id == confirm.id),
            "still unconfirmed"
        );

        // A person denies the stop: the daemon keeps running.
        ui.call_value(
            methods::POLICY_ANSWER,
            json!({ "id": stop_ask.id, "decision": "deny" }),
        )
        .await
        .unwrap();
        match stop.await.unwrap() {
            Err(ClientError::Rpc(e)) => assert_eq!(e.code, codes::POLICY_DENIED),
            other => panic!("the stop must still need approval: {other:?}"),
        }
        // A person confirms the edit in the UI: now stopping is allowed outright.
        ui.call_value(
            methods::POLICY_ANSWER,
            json!({ "id": confirm.id, "decision": "allow" }),
        )
        .await
        .unwrap();
        let mut c = env.client().await;
        c.call_value(methods::DAEMON_STOP, json!({}))
            .await
            .expect("allowed once confirmed");
    });
    env.wait_until("daemon stopped", Duration::from_secs(10), || {
        env.running_pid().is_none()
    });
    let events = env.events();
    let changed: Vec<_> = events
        .iter()
        .filter(|e| e["kind"] == "policy.config_changed")
        .collect();
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(changed[0]["payload"]["applied"], false);
    assert!(changed[0]["payload"]["diff"]
        .as_str()
        .unwrap()
        .contains("daemon.stop"));
    let refused: Vec<_> = events
        .iter()
        .filter(|e| e["kind"] == "policy.answer_refused")
        .collect();
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0]["payload"]["reason"], "agent_job");
    let accepted: Vec<_> = events
        .iter()
        .filter(|e| {
            e["kind"] == "policy.config_accepted" && e["payload"]["file"] == "policies.toml"
        })
        .map(|e| e["payload"]["reason"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(accepted.len(), 2, "{accepted:?}");
    assert!(accepted[0].starts_with("first start"));
    assert!(accepted[1].starts_with("confirmed by"));
}

/// An edit that only tightens applies at once (and is logged with its diff).
#[test]
fn tightening_edit_applies_at_once() {
    let env = TestEnv::new();
    env.start();
    env.block_on(Duration::from_secs(60), async {
        let shell = new_cmd(&env).await;
        let tight = format!("{TEST_POLICIES}\"session.create\" = \"ask\"\n").replace(
            "\"session.kill\" = \"allow\"",
            "\"session.kill\" = \"deny\"",
        );
        env.write_policies(&tight);
        let mut c = env.client().await;
        match c
            .call_value(methods::SESSION_KILL, json!({ "id": shell.id }))
            .await
        {
            Err(ClientError::Rpc(e)) => assert_eq!(e.code, codes::POLICY_DENIED),
            other => panic!("the tightened rule must apply at once: {other:?}"),
        }
    });
    let reasons: Vec<String> = env
        .events()
        .iter()
        .filter(|e| {
            e["kind"] == "policy.config_accepted" && e["payload"]["file"] == "policies.toml"
        })
        .map(|e| e["payload"]["reason"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        reasons.last().map(String::as_str),
        Some("an edit that only tightens")
    );
}
