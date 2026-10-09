//! Slice A IPC tests: pipe security, authentication, event log over IPC.

mod support;

use amiri_ipc::client::open_pipe;
use amiri_ipc::protocol::{codes, methods, notifications, EventsSubscribeParams, Request};
use amiri_ipc::win::security::{object_sddl, owner_only_sddl, sddl_aces, SecurityDescriptor};
use amiri_ipc::{Client, ClientError};
use serde_json::json;
use std::os::windows::fs::OpenOptionsExt;
use std::time::Duration;
use support::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows_sys::Win32::Security::Authorization::SE_KERNEL_OBJECT;
use windows_sys::Win32::Storage::FileSystem::READ_CONTROL;

/// SDDL (DACL + mandatory label) of the daemon's pipe, read through a client handle.
fn pipe_sddl(pipe_name: &str) -> String {
    let pipe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL)
        .open(pipe_name)
        .expect("open pipe with READ_CONTROL");
    object_sddl(&pipe, SE_KERNEL_OBJECT).expect("read pipe security")
}

#[test]
fn pipe_dacl_is_current_user_only() {
    let env = TestEnv::new();
    env.start();
    let inst = env.instance();
    let sddl = pipe_sddl(&inst.pipe_name());
    // Protected DACL: exactly the ACEs of owner_only_sddl (one allow ACE for this
    // user, NWNR mandatory label), compared as Windows renders them (e.g. `LA` on CI).
    let expected = SecurityDescriptor::from_sddl(&owner_only_sddl(&inst.sid, elevated()))
        .and_then(|d| d.to_sddl())
        .expect("expected SDDL");
    assert!(sddl.starts_with("D:P"), "{sddl}");
    assert_eq!(
        sddl_aces(&sddl),
        sddl_aces(&expected),
        "{sddl} vs {expected}"
    );
    assert_eq!(
        sddl.matches("(A;").count(),
        1,
        "exactly one allow ACE: {sddl}"
    );
}

