//! The config files agents could edit to give themselves more rights:
//! `policies.toml` and `agents.toml` (reviewer, 2026-10-09). Pure functions over the
//! confirmed and the current text; the daemon's guard (`daemon/config_guard.rs`)
//! keeps the confirmed text in the event log and asks a person about loosening edits.
//!
//! - An edit that only tightens applies at once.
//! - A loosening edit waits; meanwhile each decision is the stricter of the
//!   confirmed and the edited rules, so its tightening parts still apply.
//! - A file that doesn't parse fails closed: for policies, at least ASK everywhere;
//!   for agents, the confirmed list.

use crate::agents_config::{self, AgentSpec};
use std::path::Path;
use yolmira_core::diff::line_diff;
use yolmira_core::policy::{Decision, Policy, Verdict};

/// Recorded with the full content each time a version is confirmed.
pub const ACCEPTED_KIND: &str = yolmira_core::event::kinds::POLICY_CONFIG_ACCEPTED;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfigFile {
    Policies,
    Agents,
}

impl ConfigFile {
    pub const ALL: [ConfigFile; 2] = [ConfigFile::Policies, ConfigFile::Agents];

    pub fn file_name(self) -> &'static str {
        match self {
            Self::Policies => "policies.toml",
            Self::Agents => agents_config::USER_FILE,
        }
    }

    /// The file's text, `None` when it doesn't exist.
    pub fn read(self, config_dir: &Path) -> Result<Option<String>, String> {
        let path = config_dir.join(self.file_name());
        match std::fs::read_to_string(&path) {
            Ok(t) => Ok(Some(t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }
}

/// The confirmed text of `file` from `policy.config_accepted` events (oldest
/// first): `None` when none was ever recorded, `Some(None)` when the confirmed state
/// is "no file".
pub fn confirmed_in(events: &[yolmira_core::Event], file: ConfigFile) -> Option<Option<String>> {
    events
        .iter()
        .rev()
        .find(|e| e.kind == ACCEPTED_KIND && e.payload["file"] == file.file_name())
        .map(|e| e.payload["content"].as_str().map(str::to_string))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    /// `-`/`+` lines from the confirmed to the current text.
    pub diff: String,
    /// What the edit loosens (empty: it only tightens or changes nothing).
    pub loosenings: Vec<String>,
    /// Why the current text can't be used, if it can't.
    pub invalid: Option<String>,
}

pub fn evaluate(file: ConfigFile, confirmed: Option<&str>, current: Option<&str>) -> Evaluation {
    let diff = line_diff(confirmed.unwrap_or(""), current.unwrap_or(""));
    let (loosenings, invalid) = match file {
        ConfigFile::Policies => match parse_policy(current) {
            Err(e) => (vec![], Some(e)),
            Ok(new) => {
                let old = parse_policy(confirmed).unwrap_or_default();
                let l = old
                    .loosenings(&new)
                    .into_iter()
                    .map(|l| {
                        let scope = l
                            .project
                            .map(|p| format!("project {p}"))
                            .unwrap_or("global".into());
                        format!("{} ({scope}): {:?} -> {:?}", l.action, l.from, l.to)
                    })
                    .collect();
                (l, None)
            }
        },
        ConfigFile::Agents => match agents_config::effective(current) {
            Err(e) => (vec![], Some(e)),
            Ok(new) => {
                let old = agents_config::effective(confirmed).unwrap_or_default();
                (agents_config::loosenings(&old, &new), None)
            }
        },
    };
    Evaluation {
        diff,
        loosenings,
        invalid,
    }
}

fn parse_policy(text: Option<&str>) -> Result<Policy, String> {
    match text {
        None => Ok(Policy::default()),
        Some(t) => Policy::from_toml(t),
    }
}

/// The policy in force: the confirmed one, and the current file when it differs.
pub struct GuardedPolicy {
    pub confirmed: Policy,
    /// `None`: the file is the confirmed version. `Some(Err)`: it doesn't parse.
    pub edited: Option<Result<Policy, String>>,
}

impl GuardedPolicy {
    pub fn new(confirmed: Option<&str>, current: Option<&str>) -> Self {
        Self {
            confirmed: parse_policy(confirmed).unwrap_or_default(),
            edited: (confirmed != current).then(|| parse_policy(current)),
        }
    }

    pub fn decide(&self, action: &str, project: Option<&str>) -> (Verdict, Option<String>) {
        match &self.edited {
            None => (self.confirmed.decide(action, project), None),
            Some(Ok(edited)) => (
                self.confirmed.decide_stricter(edited, action, project),
                Some("an unconfirmed edit of policies.toml applies only where it tightens".into()),
            ),
            Some(Err(e)) => {
                let mut v = self.confirmed.decide(action, project);
                v.decision = v.decision.max(Decision::Ask);
                (
                    v,
                    Some(format!("policies.toml doesn't parse ({e}): at least ASK")),
                )
            }
        }
    }
}

/// The agent list in force, and what an unconfirmed edit would loosen.
pub fn guarded_agents(
    confirmed: Option<&str>,
    current: Option<&str>,
) -> Result<(Vec<AgentSpec>, Vec<String>), String> {
    let old = agents_config::effective(confirmed)?;
    match agents_config::effective(current) {
        Err(_) => Ok((
            old,
            vec!["agents.toml doesn't parse; the confirmed list applies".into()],
        )),
        Ok(new) => {
            let loose = agents_config::loosenings(&old, &new);
            if loose.is_empty() {
                Ok((new, loose))
            } else {
                Ok((agents_config::stricter(&old, &new), loose))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASK_STOP: &str = "[global]\n\"session.kill\" = \"allow\"\n";
    const ALLOW_STOP: &str =
        "[global]\n\"session.kill\" = \"allow\"\n\"daemon.stop\" = \"allow\"\n";

    #[test]
    fn an_edit_allowing_daemon_stop_loosens_and_waits() {
        let e = evaluate(ConfigFile::Policies, Some(ASK_STOP), Some(ALLOW_STOP));
        assert_eq!(e.diff, "+\"daemon.stop\" = \"allow\"");
        assert!(
            e.loosenings.iter().any(|l| l.starts_with("daemon.stop")),
            "{e:?}"
        );
        let g = GuardedPolicy::new(Some(ASK_STOP), Some(ALLOW_STOP));
        assert_eq!(g.decide("daemon.stop", None).0.decision, Decision::Ask);
        assert_eq!(g.decide("session.kill", None).0.decision, Decision::Allow);
    }

    #[test]
    fn a_tightening_edit_is_not_a_loosening() {
        let tight = "[global]\n\"session.kill\" = \"ask\"\n";
        assert!(evaluate(ConfigFile::Policies, Some(ASK_STOP), Some(tight))
            .loosenings
            .is_empty());
    }

    #[test]
    fn deleting_the_file_can_loosen() {
        let deny = "[global]\n\"session.create\" = \"deny\"\n";
        assert!(!evaluate(ConfigFile::Policies, Some(deny), None)
            .loosenings
            .is_empty());
    }

    #[test]
    fn an_unparseable_policy_fails_closed() {
        let g = GuardedPolicy::new(Some(ALLOW_STOP), Some("not [ toml"));
        assert_eq!(g.decide("session.kill", None).0.decision, Decision::Ask);
        let e = evaluate(ConfigFile::Policies, Some(ALLOW_STOP), Some("not [ toml"));
        assert!(e.invalid.is_some());
    }

    #[test]
    fn agents_loosening_keeps_the_confirmed_entry() {
        let widen = "[[agent]]\nname = \"claude\"\ncommand = \"claude\"\ntested = \"1.0.0..9.0.0\"\nmissing = \"warn\"\n";
        let (list, loose) = guarded_agents(None, Some(widen)).unwrap();
        assert!(!loose.is_empty());
        let claude = list.iter().find(|a| a.name == "claude").unwrap();
        assert_eq!(claude.tested.as_ref().unwrap().to_string(), "2.1.281");
    }
}
