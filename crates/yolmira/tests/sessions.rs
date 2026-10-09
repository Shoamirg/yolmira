//! Slice B: session lifecycle through the daemon (phase1-plan §1 Slice B).

mod support;

use serde_json::json;
use std::time::Duration;
use support::shell::{not_run, Shell};
use support::*;
use yolmira_ipc::protocol::{methods, SessionIdParams, SessionInfo};

const OS_MARK: &str = "YOLMIRA_Windows_NT";

fn session_events(env: &TestEnv, id: &str) -> Vec<serde_json::Value> {
    env.events()
        .into_iter()
        .filter(|e| e["session_id"] == id)
        .collect()
}

fn kinds(events: &[serde_json::Value]) -> Vec<String> {
    events
        .iter()
        .map(|e| e["kind"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn session_survives_client_disconnect() {
    let env = TestEnv::new();
    env.start();
    runtime().block_on(async {
        let mut s = Shell::open(&env, json!({ "kind": "cmd" }), None)
            .await
            .expect("session");
        s.send("set YOLMIRA_MARK=42\r").await;
        s.send("echo YOLMIRA_SET_%OS%\r").await;
        s.expect("YOLMIRA_SET_Windows_NT").await;
        let id = s.id.clone();
        s.drop_connections(); // killed client: no detach
        let ctl = env.client().await;
        let mut again = Shell::attach(&env, &id, ctl).await.expect("reattach");
        assert!(
            again.text.contains("YOLMIRA_SET_Windows_NT"),
            "scrollback replayed: {:?}",
            support::shell::tail(&again.text)
        );
        again.send("echo YOLMIRA_V%YOLMIRA_MARK%_%OS%\r").await;
        again.expect("YOLMIRA_V42_Windows_NT").await;
    });
}

#[test]
fn attach_after_more_output_than_one_frame() {
    // The scrollback (2 MiB) is bigger than a protocol line (1 MiB): the replay must
    // arrive in pieces, or a busy session could never be attached again.
    let env = TestEnv::new();
    env.start();
    runtime().block_on(async {
        let mut s = Shell::open(&env, json!({ "kind": "powershell" }), None).await.expect("session");
        s.send("1..12000 | ForEach-Object { 'x' * 150 + $_ }; Write-Output (\"YOLMIRA_FLOOD\" + (6*7))\r").await;
        s.expect_within("YOLMIRA_FLOOD42", Duration::from_secs(120)).await;
        let id = s.id.clone();
        s.drop_connections();
        let ctl = env.client().await;
        let mut again = Shell::attach(&env, &id, ctl).await.expect("attach after a large output");
        assert!(again.text.len() > 1024 * 1024, "replay of {} bytes", again.text.len());
        again.send("Write-Output (\"YOLMIRA_AFTER\" + (6*7))\r").await;
        again.expect("YOLMIRA_AFTER42").await;
    });
}

#[test]
fn attach_and_detach_are_recorded_without_keystrokes() {
    let env = TestEnv::new();
    env.start();
    let id = runtime().block_on(async {
        let mut s = Shell::open(&env, json!({ "kind": "cmd" }), None)
            .await
            .expect("session");
        s.send("echo YOLMIRA_SECRETISH_%OS%\r").await;
        s.expect("YOLMIRA_SECRETISH_Windows_NT").await;
        s.id.clone()
    });
    let events = session_events(&env, &id);
    let k = kinds(&events);
    assert!(
        k.contains(&"session.created".into()) && k.contains(&"session.attached".into()),
        "{k:?}"
    );
    env.wait_until("detach recorded", Duration::from_secs(10), || {
        kinds(&session_events(&env, &id)).contains(&"session.detached".to_string())
    });
    let all = serde_json::to_string(&env.events()).unwrap();
    assert!(
        !all.contains("SECRETISH"),
        "keystrokes and output are never in the event log"
    );
}

#[test]
fn daemon_restart_reconciles_sessions() {
    let env = TestEnv::new();
    let pid = env.start()["pid"].as_u64().expect("pid");
    let id = runtime().block_on(async {
        let s = Shell::open(&env, json!({ "kind": "cmd" }), None)
            .await
            .expect("session");
        s.id.clone()
    });
    // A crash, not a stop: nothing gets to record the shell's end.
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .output();
    env.wait_until("daemon gone", Duration::from_secs(10), || {
        env.running_pid().is_none()
    });
    env.start();
    let k = kinds(&session_events(&env, &id));
    assert_eq!(
        &k[k.len() - 2..],
        ["session.unknown", "session.exited"],
        "{k:?}"
    );
    let last = session_events(&env, &id).pop().unwrap();
    assert_eq!(last["payload"]["reason"], "daemon_restart");
    let list = last_json(&env.yolmira(&["shell", "list"]));
    let row = list
        .as_array()
        .and_then(|a| a.iter().find(|r| r["id"] == id))
        .cloned()
        .unwrap();
    assert_eq!(
        row["status"], "EXITED",
        "never RUNNING after a restart: {row}"
    );
}

#[test]
fn clean_stop_records_session_exits() {
    let env = TestEnv::new();
    env.start();
    let id = runtime().block_on(async {
        let s = Shell::open(&env, json!({ "kind": "cmd" }), None)
            .await
            .expect("session");
        s.id.clone()
    });
    let _ = env.yolmira(&["daemon", "stop"]);
    env.wait_until("daemon stopped", Duration::from_secs(15), || {
        env.running_pid().is_none()
    });
    let k = kinds(&session_events(&env, &id));
    assert!(
        k.contains(&"session.exited".to_string()) && !k.contains(&"session.unknown".to_string()),
        "{k:?}"
    );
}

#[test]
fn exited_sessions_reject_input_and_attach() {
    let env = TestEnv::new();
    env.start();
    runtime().block_on(async {
        let mut s = Shell::open(&env, json!({ "kind": "cmd" }), None)
            .await
            .expect("session");
        s.send("exit 3\r").await;
        assert_eq!(s.wait_exit().await, Some(3));
        let mut c = env.client().await;
        let attach = c
            .call_value(
                methods::SESSION_ATTACH,
                json!(SessionIdParams { id: s.id.clone() }),
            )
            .await;
        assert!(attach.is_err(), "attach to an exited session must fail");
        let listed: Vec<SessionInfo> = c.call(methods::SESSION_LIST, json!({})).await.unwrap();
        assert!(listed
            .iter()
            .any(|i| i.id == s.id && i.status == "EXITED" && i.exit_code == Some(3)));
    });
}

#[test]
fn child_env_is_allowlisted_in_a_real_shell() {
    let env = TestEnv::new();
    let junk_path = format!(
        r"C:\YOLMIRA_JUNK_PATH;{}",
        std::env::var("PATH").unwrap_or_default()
    );
    env.start_with_env(&[("YOLMIRA_TEST_SECRET", "canary"), ("PATH", &junk_path)]);
    runtime().block_on(async {
        let mut s = Shell::open(&env, json!({ "kind": "cmd" }), None)
            .await
            .expect("session");
        s.send("if defined YOLMIRA_TEST_SECRET (echo LEAK_%OS%) else (echo CLEAN_%OS%)\r")
            .await;
        s.expect("CLEAN_Windows_NT").await;
        s.send(
            "if \"%PATH:YOLMIRA_JUNK=%\"==\"%PATH%\" (echo NOJUNK_%OS%) else (echo JUNK_%OS%)\r",
        )
        .await;
        s.expect("NOJUNK_Windows_NT").await;
        assert!(
            !s.text.contains("LEAK_Windows_NT") && !s.text.contains("JUNK_Windows_NT\r\n\x1b"),
            "{:?}",
            support::shell::tail(&s.text)
        );
    });
}

#[test]
fn cli_attach_round_trip() {
    let env = TestEnv::new();
    env.start();
    let created = last_json(&env.yolmira(&["shell", "new", "--kind", "cmd"]));
    let id = created["id"].as_str().expect("id").to_string();
    // `yolmira shell attach` needs a real console: run it inside a pseudoconsole.
    let mut args = env.yolmira_args(&["shell", "attach", &id]);
    args.retain(|a| a != "--json");
    let mut pty = Pty::spawn(YOLMIRA, &args);
    std::thread::sleep(Duration::from_millis(1500));
    pty.send(b"echo YOLMIRA_CLI_%OS%\r");
    pty.wait_for("YOLMIRA_CLI_Windows_NT", Duration::from_secs(30));
    pty.send(&[0x1d]); // Ctrl+] detaches
    env.wait_until("attach exits", Duration::from_secs(15), || {
        matches!(pty.child.try_wait(), Ok(Some(_)))
    });
    let code = pty.child.try_wait().ok().flatten().map(|s| s.exit_code());
    assert_eq!(
        code,
        Some(0),
        "detach exits 0; output: {:?}",
        support::shell::tail(&pty.text())
    );
    // The shell is still running after the client left.
    let list = last_json(&env.yolmira(&["shell", "list"]));
    let row = list
        .as_array()
        .and_then(|a| a.iter().find(|r| r["id"] == id))
        .cloned()
        .unwrap();
    assert_eq!(row["status"], "RUNNING", "{row}");
    let _ = OS_MARK;
}

#[test]
fn wsl_shutdown_while_attached() {
    // `wsl --shutdown` stops every distro (the user's too); this test only terminates
    // a throwaway distro named in YOLMIRA_TEST_WSL_DISTRO.
    let Ok(distro) = std::env::var("YOLMIRA_TEST_WSL_DISTRO") else {
        not_run(
            "wsl_shutdown_while_attached",
            "set YOLMIRA_TEST_WSL_DISTRO to a throwaway distro (never the user's)",
        );
        return;
    };
    let env = TestEnv::new();
    env.start();
    runtime().block_on(async {
        let mut s = Shell::open(&env, json!({ "kind": "wsl", "distro": distro }), None)
            .await
            .expect("wsl session");
        s.send("echo YOLMIRA_$((6*7))\r").await;
        s.expect_within("YOLMIRA_42", Duration::from_secs(60)).await;
        let _ = std::process::Command::new(r"C:\Windows\System32\wsl.exe")
            .args(["--terminate", &distro])
            .output();
        s.wait_exit().await;
    });
    assert!(
        env.running_pid().is_some(),
        "the daemon survives a distro going away"
    );
}
