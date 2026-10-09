//! Policy engine (ARCHITECTURE §6.3, phase1-plan Slice D): typed actions, rules from
//! `policies.toml`, a verdict per request. Pure: reading the file and asking the user
//! happen in the daemon.
//!
//! Resolution, most specific first: the project's rules, then global rules, then the
//! built-in defaults; unknown actions are ASK. Within one scope an exact action beats
//! a wildcard (`git.*`), and a longer wildcard beats a shorter one. Then three limits
//! no rule can lift:
//! - a project rule may tighten a global DENY but never loosen it;
//! - system changes (`system.*`) are never ALLOW: an ALLOW becomes ASK;
//! - destructive actions are ASK under a wildcard ALLOW; only an exact per-action
//!   rule can allow them.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod compare;
#[cfg(test)]
mod tests;

pub use compare::Loosening;

/// The typed actions Phase 1 checks, and the hard-classified ones listed ahead of
/// the features that will use them.
pub mod actions {
    pub const SESSION_CREATE: &str = "session.create";
    pub const SESSION_KILL: &str = "session.kill";
    pub const DAEMON_STOP: &str = "daemon.stop";
    pub const PROJECT_REGISTER: &str = "project.register";
    pub const PROJECT_REMOVE: &str = "project.remove";
    pub const SYSTEM_ELEVATE: &str = "system.elevate";
    pub const SYSTEM_ENV_WRITE: &str = "system.env.write";
    pub const SYSTEM_REGISTRY_WRITE: &str = "system.registry.write";
    pub const GIT_RESET_HARD: &str = "git.reset.hard";
    pub const GIT_CLEAN: &str = "git.clean";
    pub const GIT_PUSH_FORCE: &str = "git.push.force";
    pub const FS_DELETE_UNTRACKED: &str = "fs.delete.untracked";
}

/// Destructive whatever a broad rule says (ARCHITECTURE §6.3).
pub const DESTRUCTIVE: &[&str] = &[
    actions::GIT_RESET_HARD,
    actions::GIT_CLEAN,
    actions::GIT_PUSH_FORCE,
    actions::FS_DELETE_UNTRACKED,
];

/// Changes to the machine itself: always ASK, the exact commands shown first
/// (reviewer, 2026-09-25).
const SYSTEM_PREFIX: &str = "system.";

/// Built-in defaults, used where no rule matches.
const DEFAULTS: &[(&str, Decision)] = &[
    (actions::SESSION_CREATE, Decision::Allow),
    // Ending a shell loses its state: confirm (Slice C review, 2026-10-05).
    (actions::SESSION_KILL, Decision::Ask),
    // Stopping the daemon ends every shell (phase1-plan §2).
    (actions::DAEMON_STOP, Decision::Ask),
    (actions::PROJECT_REGISTER, Decision::Allow),
    // Forgets the registration only; nothing on disk is touched.
    (actions::PROJECT_REMOVE, Decision::Allow),
];

/// Ordered by strictness: `Allow < Ask < Deny`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Ask,
    Deny,
}

impl Decision {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "allow" => Some(Self::Allow),
            "ask" => Some(Self::Ask),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }
}

/// Which rule produced a verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleSource {
    /// `[project.<name>]`, with the matching pattern.
    Project(String, String),
    /// `[global]`, with the matching pattern.
    Global(String),
    Default,
    /// No rule and no default: ASK.
    Unknown,
}

impl RuleSource {
    /// For the `policy.decision` event and prompts.
    pub fn describe(&self) -> String {
        match self {
            Self::Project(p, pat) => format!("project {p}: {pat}"),
            Self::Global(pat) => format!("global: {pat}"),
            Self::Default => "built-in default".into(),
            Self::Unknown => "unknown action".into(),
        }
    }

