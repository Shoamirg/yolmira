//! The policy gate (ARCHITECTURE §6.3, phase1-plan Slice D). Mutating requests that
//! map to a typed action call `check` before doing anything.
//!
//! - `policies.toml` is read on every check, so an edit applies at once. A file that
//!   doesn't parse makes every action ASK (fail closed) until it is fixed.
//! - ASK records `policy.ask` and holds the request until a client answers with
//!   `policy.answer` (once, or for the rest of this daemon run), or `ASK_TIMEOUT`
//!   passes, which counts as DENY. The waiting connection is blocked; answers come
//!   from another connection (the UI, `amiri approve`, the CLI's own prompt).
//! - Every verdict is a `policy.decision` event, automatic ones included.
//!
//! Who may answer (reviewer, 2026-10-09: agents run as the user and can read the
//! pipe token, so they must not approve their own requests):
//! - nobody inside an agent job (`peer.rs`), whatever else it presents;
//! - the genuine UI, directly;
//! - anyone else only with the request's one-time code, which the daemon hands to
//!   the genuine UI alone (`policy.code`) and never writes to an event. Three wrong
//!   codes deny the request.

use super::auth::constant_time_eq;
use super::peer::{is_trusted_ui, Peer};
use super::state::State;
use amiri_core::event::kinds;
use amiri_core::policy::{Decision, Policy, Verdict};
use amiri_core::{AmiriError, NewEvent};
use amiri_ipc::protocol::{
    codes, AnswerDecision, AnswerScope, PendingAsk, PolicyAnswerParams, PolicyIdParams, RpcError,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::sync::oneshot;

/// How long a request waits for an answer before it is denied.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(600);
/// Wrong codes allowed per request before it is denied.
const MAX_CODE_ATTEMPTS: u32 = 3;
const WHERE: &str = "amirid policy";

struct Answer {
    decision: AnswerDecision,
    scope: AnswerScope,
    by: String,
}

struct Waiting {
    ask: PendingAsk,
    /// Never in an event; only the genuine UI may read it.
    code: String,
    wrong_codes: u32,
    reply: oneshot::Sender<Answer>,
}

#[derive(Default)]
pub struct Gate {
    waiting: Mutex<HashMap<String, Waiting>>,
    /// (action, project) allowed for the rest of this daemon run.
    allowed_for_run: Mutex<HashSet<(String, Option<String>)>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One gated request.
pub struct Check<'a> {
    pub action: &'static str,
    pub project: Option<String>,
    /// What would happen, shown in the prompt and kept in the events.
    pub detail: Value,
    pub client: &'a str,
    pub connection: u64,
}

impl Gate {
    pub fn pending(&self) -> Vec<PendingAsk> {
        let mut asks: Vec<_> = lock(&self.waiting)
            .values()
            .map(|w| w.ask.clone())
            .collect();
        asks.sort_by(|a, b| a.id.cmp(&b.id));
        asks
    }
}

/// Eight random digits (from the OS RNG, via a v4 UUID).
fn new_code() -> String {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let n = u64::from_le_bytes(bytes[..8].try_into().unwrap_or([0; 8]));
    format!("{:08}", n % 100_000_000)
}

/// The policy in force, or why the file can't be used (then everything is ASK).
fn load(state: &State) -> Result<Policy, String> {
    let path = state.instance.policies_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => Policy::from_toml(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Policy::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn verdict(state: &State, c: &Check<'_>) -> (Decision, String, Option<&'static str>) {
    match load(state) {
        Ok(policy) => {
            let Verdict {
                decision,
                rule,
                limited_by,
            } = policy.decide(c.action, c.project.as_deref());
            (decision, rule.describe(), limited_by)
        }
        Err(e) => (
            Decision::Ask,
            format!("policies.toml is invalid ({e})"),
            None,
        ),
    }
}

struct Outcome {
    allow: bool,
    rule: String,
    by: Option<String>,
}

/// Decide, asking a client if the rules say ASK. `Ok` means go ahead.
pub async fn check(state: &Arc<State>, c: Check<'_>) -> Result<(), RpcError> {
    let (decision, rule, limited_by) = verdict(state, &c);
    let key = (c.action.to_string(), c.project.clone());
    let decided = match decision {
        Decision::Ask if lock(&state.policy.allowed_for_run).contains(&key) => Outcome {
            allow: true,
            rule: format!("{rule}; allowed for this daemon run"),
            by: None,
        },
        Decision::Ask => ask(state, &c, &rule).await?,
        other => Outcome {
            allow: other == Decision::Allow,
            rule,
            by: None,
        },
    };
    record_decision(state, &c, &decided, limited_by)?;
    if decided.allow {
        Ok(())
    } else {
        Err(denied(&c, &decided.rule))
    }
}

/// Removes the waiting entry however the wait ends (answer, timeout, cancelled).
struct Unregister<'a> {
    state: &'a State,
    id: String,
}

impl Drop for Unregister<'_> {
    fn drop(&mut self) {
        lock(&self.state.policy.waiting).remove(&self.id);
    }
}

async fn ask(state: &Arc<State>, c: &Check<'_>, rule: &str) -> Result<Outcome, RpcError> {
    let ask = PendingAsk {
        id: uuid::Uuid::now_v7().to_string(),
        action: c.action.into(),
        project: c.project.clone(),
        detail: c.detail.clone(),
        rule: rule.into(),
        client: c.client.into(),
        connection: c.connection,
        person_id: Some(state.person.person_id.clone()),
        // Phase 1 has no agents; Phase 4 sets the requesting agent here.
        agent_id: None,
        asked_at: amiri_core::time::now_rfc3339(),
    };
    let (reply, answer) = oneshot::channel();
    let id = ask.id.clone();
    let waiting = Waiting {
        ask: ask.clone(),
        code: new_code(),
        wrong_codes: 0,
        reply,
    };
    lock(&state.policy.waiting).insert(id.clone(), waiting);
    let _unregister = Unregister { state, id };
    let event = NewEvent::new(
        kinds::POLICY_ASK,
        format!("client:{}", c.client),
        json!(ask),
    );
    state.record(event).map_err(RpcError::amiri)?;
    Ok(match tokio::time::timeout(ASK_TIMEOUT, answer).await {
        Ok(Ok(a)) => {
            let allow = a.decision == AnswerDecision::Allow;
            if allow && a.scope == AnswerScope::Session {
                let key = (c.action.to_string(), c.project.clone());
                lock(&state.policy.allowed_for_run).insert(key);
            }
            let scope = match (allow, a.scope) {
                (false, _) => "denied",
                (true, AnswerScope::Once) => "once",
                (true, AnswerScope::Session) => "for this daemon run",
            };
            Outcome {
                allow,
                rule: format!("answered by {} ({scope})", a.by),
                by: Some(a.by),
            }
        }
        // The daemon is shutting down and dropped the waiting entry.
        Ok(Err(_)) => Outcome {
            allow: false,
            rule: "the request was cancelled before anyone answered".into(),
            by: None,
        },
        Err(_) => Outcome {
            allow: false,
            rule: format!("no answer within {} s", ASK_TIMEOUT.as_secs()),
            by: None,
        },
    })
}

fn record_decision(
    state: &State,
    c: &Check<'_>,
    o: &Outcome,
    limited_by: Option<&'static str>,
) -> Result<(), RpcError> {
    let payload = json!({
        "action": c.action,
        "project": c.project,
        "result": if o.allow { "allow" } else { "deny" },
        "rule": o.rule,
        "limited_by": limited_by,
        "answered_by": o.by,
        "requested_by": c.client,
        "requested_by_person": state.person.person_id,
        "requested_by_agent": Value::Null,
    });
    let event = NewEvent::new(
        kinds::POLICY_DECISION,
        format!("client:{}", c.client),
        payload,
    );
    state.record(event).map(|_| ()).map_err(RpcError::amiri)
}

fn denied(c: &Check<'_>, rule: &str) -> RpcError {
    let mut e = RpcError::amiri(
        AmiriError::new(
            format!("Run {}", c.action),
            WHERE,
            format!("denied: {rule}"),
        )
        .next_step("Ask again and approve it, or change the rule in policies.toml."),
    );
    e.code = codes::POLICY_DENIED;
    e
}

fn forbidden(what: &str, why: &str, next: &str) -> RpcError {
    let mut e = RpcError::amiri(AmiriError::new(what, WHERE, why).next_step(next));
    e.code = codes::FORBIDDEN;
    e
}

/// A refused answer or code request, as an event (never containing the code).
fn record_refusal(state: &State, client: &str, peer: &Peer, id: &str, reason: &str) {
    let payload = json!({
        "id": id,
        "reason": reason,
        "client": client,
        "pid": peer.pid,
        "image": peer.image().map(|p| p.display().to_string()),
    });
    let event = NewEvent::new(
        kinds::POLICY_ANSWER_REFUSED,
        format!("client:{client}"),
        payload,
    );
    if let Err(e) = state.record(event) {
        tracing::error!(error = %e, "could not record policy.answer_refused");
    }
    tracing::warn!(id, reason, client, pid = ?peer.pid, "policy answer refused");
}

const AGENT_REFUSAL: &str =
    "this process runs inside an AMIRI agent job; agents can't approve requests";

/// `policy.code`: the one-time code of a waiting request, for the genuine UI only.
pub fn code(
    state: &State,
    p: PolicyIdParams,
    client: &str,
    peer: &Peer,
) -> Result<Value, RpcError> {
    const WHAT: &str = "Show the approval code";
    if state.agent_jobs.contains(peer) {
        record_refusal(state, client, peer, &p.id, "agent_job");
        return Err(forbidden(
            WHAT,
            AGENT_REFUSAL,
            "Approve in the AMIRI window.",
        ));
    }
    if !is_trusted_ui(peer, &state.agent_jobs, &state.instance.config_dir()) {
        record_refusal(state, client, peer, &p.id, "not_the_ui");
        return Err(forbidden(
            WHAT,
            "only the AMIRI window may show approval codes",
            "Open the AMIRI window to see the code.",
        ));
    }
    let waiting = lock(&state.policy.waiting);
    let w = waiting.get(&p.id).ok_or_else(|| not_waiting(&p.id))?;
    Ok(json!({ "id": p.id, "code": w.code }))
}

fn not_waiting(id: &str) -> RpcError {
    RpcError::amiri(
        AmiriError::new("Answer the request", WHERE, format!("nothing waits as {id}")).next_step(
            "It was answered already, timed out, or its client went away; see `amiri approve --list`.",
        ),
    )
}

/// `policy.answer`: check who answers, record it, then release the waiting request.
pub fn answer(
    state: &State,
    p: PolicyAnswerParams,
    client: &str,
    peer: &Peer,
) -> Result<Value, RpcError> {
    const WHAT: &str = "Answer the request";
    if state.agent_jobs.contains(peer) {
        record_refusal(state, client, peer, &p.id, "agent_job");
        return Err(forbidden(
            WHAT,
            AGENT_REFUSAL,
            "Approve in the AMIRI window.",
        ));
    }
    let trusted_ui = is_trusted_ui(peer, &state.agent_jobs, &state.instance.config_dir());
    let mut waiting = lock(&state.policy.waiting);
    let entry = waiting.get_mut(&p.id).ok_or_else(|| not_waiting(&p.id))?;
    if !trusted_ui {
        let given = p.code.as_deref().unwrap_or("");
        if !constant_time_eq(given.as_bytes(), entry.code.as_bytes()) {
            entry.wrong_codes += 1;
            let exhausted = entry.wrong_codes >= MAX_CODE_ATTEMPTS;
            let reason = if given.is_empty() {
                "no_code"
            } else {
                "wrong_code"
            };
            record_refusal(state, client, peer, &p.id, reason);
            if exhausted {
                // Too many tries: the request is denied, so the code can't be guessed.
                if let Some(w) = waiting.remove(&p.id) {
                    let _ = w.reply.send(Answer {
                        decision: AnswerDecision::Deny,
                        scope: AnswerScope::Once,
                        by: format!("{MAX_CODE_ATTEMPTS} wrong approval codes"),
                    });
                }
            }
            return Err(forbidden(
                WHAT,
                "the approval code is missing or wrong",
                "Use the code shown in the AMIRI window for this request.",
            ));
        }
    }
    let w = waiting.remove(&p.id).ok_or_else(|| not_waiting(&p.id))?;
    drop(waiting);
    let via = if trusted_ui { "ui" } else { "code" };
    let event = NewEvent::new(
        kinds::POLICY_ANSWERED,
        format!("client:{client}"),
        json!({ "id": p.id, "action": w.ask.action, "decision": p.decision, "scope": p.scope, "via": via, "pid": peer.pid, "answered_by_person": state.person.person_id }),
    );
    state.record(event).map_err(RpcError::amiri)?;
    // The requester may have gone meanwhile; its own decision event says so.
    let _ = w.reply.send(Answer {
        decision: p.decision,
        scope: p.scope,
        by: client.into(),
    });
    Ok(json!({ "answered": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_eight_digits_and_vary() {
        let a = new_code();
        assert_eq!(a.len(), 8);
        assert!(a.chars().all(|c| c.is_ascii_digit()));
        let distinct: HashSet<_> = (0..50).map(|_| new_code()).collect();
        assert!(distinct.len() > 45, "codes repeat too often");
    }
}
