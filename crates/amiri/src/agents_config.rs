//! The agent CLIs AMIRI knows, as data (`agents.toml`): a built-in list, extended or
//! overridden by the user's `agents.toml` in the config folder. Shared by `amiri
//! doctor` and the daemon's config guard: an edit that loosens the list (an agent
//! dropped, a tested range widened, a command changed, a warning demoted) waits for
//! a person's confirmation; one that tightens it applies at once.

use serde::Deserialize;

pub const BUILT_IN: &str = include_str!("agents.toml");
pub const USER_FILE: &str = "agents.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Missing {
    /// Ordered by strictness: `Info < Warn`.
    Info,
    Warn,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AgentSpec {
    pub name: String,
    pub command: String,
    #[serde(default = "default_version_args")]
    pub version_args: Vec<String>,
    #[serde(default)]
    pub tested: Option<Range>,
    pub missing: Missing,
}

fn default_version_args() -> Vec<String> {
    vec!["--version".into()]
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    agent: Vec<AgentSpec>,
}

pub type Version = (u64, u64, u64);

/// Tested versions: `"x.y.z"` or `"x.y.z..x.y.z"` (inclusive).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Range {
    text: String,
    min: Version,
    max: Version,
}

impl TryFrom<String> for Range {
    type Error = String;
    fn try_from(text: String) -> Result<Self, String> {
        let (a, b) = text.split_once("..").unwrap_or((&text, &text));
        let parse = |s: &str| {
            parse_version(s).ok_or(format!("{text:?} is not \"x.y.z\" or \"x.y.z..x.y.z\""))
        };
        let (min, max) = (parse(a)?, parse(b)?);
        if min > max {
            return Err(format!("{text:?}: the range is backwards"));
        }
        Ok(Self { text, min, max })
    }
}

impl Range {
    pub fn contains(&self, v: &str) -> bool {
        parse_version(v).is_some_and(|v| self.min <= v && v <= self.max)
    }

    fn within(&self, other: &Range) -> bool {
        other.min <= self.min && self.max <= other.max
    }
}

