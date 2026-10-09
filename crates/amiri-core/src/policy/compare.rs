//! Is an edited `policies.toml` looser than the confirmed one? (reviewer, 2026-10-09:
//! agents run as the user and can edit the file, so a loosening edit waits for a
//! person's confirmation; a tightening one applies at once.)
//!
//! Compared by behaviour, not text: every action the rules could affect (the known
//! actions, every pattern either file names, a representative action under each
//! wildcard) is decided under both, globally and in every project either file names.
//! Any decision that gets less strict is a loosening.

use super::{actions, Decision, Policy, Verdict, DESTRUCTIVE};
use std::collections::BTreeSet;

/// One decision the edit makes less strict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loosening {
    pub action: String,
    pub project: Option<String>,
    pub from: Decision,
    pub to: Decision,
}

const KNOWN: &[&str] = &[
    actions::SESSION_CREATE,
    actions::SESSION_KILL,
    actions::DAEMON_STOP,
    actions::PROJECT_REGISTER,
    actions::PROJECT_REMOVE,
    actions::SYSTEM_ELEVATE,
    actions::SYSTEM_ENV_WRITE,
    actions::SYSTEM_REGISTRY_WRITE,
];

impl Policy {
    /// The decisions `new` makes less strict than `self`.
    pub fn loosenings(&self, new: &Policy) -> Vec<Loosening> {
        let mut actions: BTreeSet<String> = KNOWN
            .iter()
            .chain(DESTRUCTIVE)
            .map(|a| a.to_string())
            .collect();
        for p in self.patterns().chain(new.patterns()) {
            match p.strip_suffix(".*") {
                // Something only the wildcard covers, and the prefix's own name.
                Some(prefix) => {
                    actions.insert(format!("{prefix}.any_action"));
                }
                None => {
                    actions.insert(p.to_string());
                }
            }
        }
        let projects: BTreeSet<Option<String>> = std::iter::once(None)
            .chain(
                self.projects
                    .keys()
                    .chain(new.projects.keys())
                    .map(|p| Some(p.clone())),
            )
            .collect();
        let mut out = Vec::new();
        for project in &projects {
            for action in &actions {
                let from = self.decide(action, project.as_deref()).decision;
                let to = new.decide(action, project.as_deref()).decision;
                if to < from {
                    out.push(Loosening {
                        action: action.clone(),
                        project: project.clone(),
                        from,
                        to,
                    });
                }
            }
        }
        out
    }

    fn patterns(&self) -> impl Iterator<Item = &str> {
        self.global
            .keys()
            .chain(self.projects.values().flat_map(|r| r.keys()))
            .map(String::as_str)
    }

    /// While a loosening edit waits for confirmation: each decision is the stricter
    /// of the confirmed rules and the edited ones (so tightening applies at once).
    pub fn decide_stricter(&self, other: &Policy, action: &str, project: Option<&str>) -> Verdict {
        let a = self.decide(action, project);
        let b = other.decide(action, project);
        if b.decision > a.decision {
            b
        } else {
            a
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(toml: &str) -> Policy {
        Policy::from_toml(toml).unwrap()
    }

    #[test]
    fn allowing_daemon_stop_loosens() {
        let l = Policy::default().loosenings(&p("[global]\n\"daemon.stop\" = \"allow\"\n"));
        assert_eq!(l.len(), 1, "{l:?}");
        assert_eq!(
            (l[0].action.as_str(), l[0].from, l[0].to),
            ("daemon.stop", Decision::Ask, Decision::Allow)
        );
    }

    #[test]
    fn tightening_and_no_change_are_not_loosening() {
        let base = p("[global]\n\"session.create\" = \"allow\"\n");
        assert!(base
            .loosenings(&p("[global]\n\"session.create\" = \"deny\"\n"))
            .is_empty());
        assert!(base.loosenings(&base).is_empty());
        // Removing an ALLOW falls back to the default ALLOW: no change in behaviour.
        assert!(base.loosenings(&Policy::default()).is_empty());
    }

    #[test]
    fn removing_a_deny_loosens() {
        let base = p("[global]\n\"session.create\" = \"deny\"\n");
        assert!(!base.loosenings(&Policy::default()).is_empty());
    }

    #[test]
    fn a_wildcard_allow_loosens_what_it_covers() {
        let l = Policy::default().loosenings(&p("[global]\n\"session.*\" = \"allow\"\n"));
        assert!(l.iter().any(|x| x.action == "session.kill"), "{l:?}");
        assert!(
            l.iter().any(|x| x.action == "session.any_action"),
            "unknown actions under it too: {l:?}"
        );
    }

    #[test]
    fn a_project_rule_loosens_in_that_project_only() {
        let l = Policy::default().loosenings(&p("[project.web]\n\"session.kill\" = \"allow\"\n"));
        assert_eq!(l.len(), 1, "{l:?}");
        assert_eq!(l[0].project.as_deref(), Some("web"));
    }

    #[test]
    fn hard_limits_mean_some_edits_change_nothing() {
        // system.* can't become ALLOW, so allowing it is no loosening at all.
        assert!(Policy::default()
            .loosenings(&p("[global]\n\"system.elevate\" = \"allow\"\n"))
            .is_empty());
    }

    #[test]
    fn stricter_of_two_policies_applies_the_tightening_only() {
        let confirmed = p("[global]\n\"session.create\" = \"allow\"\n");
        let edited = p("[global]\n\"session.create\" = \"deny\"\n\"daemon.stop\" = \"allow\"\n");
        let d = |a: &str| confirmed.decide_stricter(&edited, a, None).decision;
        assert_eq!(d("session.create"), Decision::Deny, "tightening applies");
        assert_eq!(d("daemon.stop"), Decision::Ask, "loosening waits");
    }
}
