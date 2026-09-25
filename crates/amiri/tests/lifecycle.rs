//! Slice A lifecycle tests (phase1-plan §1 slice A, §2): detached start, single
//! instance, restrictive jobs, console close, Ctrl+C.

mod support;

use amiri_ipc::protocol::exit;
use amiri_ipc::win::process::{spawn, Job, SpawnOptions, CREATE_NO_WINDOW};
use std::time::Duration;
use support::*;

fn launcher_in_job(env: &TestEnv, allow_breakaway: bool) -> (Job, Option<u32>) {
    let job = Job::new(true, allow_breakaway).expect("job");
    let p = job
        .spawn(&SpawnOptions {
            exe: AMIRI.into(),
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
    let mut args = vec!["/d".to_string(), "/c".to_string(), AMIRI.to_string()];
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
    let err: amiri_core::AmiriError =
        serde_json::from_value(v["error"].clone()).expect("AmiriError");
    assert!(err.is_actionable(), "{err:?}");
    assert!(
        err.next_steps.iter().any(|s| s.contains("amirid")),
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
    let mut pty = Pty::spawn(AMIRI, &env.start_args());
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
            .map(|_| s.spawn(|| last_json(&run_bounded(AMIRI, &args, &home))))
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
    // A second amirid for the same instance refuses to run.
    let p = spawn(&SpawnOptions {
        exe: AMIRID.into(),
        args: env.amirid_args(),
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
    let mut args = env.amirid_args();
    args.push("--foreground".into());
    let mut pty = Pty::spawn(AMIRID, &args);
    env.wait_until("foreground daemon answers", Duration::from_secs(20), || {
        env.running_pid().is_some()
    });

    pty.send(b"");
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