impl std::fmt::Display for Range {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// The first `x.y.z` in `s` (digits only, so "2.1.281 (Claude Code)" gives 2.1.281).
pub fn parse_version(s: &str) -> Option<Version> {
    s.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find_map(|tok| {
            let mut parts = tok.split('.').filter(|p| !p.is_empty());
            Some((
                parts.next()?.parse().ok()?,
                parts.next()?.parse().ok()?,
                parts.next()?.parse().ok()?,
            ))
        })
}

fn parse(text: &str, from: &str) -> Result<Vec<AgentSpec>, String> {
    toml::from_str::<File>(text)
        .map(|f| f.agent)
        .map_err(|e| format!("{from}: {}", e.message()))
}

/// The built-in list with the user's file applied (same name replaces). `user` is the
/// file's text, `None` when there is no file.
pub fn effective(user: Option<&str>) -> Result<Vec<AgentSpec>, String> {
    let mut list = parse(BUILT_IN, "built-in agents.toml")?;
    if let Some(text) = user {
        for spec in parse(text, USER_FILE)? {
            match list.iter_mut().find(|s| s.name == spec.name) {
                Some(existing) => *existing = spec,
                None => list.push(spec),
            }
        }
    }
    Ok(list)
}

/// Why `new` is looser than `old` for this agent, if it is.
fn looser(old: &AgentSpec, new: &AgentSpec) -> Option<String> {
    let n = &old.name;
    if new.command != old.command || new.version_args != old.version_args {
        return Some(format!("{n}: command changed to {:?}", new.command));
    }
    if new.missing < old.missing {
        return Some(format!("{n}: a missing CLI no longer warns"));
    }
    match (&old.tested, &new.tested) {
        (Some(o), None) => Some(format!("{n}: any version accepted (was {o})")),
        (Some(o), Some(nw)) if !nw.within(o) => {
            Some(format!("{n}: tested range {o} widened to {nw}"))
        }
        _ => None,
    }
}

/// What `new` loosens compared with `old` (both effective lists).
pub fn loosenings(old: &[AgentSpec], new: &[AgentSpec]) -> Vec<String> {
    old.iter()
        .filter_map(|o| match new.iter().find(|n| n.name == o.name) {
            None => Some(format!("{}: no longer checked", o.name)),
            Some(n) => looser(o, n),
        })
        .collect()
}

/// While a loosening edit waits: each entry of the edit applies only where it isn't
/// looser; new agents apply; agents the edit dropped stay.
pub fn stricter(confirmed: &[AgentSpec], edited: &[AgentSpec]) -> Vec<AgentSpec> {
    let mut out: Vec<AgentSpec> = confirmed
        .iter()
        .map(|c| match edited.iter().find(|e| e.name == c.name) {
            Some(e) if looser(c, e).is_none() => e.clone(),
            _ => c.clone(),
        })
        .collect();
    out.extend(
        edited
            .iter()
            .filter(|e| !confirmed.iter().any(|c| c.name == e.name))
            .cloned(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(user: &str) -> Vec<AgentSpec> {
        effective(Some(user)).unwrap()
    }

    #[test]
    fn versions_are_read_from_typical_output() {
        assert_eq!(parse_version("2.1.281 (Claude Code)"), Some((2, 1, 281)));
        assert_eq!(parse_version("codex-cli 0.48.0"), Some((0, 48, 0)));
        assert_eq!(parse_version("no version here 1.2"), None);
    }

    #[test]
    fn ranges_hold_one_version_or_a_span() {
        let one = Range::try_from("2.1.281".to_string()).unwrap();
        assert!(one.contains("2.1.281") && !one.contains("2.1.282"));
        let span = Range::try_from("1.0.0..1.4.9".to_string()).unwrap();
        assert!(span.contains("1.2.3") && !span.contains("1.5.0") && !span.contains("0.9.9"));
        assert!(Range::try_from("2.0.0..1.0.0".to_string()).is_err());
        assert!(Range::try_from("latest".to_string()).is_err());
    }

    #[test]
    fn the_built_in_list_has_claude_and_codex_and_a_user_file_overrides_and_adds() {
        let names = |l: &[AgentSpec]| l.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&effective(None).unwrap()), ["claude", "codex"]);
        let l = list("[[agent]]\nname = \"claude\"\ncommand = \"claude\"\ntested = \"2.1.281\"\nmissing = \"info\"\n\n[[agent]]\nname = \"other\"\ncommand = \"other\"\nmissing = \"info\"\n");
        assert_eq!(names(&l), ["claude", "codex", "other"]);
        assert_eq!(l[0].missing, Missing::Info);
        assert!(effective(Some("[[agent]]\nname = 1\n")).is_err());
    }

    #[test]
    fn loosening_edits_are_recognised() {
        let base = effective(None).unwrap();
        let claude = |rest: &str| {
            list(&format!(
                "[[agent]]\nname = \"claude\"\ncommand = \"claude\"\n{rest}"
            ))
        };
        assert!(
            !loosenings(
                &base,
                &claude("tested = \"2.0.0..2.9.9\"\nmissing = \"warn\"\n")
            )
            .is_empty(),
            "widened"
        );
        assert!(
            !loosenings(&base, &claude("missing = \"warn\"\n")).is_empty(),
            "any version"
        );
        assert!(
            !loosenings(&base, &claude("tested = \"2.1.281\"\nmissing = \"info\"\n")).is_empty(),
            "no warning"
        );
        let moved = list("[[agent]]\nname = \"claude\"\ncommand = 'C:\\x\\claude.exe'\ntested = \"2.1.281\"\nmissing = \"warn\"\n");
        assert!(!loosenings(&base, &moved).is_empty(), "command changed");
        // Adding an agent, or narrowing nothing at all, is not loosening.
        let added =
            list("[[agent]]\nname = \"gemini\"\ncommand = \"gemini\"\nmissing = \"info\"\n");
        assert!(loosenings(&base, &added).is_empty());
        let codex_warns =
            list("[[agent]]\nname = \"codex\"\ncommand = \"codex\"\nmissing = \"warn\"\n");
        assert!(
            loosenings(&base, &codex_warns).is_empty(),
            "info -> warn tightens"
        );
    }

    #[test]
    fn stricter_keeps_the_confirmed_entry_where_the_edit_loosens() {
        let base = effective(None).unwrap();
        let edit = list("[[agent]]\nname = \"claude\"\ncommand = \"claude\"\nmissing = \"info\"\n\n[[agent]]\nname = \"codex\"\ncommand = \"codex\"\nmissing = \"warn\"\n\n[[agent]]\nname = \"new\"\ncommand = \"new\"\nmissing = \"info\"\n");
        let s = stricter(&base, &edit);
        assert_eq!(s[0], base[0], "claude's loosening waits");
        assert_eq!(s[1].missing, Missing::Warn, "codex's tightening applies");
        assert_eq!(s[2].name, "new", "an added agent applies");
    }
}
