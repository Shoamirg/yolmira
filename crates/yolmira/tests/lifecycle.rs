//! Slice A lifecycle tests (phase1-plan §1 slice A, §2): detached start, single
//! instance, restrictive jobs, console close, Ctrl+C.

mod support;

use std::time::Duration;
use support::*;
use yolmira_ipc::protocol::exit;
use yolmira_ipc::win::process::{spawn, Job, SpawnOptions, CREATE_NO_WINDOW};

fn launcher_in_job(env: &TestEnv, allow_breakaway: bool) -> (Job, Option<u32>) {
    let job = Job::new(true, allow_breakaway).expect("job");
    let p = job
        .spawn(&SpawnOptions {
            exe: YOLMIRA.into(),
            args: env.start_args(),
            flags: CREATE_NO_WINDOW,
            ..Default::default()
        })
        .expect("spawn launcher in job");
    let code = p.wait(Duration::from_secs(30)).expect("wait launcher");
    (job, code)
}

#[test]
fn daemon_survives_launcher_job_close() {
    let env = TestEnv::new();
    if !env.require_detach("daemon_survives_launcher_job_close") {
        return;
    }
    let (job, code) = launcher_in_job(&env, true);
    assert_eq!(code, Some(0), "launcher exit code");
    let pid = env.running_pid().expect("daemon answers after start");
    drop(job); // kill-on-close: everything still in the job dies now
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(
        env.running_pid(),
        Some(pid),
        "daemon must survive its launcher's job closing"
    );
}

#[test]
fn restrictive_job_reports_manual_start() {
    // The launcher runs under cmd inside a job that forbids breakaway; cmd captures its
    // JSON output in a file and passes its exit code through.
    let env = TestEnv::new();
    let out_file = env.home().join("launcher.json");
    let comspec =
        std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());
    let mut args = vec!["/d".to_string(), "/c".to_string(), YOLMIRA.to_string()];
    args.extend(env.start_args());
    args.extend([">".to_string(), out_file.to_string_lossy().into_owned()]);
    let job = Job::new(true, false).expect("job");
    let p = job
        .spawn(&SpawnOptions {
            exe: comspec.into(),
            args,
            flags: CREATE_NO_WINDOW,
            ..Default::default()
        })
        .expect("spawn launcher in job");
    assert_eq!(
        p.wait(Duration::from_secs(30)).unwrap(),
        Some(1),
        "launcher must fail, not escape the job"
    );
    drop(job);
    let text = std::fs::read_to_string(&out_file).expect("launcher output");
    let v: serde_json::Value = text
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str(l).ok())
        .expect("json");
    let err: yolmira_core::YolmiraError =
        serde_json::from_value(v["error"].clone()).expect("YolmiraError");
    assert!(err.is_actionable(), "{err:?}");
    assert!(
        err.next_steps.iter().any(|s| s.contains("yolmirad")),
        "shows the manual command: {err:?}"
    );
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(
        env.running_pid(),
        None,
        "no daemon may start from inside a restrictive job"
    );
}

#[test]
fn daemon_survives_launcher_console_close() {
    let env = TestEnv::new();
    if !env.require_detach("daemon_survives_launcher_console_close") {
        return;
    }
    let mut pty = Pty::spawn(YOLMIRA, &env.start_args());
    env.wait_until("launcher exit", Duration::from_secs(30), || {
        matches!(pty.child.try_wait(), Ok(Some(_)))
    });
    let pid = env.running_pid().expect("daemon answers");
    drop(pty.close()); // close the pseudoconsole: attached processes get CTRL_CLOSE_EVENT
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        env.running_pid(),
        Some(pid),
        "a detached daemon has no console to lose"
    );
}

#[test]
fn second_start_connects_to_existing() {
    let env = TestEnv::new();
    if !env.require_detach("second_start_connects_to_existing") {
        return;
    }
    let first = env.start();
    let second = env.start();
    assert_eq!(first["state"], "started");
    assert_eq!(second["state"], "already_running");
    assert_eq!(first["pid"], second["pid"]);
}

#[test]
fn concurrent_start_single_daemon() {
    let env = TestEnv::new();
    if !env.require_detach("concurrent_start_single_daemon") {
        return;
    }
    let args = env.start_args();
    let home = env.home();
    let results: Vec<serde_json::Value> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|_| s.spawn(|| last_json(&run_bounded(YOLMIRA, &args, &home))))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let pids: std::collections::BTreeSet<_> = results.iter().map(|r| r["pid"].as_u64()).collect();
    assert_eq!(
        pids.len(),
        1,
        "one daemon for 8 concurrent starts: {results:?}"
    );
    assert!(pids.iter().all(Option::is_some), "{results:?}");
    assert_eq!(
        results.iter().filter(|r| r["state"] == "started").count(),
        1,
        "{results:?}"
    );
    // A second yolmirad for the same instance refuses to run.
    let p = spawn(&SpawnOptions {
        exe: YOLMIRAD.into(),
        args: env.yolmirad_args(),
        flags: CREATE_NO_WINDOW,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        p.wait(Duration::from_secs(20)).unwrap(),
        Some(exit::ALREADY_RUNNING)
    );
}

#[test]
fn ctrl_c_handling_s1d() {
    let env = TestEnv::new();
    let mut args = env.yolmirad_args();
    args.push("--foreground".into());
    let mut pty = Pty::spawn(YOLMIRAD, &args);
    env.wait_until("foreground daemon answers", Duration::from_secs(20), || {
        env.running_pid().is_some()
    });

    pty.send(b"\x03"); // Ctrl+C
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        matches!(pty.child.try_wait(), Ok(None)),
        "daemon must survive Ctrl+C in its own console"
    );
    assert!(env.running_pid().is_some(), "and keep answering");

    let mut child = pty.close();
    let mut code = None;
    env.wait_until(
        "daemon exit after console close",
        Duration::from_secs(8),
        || {
            code = child.try_wait().ok().flatten().map(|s| s.exit_code());
            code.is_some()
        },
    );
    assert_eq!(code, Some(0), "clean shutdown on console close");
    let stopped = env
        .events()
        .into_iter()
        .find(|e| e["kind"] == "daemon.stopped")
        .expect("daemon.stopped event");
    assert_eq!(stopped["payload"]["reason"], "console_close");
}

/// A detached daemon's stderr and panics go to `logs\yolmirad-stderr.log`, written
/// synchronously, so a crash leaves its reason behind (Phase 1 exit checks: one daemon
/// died with 0xC0000409 and left no trace). A panicking task doesn't end the daemon.
#[test]
fn crash_log_records_a_panic() {
    let env = TestEnv::new();
    let pid = env.start()["pid"].as_u64().expect("pid");
    let log = env.home().join("logs").join("yolmirad-stderr.log");
    let header = format!("--- yolmirad pid {pid} started");
    env.wait_until("the crash log's header", Duration::from_secs(10), || {
        std::fs::read_to_string(&log).is_ok_and(|t| t.contains(&header))
    });
    env.block_on(Duration::from_secs(30), async {
        let mut c = env.client().await;
        c.call_value("debug.panic", serde_json::json!({}))
            .await
            .expect("debug.panic");
    });
    env.wait_until(
        "the panic in the crash log",
        Duration::from_secs(10),
        || {
            std::fs::read_to_string(&log)
                .is_ok_and(|t| t.contains("debug.panic requested") && t.contains("panic in thread"))
        },
    );
    assert_eq!(env.running_pid(), Some(pid), "the daemon carries on");
}