#[test]
fn ipc_rejects_missing_or_wrong_token() {
    let env = TestEnv::new();
    env.start();
    let inst = env.instance();
    runtime().block_on(async {
        // Wrong token.
        let pipe = open_pipe(&inst).await.unwrap();
        match Client::handshake(pipe, "not-the-token", "attacker").await {
            Err(ClientError::Rpc(e)) => assert_eq!(e.code, codes::UNAUTHORIZED),
            Err(e) => panic!("expected an RPC error, got {e}"),
            Ok(_) => panic!("wrong token accepted"),
        }
        // No hello: a normal method first.
        let pipe = open_pipe(&inst).await.unwrap();
        let (r, mut w) = tokio::io::split(pipe);
        let line =
            serde_json::to_string(&Request::new(1, methods::DAEMON_STATUS, json!({}))).unwrap();
        w.write_all(format!("{line}\n").as_bytes()).await.unwrap();
        let mut reply = String::new();
        BufReader::new(r).read_line(&mut reply).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["error"]["code"], codes::UNAUTHORIZED);
        assert!(v.get("result").is_none(), "no data before hello: {v}");
    });
    let reasons: Vec<_> = env
        .events()
        .into_iter()
        .filter(|e| e["kind"] == "ipc.auth_failed")
        .map(|e| {
            e["payload"]["reason"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(reasons, vec!["bad_token", "no_hello"]);
}

#[test]
fn overlong_hello_is_cut_off() {
    let env = TestEnv::new();
    env.start();
    let inst = env.instance();
    runtime().block_on(async {
        let pipe = open_pipe(&inst).await.unwrap();
        let (r, mut w) = tokio::io::split(pipe);
        // 64 KiB without a newline: far past the 4 KiB hello limit.
        let _ = w.write_all(&vec![b'x'; 64 * 1024]).await;
        let mut reply = String::new();
        let read = tokio::time::timeout(
            Duration::from_secs(10),
            BufReader::new(r).read_line(&mut reply),
        )
        .await;
        let v: serde_json::Value =
            serde_json::from_str(&reply).expect("an error reply before closing");
        assert!(read.is_ok());
        assert_eq!(v["error"]["code"], codes::UNAUTHORIZED);
        assert!(v["id"].is_null(), "{v}");
    });
    assert!(env
        .events()
        .iter()
        .any(|e| e["kind"] == "ipc.auth_failed" && e["payload"]["reason"] == "bad_frame"));
}

#[test]
fn failed_auth_events_are_rate_limited() {
    let env = TestEnv::new();
    env.start();
    let inst = env.instance();
    runtime().block_on(async {
        for _ in 0..20 {
            let pipe = open_pipe(&inst).await.unwrap();
            assert!(Client::handshake(pipe, "wrong", "flood").await.is_err());
        }
    });
    let failures: Vec<_> = env
        .events()
        .into_iter()
        .filter(|e| e["kind"] == "ipc.auth_failed")
        .collect();
    assert_eq!(
        failures.len(),
        1,
        "20 bad tokens within 10 s record one event: {failures:?}"
    );
}

#[test]
fn events_persist_across_restart() {
    let env = TestEnv::new();
    env.start();
    let _ = env.amiri(&["daemon", "stop"]);
    env.wait_until("daemon stopped", Duration::from_secs(10), || {
        env.running_pid().is_none()
    });
    let first_run: Vec<_> = env.events().into_iter().take(4).collect();
    let kinds: Vec<_> = first_run
        .iter()
        .map(|e| e["kind"].as_str().unwrap_or_default())
        .collect();
    // Stopping goes through the policy gate (Slice D): its decision is logged first.
    assert_eq!(
        kinds,
        vec![
            "daemon.started",
            "policy.decision",
            "daemon.stopping",
            "daemon.stopped"
        ]
    );
    // `events` restarted the daemon; the first run's events are unchanged, in order.
    let after_restart = env.events();
    assert_eq!(&after_restart[..4], &first_run[..]);
    assert_eq!(after_restart[4]["kind"], "daemon.started");
    assert!(after_restart
        .windows(2)
        .all(|w| w[0]["seq"].as_i64() < w[1]["seq"].as_i64()));
}

#[test]
fn subscribers_receive_new_events() {
    let env = TestEnv::new();
    env.start();
    runtime().block_on(async {
        let mut sub = env.client().await;
        sub.call_value(
            methods::EVENTS_SUBSCRIBE,
            json!(EventsSubscribeParams { after: Some(0) }),
        )
        .await
        .unwrap();
        // Backlog first: the daemon.started event.
        let first = tokio::time::timeout(Duration::from_secs(10), sub.next_notification())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.method, notifications::EVENT);
        assert_eq!(first.params["kind"], "daemon.started");
        // A new event from another client arrives live.
        let pipe = open_pipe(&env.instance()).await.unwrap();
        let _ = Client::handshake(pipe, "wrong", "other").await;
        let live = tokio::time::timeout(Duration::from_secs(10), sub.next_notification())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(live.params["kind"], "ipc.auth_failed");
        assert!(live.params["seq"].as_i64() > first.params["seq"].as_i64());
    });
}

/// Team mode needs no migration later (decisions.md 2026-10-09): every event names the
/// person this install acts for, and that id survives restarts.
#[test]
fn events_name_the_person_and_it_is_stable() {
    let env = TestEnv::new();
    env.start();
    let path = env.instance().config_dir().join("identity.toml");
    let text = std::fs::read_to_string(&path).expect("identity.toml created on first start");
    let id_line = text
        .lines()
        .find(|l| l.starts_with("person_id"))
        .expect("person_id");
    let person = id_line.split('"').nth(1).expect("quoted id").to_string();
    let _ = env.amiri(&["daemon", "stop"]);
    env.wait_until("daemon stopped", Duration::from_secs(10), || {
        env.running_pid().is_none()
    });
    env.start();
    let events = env.events();
    assert!(events.len() >= 5, "two runs' events: {events:?}");
    for e in &events {
        assert_eq!(e["person_id"], person.as_str(), "{e}");
    }
    let started: Vec<_> = events
        .iter()
        .filter(|e| e["kind"] == "daemon.started")
        .collect();
    assert_eq!(started.len(), 2);
    assert!(started
        .iter()
        .all(|e| e["payload"]["person_id"] == person.as_str()));
    let stop_decision = events
        .iter()
        .find(|e| e["kind"] == "policy.decision")
        .expect("daemon.stop's decision");
    assert_eq!(
        stop_decision["payload"]["requested_by_person"],
        person.as_str()
    );
}
