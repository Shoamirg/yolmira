//! Shared daemon state. Every state change goes through `record`, which appends the
//! event and broadcasts it while still holding the store lock, so subscribers see
//! events in `seq` order.

use amiri_core::event::{actors, kinds};
use amiri_core::{AmiriError, Event, NewEvent};
use amiri_ipc::Instance;
use amiri_store::Store;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

const BROADCAST_CAPACITY: usize = 1024;
/// At most one `ipc.auth_failed` event per reason in this window; the rest are counted.
const AUTH_FAIL_WINDOW: Duration = Duration::from_secs(10);

pub struct State {
    store: Mutex<Store>,
    events: broadcast::Sender<Event>,
    pub instance: Instance,
    pub token: String,
    pub elevated: bool,
    pub started_at: String,
    clients: AtomicU32,
    next_connection: AtomicU64,
    auth_failures: Mutex<HashMap<String, (Instant, u64)>>,
    pub sessions: super::sessions::Sessions,
    pub agent_jobs: super::peer::AgentJobs,
    pub policy: super::policy::Gate,
}

impl State {
    pub fn new(store: Store, instance: Instance, token: String, elevated: bool) -> Self {
        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            store: Mutex::new(store),
            events,
            instance,
            token,
            elevated,
            started_at: amiri_core::time::now_rfc3339(),
            clients: AtomicU32::new(0),
            next_connection: AtomicU64::new(1),
            auth_failures: Mutex::new(HashMap::new()),
            sessions: super::sessions::Sessions::default(),
            agent_jobs: super::peer::AgentJobs::default(),
            policy: super::policy::Gate::default(),
        }
    }

    pub fn record(&self, event: NewEvent) -> Result<Event, AmiriError> {
        let mut store = self.lock_store()?;
        let stored = store.append(event).map_err(store_error)?;
        // Under the lock: broadcast order = seq order. No subscribers is fine.
        let _ = self.events.send(stored.clone());
        Ok(stored)
    }

    /// Record a failed authentication, rate-limited per reason so a flood of bad
    /// connections can't grow the log without bound.
    pub fn record_auth_failure(&self, reason: &str, client: Option<&str>) {
        let suppressed = {
            let Ok(mut map) = self.auth_failures.lock() else {
                return;
            };
            let now = Instant::now();
            let entry = map
                .entry(reason.to_string())
                .or_insert((now - AUTH_FAIL_WINDOW, 0));
            if now.duration_since(entry.0) < AUTH_FAIL_WINDOW {
                entry.1 += 1;
                return;
            }
            let suppressed = entry.1;
            *entry = (now, 0);
            suppressed
        };
        let payload =
            json!({ "reason": reason, "client": client, "suppressed_since_last": suppressed });
        if let Err(e) = self.record(NewEvent::new(
            kinds::IPC_AUTH_FAILED,
            actors::AMIRI,
            payload,
        )) {
            tracing::error!(error = %e, "could not record ipc.auth_failed");
        }
    }

    /// Run `f` against the store, mapping store errors to a §14 error.
    pub fn with_store<T>(
        &self,
        f: impl FnOnce(&mut Store) -> amiri_store::Result<T>,
    ) -> Result<T, AmiriError> {
        let mut store = self.lock_store()?;
        f(&mut store).map_err(store_error)
    }

    fn lock_store(&self) -> Result<std::sync::MutexGuard<'_, Store>, AmiriError> {
        self.store.lock().map_err(|_| {
            AmiriError::new(
                "Access the event log",
                "amirid store",
                "store lock poisoned by a panic",
            )
            .next_step("Restart the daemon: `amiri daemon stop`, then `amiri daemon start`.")
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// A new id for an authenticated connection (unique for this daemon run).
    pub fn new_connection_id(&self) -> u64 {
        self.next_connection.fetch_add(1, Ordering::Relaxed)
    }

    pub fn client_connected(&self) {
        self.clients.fetch_add(1, Ordering::Relaxed);
    }

    pub fn client_disconnected(&self) {
        self.clients.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn clients(&self) -> u32 {
        self.clients.load(Ordering::Relaxed)
    }
}

fn store_error(e: amiri_store::StoreError) -> AmiriError {
    AmiriError::new("Access the event log", "amirid store", e.to_string())
        .next_step("Run `amiri doctor` to check database integrity.")
}
