//! Guards `policies.toml` and `agents.toml` against edits that loosen them (reviewer,
//! 2026-10-09): agents run as the user and could otherwise grant themselves any
//! action by editing a file. Rules in `crate::config_files`; here the state:
//!
//! - The confirmed text of each file lives in the event log
//!   (`policy.config_accepted`, full content). On an instance's first start, or the
//!   guard's first start on an older install, the file as found is confirmed (trust
//!   on first use; logged with that reason).
//! - Every change is logged with a diff (`policy.config_changed`). A tightening-only
//!   edit is confirmed at once. A loosening one opens a confirmation in the approval
//!   machinery (`policy.rs`): the UI shows it with its one-time code; agents can't
//!   answer it; until it is confirmed, the stricter of the two versions applies.
//! - Residual: a same-user process could also edit the event log itself; agents at
//!   low integrity (spike S7, risk R14) can write neither.

use super::policy;
use super::state::State;
use crate::config_files::{self, evaluate, ConfigFile, GuardedPolicy};
use amiri_core::event::kinds;
use amiri_core::NewEvent;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, PoisonError};

#[derive(Default)]
struct FileState {
    confirmed: Option<String>,
    /// The text last looked at (`None`: not looked at yet).
    seen: Option<Option<String>>,
    /// The open confirmation for a loosening edit.
    pending: Option<String>,
    /// Texts a person declined: not asked again while the file keeps them.
    declined: HashSet<Option<String>>,
}

#[derive(Default)]
pub struct ConfigGuard {
    files: Mutex<HashMap<ConfigFile, FileState>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn record(state: &State, kind: &'static str, payload: serde_json::Value) {
    if let Err(e) = state.record(NewEvent::new(kind, "amiri", payload)) {
        tracing::error!(error = %e, kind, "could not record a config event");
    }
}

impl ConfigGuard {
    /// Load the confirmed versions from the event log; confirm the files as found
    /// where none was ever recorded.
    pub fn bootstrap(&self, state: &State) {
        let events = state
            .with_store(|s| s.list(0, u32::MAX, Some(config_files::ACCEPTED_KIND)))
            .unwrap_or_default();
        let dir = state.instance.config_dir();
        for file in ConfigFile::ALL {
            let confirmed = match config_files::confirmed_in(&events, file) {
                Some(c) => c,
                None => {
                    let found = file.read(&dir).unwrap_or(None);
                    record(
                        state,
                        kinds::POLICY_CONFIG_ACCEPTED,
                        json!({ "file": file.file_name(), "content": found, "reason": "first start: the file as found (trust on first use)" }),
                    );
                    found
                }
            };
            lock(&self.files).insert(
                file,
                FileState {
                    confirmed,
                    ..FileState::default()
                },
            );
        }
        self.refresh_all(state);
    }

    pub fn refresh_all(&self, state: &State) {
        for file in ConfigFile::ALL {
            self.refresh(state, file);
        }
    }

    /// Look at the file; log and handle a change since last time.
    fn refresh(&self, state: &State, file: ConfigFile) {
        let current = match file.read(&state.instance.config_dir()) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(file = file.file_name(), error = %e, "config file unreadable");
                return;
            }
        };
        let (confirmed, close) = {
            let mut files = lock(&self.files);
            let f = files.entry(file).or_default();
            if f.seen.as_ref() == Some(&current) {
                return;
            }
            f.seen = Some(current.clone());
            (f.confirmed.clone(), f.pending.take())
        };
        if let Some(id) = close {
            policy::close_confirmation(state, &id);
        }
        if current == confirmed {
            return;
        }
        let e = evaluate(file, confirmed.as_deref(), current.as_deref());
        let name = file.file_name();
        if let Some(why) = &e.invalid {
            record(
                state,
                kinds::POLICY_CONFIG_CHANGED,
                json!({ "file": name, "diff": e.diff, "invalid": why, "applied": false }),
            );
            return;
        }
        let loosens = !e.loosenings.is_empty();
        record(
            state,
            kinds::POLICY_CONFIG_CHANGED,
            json!({ "file": name, "diff": e.diff, "loosenings": e.loosenings, "applied": !loosens }),
        );
        if !loosens {
            self.accept(state, file, current, "an edit that only tightens");
            return;
        }
        if lock(&self.files)
            .get(&file)
            .is_some_and(|f| f.declined.contains(&current))
        {
            return;
        }
        let id = policy::open_confirmation(state, file, current, &e);
        lock(&self.files).entry(file).or_default().pending = Some(id);
    }

    /// Make `content` the confirmed version.
    pub fn accept(&self, state: &State, file: ConfigFile, content: Option<String>, reason: &str) {
        record(
            state,
            kinds::POLICY_CONFIG_ACCEPTED,
            json!({ "file": file.file_name(), "content": content, "reason": reason }),
        );
        let mut files = lock(&self.files);
        let f = files.entry(file).or_default();
        f.confirmed = content;
        f.pending = None;
    }

    /// A person declined the edit: the confirmed version stays; not asked again
    /// while the file keeps this text.
    pub fn decline(&self, state: &State, file: ConfigFile, content: Option<String>, by: &str) {
        record(
            state,
            kinds::POLICY_CONFIG_REJECTED,
            json!({ "file": file.file_name(), "by": by }),
        );
        let mut files = lock(&self.files);
        let f = files.entry(file).or_default();
        f.declined.insert(content);
        f.pending = None;
    }

    /// The policy in force for a check, after looking at the file.
    pub fn policy(&self, state: &State) -> GuardedPolicy {
        self.refresh(state, ConfigFile::Policies);
        let files = lock(&self.files);
        let f = files.get(&ConfigFile::Policies);
        let confirmed = f.and_then(|f| f.confirmed.clone());
        let current = f
            .and_then(|f| f.seen.clone())
            .unwrap_or_else(|| confirmed.clone());
        GuardedPolicy::new(confirmed.as_deref(), current.as_deref())
    }
}