    fn pattern(&self) -> Option<&str> {
        match self {
            Self::Project(_, p) | Self::Global(p) => Some(p),
            Self::Default | Self::Unknown => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub decision: Decision,
    pub rule: RuleSource,
    /// Set when a hard limit changed what the rule said.
    pub limited_by: Option<&'static str>,
}

type Rules = BTreeMap<String, Decision>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    global: Rules,
    projects: BTreeMap<String, Rules>,
}

impl Policy {
    /// Parse `policies.toml`:
    ///
    /// ```toml
    /// [global]
    /// "session.kill" = "allow"
    /// [project.web]
    /// "session.*" = "ask"
    /// ```
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let doc: toml::Table = text
            .parse()
            .map_err(|e: toml::de::Error| format!("not valid TOML: {}", e.message()))?;
        let mut policy = Self::default();
        for (key, value) in &doc {
            match key.as_str() {
                "global" => policy.global = rules("[global]", value)?,
                "project" => {
                    let table = value
                        .as_table()
                        .ok_or("[project] must hold tables like [project.<name>]")?;
                    for (name, rules_value) in table {
                        let rules = rules(&format!("[project.{name}]"), rules_value)?;
                        policy.projects.insert(name.clone(), rules);
                    }
                }
                other => {
                    return Err(format!(
                        "unknown section [{other}] (expected [global] or [project.<name>])"
                    ))
                }
            }
        }
        Ok(policy)
    }

    pub fn decide(&self, action: &str, project: Option<&str>) -> Verdict {
        let global = best_match(&self.global, action);
        let in_project = project.and_then(|p| {
            let rules = self.projects.get(p)?;
            best_match(rules, action).map(|(pat, d)| (p, pat, d))
        });
        let (mut decision, rule) = match (in_project, &global) {
            (Some((p, pat, d)), _) => (d, RuleSource::Project(p.into(), pat.into())),
            (None, Some((pat, d))) => (*d, RuleSource::Global((*pat).into())),
            (None, None) => match DEFAULTS.iter().find(|(a, _)| *a == action) {
                Some((_, d)) => (*d, RuleSource::Default),
                None => (Decision::Ask, RuleSource::Unknown),
            },
        };
        // A narrower scope may tighten a global DENY, never loosen it.
        if let Some((pat, Decision::Deny)) = global {
            if decision < Decision::Deny {
                return Verdict {
                    decision: Decision::Deny,
                    rule: RuleSource::Global(pat.into()),
                    limited_by: Some("a project rule can't loosen a global DENY"),
                };
            }
        }
        let mut limited_by = None;
        if decision == Decision::Allow && action.starts_with(SYSTEM_PREFIX) {
            decision = Decision::Ask;
            limited_by = Some("changes to the system are always ASK");
        }
        if decision == Decision::Allow
            && DESTRUCTIVE.contains(&action)
            && rule.pattern() != Some(action)
        {
            decision = Decision::Ask;
            limited_by = Some("destructive: only an explicit per-action rule can allow it");
        }
        Verdict {
            decision,
            rule,
            limited_by,
        }
    }
}

fn rules(section: &str, value: &toml::Value) -> Result<Rules, String> {
    let table = value
        .as_table()
        .ok_or_else(|| format!("{section} must be a table of \"action\" = \"allow|ask|deny\""))?;
    let mut out = Rules::new();
    for (pattern, v) in table {
        if !valid_pattern(pattern) {
            return Err(format!(
                "{section}: {pattern:?} is not an action (like \"session.kill\") or a wildcard (like \"git.*\")"
            ));
        }
        let d = v.as_str().and_then(Decision::parse).ok_or_else(|| {
            format!("{section}: {pattern:?} = {v} (expected \"allow\", \"ask\" or \"deny\")")
        })?;
        out.insert(pattern.clone(), d);
    }
    Ok(out)
}

/// `a.b.c` or `a.b.*`: lowercase segments; a bare `*` is too broad to be allowed.
fn valid_pattern(p: &str) -> bool {
    let body = p.strip_suffix(".*").unwrap_or(p);
    !body.is_empty()
        && body.split('.').all(|seg| {
            let mut chars = seg.chars();
            chars.next().is_some_and(|c| c.is_ascii_lowercase())
                && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
}

/// The most specific rule for `action`: exact first, then the longest wildcard.
fn best_match<'r>(rules: &'r Rules, action: &str) -> Option<(&'r str, Decision)> {
    if let Some((pat, d)) = rules.get_key_value(action) {
        return Some((pat, *d));
    }
    rules
        .iter()
        .filter_map(|(pat, d)| {
            let prefix = pat.strip_suffix(".*")?;
            let rest = action.strip_prefix(prefix)?;
            rest.starts_with('.')
                .then_some((pat.as_str(), *d, prefix.len()))
        })
        .max_by_key(|(_, _, len)| *len)
        .map(|(pat, d, _)| (pat, d))
}
