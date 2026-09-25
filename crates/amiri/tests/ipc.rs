//! Slice A IPC tests: pipe security, authentication, event log over IPC.

mod support;

use amiri_ipc::client::open_pipe;
use amiri_ipc::protocol::{codes, methods, notifications, EventsSubscribeParams, Request};
use amiri_ipc::win::security::object_sddl;
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
    // Protected DACL with exactly one ACE: allow, for this user's SID.
    assert!(sddl.starts_with("D:P"), "{sddl}");
    assert_eq!(
        sddl.matches("(A;").count(),
        1,
        "exactly one allow ACE: {sddl}"
    );
    assert!(!sddl.contains("(D;"), "{sddl}");
    assert!(sddl.contains(&format!(";;;{})", inst.sid)), "{sddl}");
    // Lower integrity levels (Low IL sandboxes) can neither read nor write.
    let level = if elevated() { "HI" } else { "ME" };
    assert!(sddl.contains(&format!("(ML;;NWNR;;;{level})")), "{sddl}");
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
    let first_run: Vec<_> = env.events().into_iter().take(3).collect();
    let kinds: Vec<_> = first_run
        .iter()
        .map(|e| e["kind"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        kinds,
        vec!["daemon.started", "daemon.stopping", "daemon.stopped"]
    );
    // `events` restarted the daemon; the first run's events are unchanged, in order.
    let after_restart = env.events();
    assert_eq!(&after_restart[..3], &first_run[..]);
    assert_eq!(after_restart[3]["kind"], "daemon.started");
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
