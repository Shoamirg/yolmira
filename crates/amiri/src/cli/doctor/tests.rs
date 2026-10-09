//! Broken machines, injected as facts: each fault is reported with why and next steps.

use super::agents::{AgentFact, AgentSpec, Missing, Probe, Range};
use super::*;

fn healthy() -> Facts {
    Facts {
        env: EnvSource {
            windows_dir: Some(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())),
            machine: vec![
                ("ComSpec".into(), r"%SystemRoot%\system32\cmd.exe".into()),
                (
                    "Path".into(),
                    r"%SystemRoot%\system32;%SystemRoot%\System32\WindowsPowerShell\v1.0\".into(),
                ),
            ],
            ..Default::default()
        },
        daemon: DaemonFact::NotRunning,
        db: DbFact::Missing,
        elevated: Some(false),
        enable_lua: Some(1),
        agents: Ok(vec![]),
        config: vec![],
        webview2: Some("154.0.4258.62".into()),
    }
}

fn find<'a>(checks: &'a [Check], name: &str) -> &'a Check {
    checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no {name} in {checks:?}"))
}

fn reported(c: &Check, status: Status) {
    assert_eq!(c.status, status, "{c:?}");
    assert!(!c.detail.is_empty(), "a why: {c:?}");
    assert!(!c.next_steps.is_empty(), "next steps: {c:?}");
}

#[test]
fn a_healthy_machine_is_all_ok() {
    let c = checks(&healthy());
    assert!(c.iter().all(|c| c.status == Status::Ok), "{c:?}");
}

#[test]
fn uac_off_fails_with_the_fix() {
    let c = checks(&Facts {
        enable_lua: Some(0),
        ..healthy()
    });
    let uac = find(&c, "uac");
    reported(uac, Status::Fail);
    assert!(uac.next_steps[0].contains("EnableLUA"));
}

#[test]
fn a_corrupt_database_fails_with_how_to_restore() {
    let c = checks(&Facts {
        db: DbFact::Inspected(Inspection {
            integrity: "*** in database main ***\nPage 2 is never used".into(),
            version: LATEST_VERSION,
            running: vec![],
        }),
        ..healthy()
    });
    reported(find(&c, "database"), Status::Fail);
    let unreadable = checks(&Facts {
        db: DbFact::Unreadable("file is not a database".into()),
        ..healthy()
    });
    reported(find(&unreadable, "database"), Status::Fail);
}

#[test]
fn sessions_recorded_running_while_the_daemon_is_down_are_stale() {
    let inspected = DbFact::Inspected(Inspection {
        integrity: "ok".into(),
        version: LATEST_VERSION,
        running: vec!["s1".into()],
    });
    let down = checks(&Facts {
        db: inspected.clone(),
        ..healthy()
    });
    reported(find(&down, "sessions_stale"), Status::Warn);
    let up = checks(&Facts {
        db: inspected,
        daemon: DaemonFact::Running {
            version: "0.1.0".into(),
            pid: 1,
            pipe_acl_ok: Some(true),
            live_running: vec!["s1".into()],
        },
        ..healthy()
    });
    assert_eq!(find(&up, "sessions_stale").status, Status::Ok);
}

fn claude(probe: Probe) -> Facts {
    Facts {
        agents: Ok(vec![AgentFact {
            spec: AgentSpec {
                name: "claude".into(),
                command: "claude".into(),
                version_args: vec!["--version".into()],
                tested: Some(Range::try_from("2.1.281".to_string()).unwrap()),
                missing: Missing::Warn,
            },
            probe,
        }]),
        ..healthy()
    }
}

#[test]
fn claude_outside_the_tested_range_warns() {
    let found = |v: &str| Probe::Found {
        path: r"C:\x\claude.exe".into(),
        version: Some(v.into()),
        output: v.into(),
    };
    reported(
        find(&checks(&claude(found("2.1.282"))), "agent_claude"),
        Status::Warn,
    );
    assert_eq!(
        find(&checks(&claude(found("2.1.281"))), "agent_claude").status,
        Status::Ok
    );
    reported(
        find(&checks(&claude(Probe::NotFound)), "agent_claude"),
        Status::Warn,
    );
}

#[test]
fn an_optional_agent_missing_is_fine() {
    let mut f = claude(Probe::NotFound);
    if let Ok(list) = &mut f.agents {
        list[0].spec.missing = Missing::Info;
    }
    assert_eq!(find(&checks(&f), "agent_claude").status, Status::Ok);
}

#[test]
fn an_impostor_pipe_and_missing_webview2_fail() {
    let c = checks(&Facts {
        daemon: DaemonFact::Impostor("pid 4242 is not amirid".into()),
        webview2: None,
        ..healthy()
    });
    reported(find(&c, "daemon"), Status::Fail);
    reported(find(&c, "webview2"), Status::Fail);
}

#[test]
fn an_unconfirmed_loosening_edit_warns_and_names_it() {
    let c = checks(&Facts {
        config: vec![ConfigFact {
            file: ConfigFile::Policies,
            confirmed: Some("[global]\n\"session.kill\" = \"allow\"\n".into()),
            current: Some(
                "[global]\n\"session.kill\" = \"allow\"\n\"daemon.stop\" = \"allow\"\n".into(),
            ),
            unreadable: None,
        }],
        ..healthy()
    });
    let check = find(&c, "config_policies");
    reported(check, Status::Warn);
    assert!(check.detail.contains("daemon.stop"), "{check:?}");
}

#[test]
fn running_elevated_warns() {
    let c = checks(&Facts {
        elevated: Some(true),
        ..healthy()
    });
    reported(find(&c, "elevation"), Status::Warn);
}
