//! The error shape used from Rust to the UI (ARCHITECTURE §14).

use serde::{Deserialize, Serialize};
use std::fmt;

/// A user-facing error: what failed, where, why, what was tried, and what to do next.
/// Serializable so it crosses the IPC boundary unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct YolmiraError {
    pub what: String,
    #[serde(rename = "where")]
    pub where_: String,
    pub why: String,
    #[serde(default)]
    pub attempted: Vec<String>,
    #[serde(default)]
    pub next_steps: Vec<String>,
}

impl YolmiraError {
    pub fn new(what: impl Into<String>, where_: impl Into<String>, why: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            where_: where_.into(),
            why: why.into(),
            attempted: Vec::new(),
            next_steps: Vec::new(),
        }
    }

    pub fn attempted(self, step: impl Into<String>) -> Self {
        let mut attempted = self.attempted;
        attempted.push(step.into());
        Self { attempted, ..self }
    }

    pub fn next_step(self, step: impl Into<String>) -> Self {
        let mut next_steps = self.next_steps;
        next_steps.push(step.into());
        Self { next_steps, ..self }
    }

    /// §14 rule: user-facing errors must say why and what to do next.
    pub fn is_actionable(&self) -> bool {
        !self.why.trim().is_empty() && !self.next_steps.is_empty()
    }
}

impl fmt::Display for YolmiraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}): {}", self.what, self.where_, self.why)?;
        for step in &self.attempted {
            write!(f, "\n  tried: {step}")?;
        }
        for step in &self.next_steps {
            write!(f, "\n  next: {step}")?;
        }
        Ok(())
    }
}

impl std::error::Error for YolmiraError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_keeps_earlier_steps_and_is_actionable_only_with_next_steps() {
        let e = YolmiraError::new("start daemon", "launcher", "access denied");
        assert!(!e.is_actionable());
        let e = e
            .attempted("CreateProcessW")
            .next_step("open a new terminal")
            .next_step("run yolmirad");
        assert_eq!(e.attempted, vec!["CreateProcessW"]);
        assert_eq!(e.next_steps.len(), 2);
        assert!(e.is_actionable());
    }

    #[test]
    fn serializes_where_without_underscore() {
        let json = serde_json::to_value(YolmiraError::new("a", "b", "c")).unwrap();
        assert_eq!(json["where"], "b");
    }
}
