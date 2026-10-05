//! `policy_table` (phase1-plan Slice D): scope precedence, unknown → ASK, a narrower
//! scope can't loosen a global DENY, hard classifications.

use super::*;

fn policy(toml: &str) -> Policy {
    Policy::from_toml(toml).expect("valid policies.toml")
}

fn decide(p: &Policy, action: &str, project: Option<&str>) -> Decision {
    p.decide(action, project).decision
}

#[test]
fn built_in_defaults_apply_without_a_file() {
    let p = Policy::default();
    assert_eq!(decide(&p, actions::SESSION_CREATE, None), Decision::Allow);
    assert_eq!(decide(&p, actions::SESSION_KILL, None), Decision::Ask);
    assert_eq!(decide(&p, actions::DAEMON_STOP, None), Decision::Ask);
    assert_eq!(decide(&p, actions::PROJECT_REGISTER, None), Decision::Allow);
    assert_eq!(decide(&p, actions::PROJECT_REMOVE, None), Decision::Allow);
}

#[test]
fn unknown_actions_are_ask_never_allow() {
    let p = Policy::default();
    let v = p.decide("deploy.production", None);
    assert_eq!(v.decision, Decision::Ask);
    assert_eq!(v.rule, RuleSource::Unknown);
}

#[test]
fn a_global_rule_overrides_the_default() {
    let p = policy("[global]\n\"session.kill\" = \"allow\"\n");
    assert_eq!(decide(&p, actions::SESSION_KILL, None), Decision::Allow);
}

#[test]
fn project_rules_win_over_global_for_that_project_only() {
    let p = policy(
        "[global]\n\"session.create\" = \"ask\"\n[project.web]\n\"session.create\" = \"allow\"\n",
    );
    assert_eq!(
        decide(&p, actions::SESSION_CREATE, Some("web")),
        Decision::Allow
    );
    assert_eq!(
        decide(&p, actions::SESSION_CREATE, Some("other")),
        Decision::Ask
    );
    assert_eq!(decide(&p, actions::SESSION_CREATE, None), Decision::Ask);
}

#[test]
fn an_exact_action_beats_a_wildcard_in_the_same_scope() {
    let p = policy("[global]\n\"session.*\" = \"deny\"\n\"session.create\" = \"allow\"\n");
    assert_eq!(decide(&p, actions::SESSION_CREATE, None), Decision::Allow);
    assert_eq!(decide(&p, actions::SESSION_KILL, None), Decision::Deny);
}

#[test]
fn the_longer_wildcard_wins() {
    let p = policy("[global]\n\"git.*\" = \"allow\"\n\"git.push.*\" = \"deny\"\n");
    assert_eq!(decide(&p, "git.push.tags", None), Decision::Deny);
    assert_eq!(decide(&p, "git.fetch", None), Decision::Allow);
}

#[test]
fn a_project_cannot_loosen_a_global_deny() {
    let p = policy(
        "[global]\n\"session.create\" = \"deny\"\n[project.web]\n\"session.create\" = \"allow\"\n",
    );
    let v = p.decide(actions::SESSION_CREATE, Some("web"));
    assert_eq!(v.decision, Decision::Deny);
    assert_eq!(v.rule, RuleSource::Global("session.create".into()));
}

#[test]
fn a_project_may_tighten_a_global_rule() {
    let p = policy(
        "[global]\n\"session.create\" = \"allow\"\n[project.prod]\n\"session.create\" = \"deny\"\n",
    );
    assert_eq!(
        decide(&p, actions::SESSION_CREATE, Some("prod")),
        Decision::Deny
    );
}

#[test]
fn system_changes_are_never_allow() {
    let p = policy(
        "[global]\n\"system.*\" = \"allow\"\n\"system.elevate\" = \"allow\"\n[project.x]\n\"system.env.write\" = \"allow\"\n",
    );
    for a in [
        actions::SYSTEM_ELEVATE,
        actions::SYSTEM_ENV_WRITE,
        actions::SYSTEM_REGISTRY_WRITE,
    ] {
        assert_eq!(decide(&p, a, None), Decision::Ask, "{a}");
        assert_eq!(decide(&p, a, Some("x")), Decision::Ask, "{a} in a project");
    }
}

#[test]
fn system_changes_can_still_be_denied() {
    let p = policy("[global]\n\"system.elevate\" = \"deny\"\n");
    assert_eq!(decide(&p, actions::SYSTEM_ELEVATE, None), Decision::Deny);
}

#[test]
fn destructive_git_is_ask_under_a_broad_allow() {
    let p = policy("[global]\n\"git.*\" = \"allow\"\n");
    for a in DESTRUCTIVE {
        assert_eq!(decide(&p, a, None), Decision::Ask, "{a}");
    }
    assert_eq!(decide(&p, "git.fetch", None), Decision::Allow);
}

#[test]
fn destructive_git_honours_an_explicit_per_action_override() {
    let p = policy("[global]\n\"git.*\" = \"allow\"\n\"git.push.force\" = \"allow\"\n");
    assert_eq!(decide(&p, actions::GIT_PUSH_FORCE, None), Decision::Allow);
    assert_eq!(decide(&p, actions::GIT_RESET_HARD, None), Decision::Ask);
}

#[test]
fn destructive_git_defaults_to_ask_with_no_rules() {
    let p = Policy::default();
    for a in DESTRUCTIVE {
        assert_eq!(decide(&p, a, None), Decision::Ask, "{a}");
    }
}

#[test]
fn verdicts_name_the_rule_that_decided() {
    let p = policy("[project.web]\n\"session.*\" = \"allow\"\n");
    let v = p.decide(actions::SESSION_KILL, Some("web"));
    assert_eq!(
        v.rule,
        RuleSource::Project("web".into(), "session.*".into())
    );
    assert_eq!(
        p.decide(actions::SESSION_KILL, None).rule,
        RuleSource::Default
    );
}

#[test]
fn invalid_files_are_rejected_with_a_reason() {
    for (bad, why) in [
        ("[global]\n\"session.kill\" = \"maybe\"\n", "maybe"),
        ("[global]\n\"session kill\" = \"allow\"\n", "session kill"),
        ("[global]\n\"*\" = \"allow\"\n", "*"),
        ("[weird]\n", "weird"),
        ("not toml at all [", "TOML"),
    ] {
        let e = Policy::from_toml(bad).expect_err(bad);
        assert!(e.contains(why), "{bad:?} -> {e}");
    }
}

#[test]
fn action_patterns_match_whole_segments() {
    let p = policy("[global]\n\"session.*\" = \"deny\"\n");
    // "sessionx.create" must not match "session.*".
    assert_eq!(decide(&p, "sessionx.create", None), Decision::Ask);
}
