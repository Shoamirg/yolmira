//! `amiri doctor` on the real machine. Machine-independent: it must report both checks
//! with a valid status whatever this machine's state is (the simulated broken
//! machines are unit tests in `cli::doctor`).

use serde_json::Value;
use std::process::Command;

mod support;

use support::*;

fn doctor(env: &TestEnv) -> (Value, Option<i32>) {
    let out = env.amiri(&["doctor"]);
    let v: Value = serde_json::from_slice(&out.stdout).expect("doctor prints one JSON object");
    (v, out.status.code())
}

fn check<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["checks"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["name"] == name))
        .unwrap_or_else(|| panic!("no {name} in {v}"))
}

fn assert_reported(c: &Value, status: &str) {
    assert_eq!(c["status"], status, "{c}");
    assert!(!c["detail"].as_str().unwrap_or("").is_empty(), "a why: {c}");
    assert!(
        c["next_steps"].as_array().is_some_and(|s| !s.is_empty()),
        "next steps: {c}"
    );
}

/// `doctor_detects_faults` (phase1-plan Slice D) on real files and processes: a shell
/// left RUNNING by a killed daemon, a corrupted database copy, an agent CLI outside
/// its tested range. (`EnableLUA=0` is injected in the unit tests: a test can't turn
/// UAC off.)
#[test]
fn doctor_detects_faults() {
    // A daemon killed with a shell running leaves the row RUNNING.
    let env = TestEnv::new();
    env.start();
    let created = env.amiri(&["shell", "new"]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stdout)
    );
    let pid = env.running_pid().expect("daemon pid");
    let killed = std::process::Command::new("taskkill.exe")
        .args(["/F", "/PID", &pid.to_string()])
        .output()
        .expect("taskkill");
    assert!(killed.status.success());
    env.wait_until("daemon gone", std::time::Duration::from_secs(10), || {
        env.running_pid().is_none()
    });
    // A fake agent CLI that reports a version outside its tested range.
    let fake = env.home().join("fake-agent.cmd");
    std::fs::write(&fake, "@echo fake-agent 9.9.9\r\n").unwrap();
    let config = env.instance().config_dir();
    std::fs::write(
        config.join("agents.toml"),
        format!(
            "[[agent]]\nname = \"fake\"\ncommand = '{}'\ntested = \"1.0.0\"\nmissing = \"warn\"\n",
            fake.display()
        ),
    )
    .unwrap();
    let (v, code) = doctor(&env);
    assert_reported(check(&v, "sessions_stale"), "warn");
    assert!(check(&v, "sessions_stale")["detail"]
        .as_str()
        .unwrap()
        .contains("1 session"));
    assert_reported(check(&v, "agent_fake"), "warn");
    assert!(check(&v, "agent_fake")["detail"]
        .as_str()
        .unwrap()
        .contains("9.9.9"));
    assert_eq!(check(&v, "daemon")["status"], "ok", "not running is fine");
    // Doctor changed nothing: the daemon wasn't started, the row is still RUNNING.
    assert!(
        env.running_pid().is_none(),
        "doctor must not start the daemon"
    );
    let again = doctor(&env).0;
    assert_eq!(check(&again, "sessions_stale")["status"], "warn");
    assert!(code.is_some());

    // A corrupted copy of that database in another instance's home.
    let broken = TestEnv::new();
    let mut bytes = std::fs::read(env.instance().db_path()).expect("database file");
    bytes[..100].fill(0xA5); // the header: SQLite no longer recognises the file
    std::fs::write(broken.instance().db_path(), &bytes).unwrap();
    let (v, code) = doctor(&broken);
    assert_reported(check(&v, "database"), "fail");
    assert_eq!(code, Some(1), "a failed check makes doctor exit 1");
}

#[test]
fn doctor_reports_comspec_and_powershell_checks() {
    let out = Command::new(env!("CARGO_BIN_EXE_amiri"))
        .args(["--json", "doctor"])
        .output()
        .expect("run amiri doctor");
    let v: Value = serde_json::from_slice(&out.stdout).expect("doctor prints one JSON object");
    let checks = v["checks"].as_array().expect("checks array");
    for name in ["machine_comspec", "machine_path_powershell"] {
        let c = checks
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing: {v}"));
        let status = c["status"].as_str().unwrap_or_default();
        assert!(["ok", "warn", "fail"].contains(&status), "{c}");
        if status != "ok" {
            assert!(
                c["next_steps"].as_array().is_some_and(|s| !s.is_empty()),
                "a problem needs a fix: {c}"
            );
        }
    }
    let any_fail = checks.iter().any(|c| c["status"] == "fail");
    assert_eq!(
        out.status.code(),
        Some(if any_fail { 1 } else { 0 }),
        "exit code follows failures"
    );
}
