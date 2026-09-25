//! `amiri doctor` on the real machine. Machine-independent: it must report both checks
//! with a valid status whatever this machine's state is (the simulated broken
//! machines are unit tests in `cli::doctor`).

use serde_json::Value;
use std::process::Command;

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
