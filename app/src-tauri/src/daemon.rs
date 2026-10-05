//! The UI's link to `amirid`.
//!
//! - One **monitor** connection follows the event stream. When it breaks, the daemon
//!   is gone: the UI shows the banner and every session as UNKNOWN until it reconnects.
//! - One **control** connection carries requests (create, input, resize, kill). A call
//!   that fails mid-way (timeout, I/O, a reply out of step) leaves the connection in an
//!   unknown state, so it is dropped and the supervisor reconnects both links.
//! - One **feed** connection per attached session carries its output, read by its own
//!   task, as in `amiri shell attach`. Each attachment has a generation chosen by the
//!   frontend; every event carries it, so output from a replaced feed is ignored.
//!
//! The UI starts the daemon once at launch (`ensure_daemon`, like the CLI) and on the
//! banner's Start button. It never stops it: closing or killing the UI leaves the
//! daemon and its shells running.

use crate::errors::{client_error, not_connected};
use amiri_core::AmiriError;
use amiri_ipc::protocol::{
    methods, notifications, EventsSubscribeParams, HelloResult, SessionExited, SessionOutput,
};
use amiri_ipc::{ensure_daemon, Client, ClientError, Instance, LaunchOptions};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

const START_TIMEOUT: Duration = Duration::from_secs(10);
const RECONNECT_EVERY: Duration = Duration::from_secs(1);
const CLIENT_MONITOR: &str = "amiri-ui-monitor";
const CLIENT_CONTROL: &str = "amiri-ui";
const CLIENT_FEED: &str = "amiri-ui-attach";

/// Events the frontend listens to.
pub mod ui_events {
    pub const DAEMON_STATE: &str = "daemon-state";
    pub const DAEMON_EVENT: &str = "daemon-event";
    pub const SESSION_OUTPUT: &str = "session-output";
    pub const SESSION_EXITED: &str = "session-exited";
    pub const SESSION_FEED_CLOSED: &str = "session-feed-closed";
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DaemonState {
    pub connected: bool,
    pub pid: Option<u32>,
    pub version: Option<String>,
    pub instance: Option<String>,
    /// Why the UI isn't connected (none while connected).
    pub error: Option<AmiriError>,
}

/// A feed event, tagged with the attachment generation it belongs to.
#[derive(Debug, Clone, Serialize)]
struct Tagged<T> {
    #[serde(flatten)]
    inner: T,
    generation: u32,
}

#[derive(Debug, Clone, Serialize)]
struct FeedClosed {
    id: String,
    why: String,
}

type Control = Arc<tokio::sync::Mutex<Client>>;

/// The attachment a session currently should have. `task` is `None` while it connects.
struct Feed {
    generation: u32,
    task: Option<JoinHandle<()>>,
}

pub struct Daemon {
    instance: Result<Instance, AmiriError>,
    /// Swapped without waiting, so a hung call never delays a disconnect.
    control: Mutex<Option<Control>>,
    feeds: Mutex<HashMap<String, Feed>>,
    state: Mutex<DaemonState>,
    /// A control call broke the connection: the supervisor reconnects.
    broken: Notify,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic elsewhere must not take the UI's state down with it.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// After these, the connection's request/response pairing can't be trusted.
fn breaks_connection(e: &ClientError) -> bool {
    match e {
        ClientError::Timeout
        | ClientError::Io(_)
        | ClientError::Protocol(_)
        | ClientError::Closed => true,
        ClientError::Rpc(_) | ClientError::NotRunning | ClientError::Impostor(_) => false,
    }
}

impl Daemon {
    pub fn new(instance: Result<Instance, AmiriError>) -> Arc<Self> {
        let state = DaemonState {
            instance: instance.as_ref().ok().and_then(|i| i.name.clone()),
            error: instance.as_ref().err().cloned(),
            ..DaemonState::default()
        };
        Arc::new(Self {
            instance,
            control: Mutex::new(None),
            feeds: Mutex::new(HashMap::new()),
            state: Mutex::new(state),
            broken: Notify::new(),
        })
    }

    pub fn state(&self) -> DaemonState {
        lock(&self.state).clone()
    }

    fn instance(&self) -> Result<&Instance, AmiriError> {
        self.instance.as_ref().map_err(Clone::clone)
    }

    /// Emit only on change, so a daemon that stays down doesn't flood the UI.
    fn set_state(&self, app: &AppHandle, next: DaemonState) {
        let changed = {
            let mut s = lock(&self.state);
            let changed = *s != next;
            *s = next.clone();
            changed
        };
        if changed {
            emit(app, ui_events::DAEMON_STATE, next);
        }
    }

    fn down(&self, error: Option<AmiriError>) -> DaemonState {
        DaemonState {
            instance: self.instance().ok().and_then(|i| i.name.clone()),
            error,
            ..DaemonState::default()
        }
    }

    /// `ensure_daemon`: connect to a running daemon or start one detached.
    pub async fn start(&self) -> Result<HelloResult, AmiriError> {
        let instance = self.instance()?.clone();
        let daemon_exe = std::env::current_exe()
            .map(|p| p.with_file_name("amirid.exe"))
            .map_err(|e| {
                AmiriError::new("Find amirid.exe", "ui", e.to_string())
                    .next_step("Reinstall AMIRI.")
            })?;
        let launched = ensure_daemon(LaunchOptions {
            instance,
            daemon_exe,
            allow_elevated: false,
            timeout: START_TIMEOUT,
            client_name: CLIENT_CONTROL.into(),
        })
        .await?;
        Ok(launched.hello().clone())
    }

    /// Open the monitor (subscribed to new events) and the control connection.
    async fn connect(&self, instance: &Instance) -> Result<(Client, HelloResult), ClientError> {
        let mut monitor = Client::connect(instance, CLIENT_MONITOR).await?;
        let _: Value = monitor
            .call(
                methods::EVENTS_SUBSCRIBE,
                json!(EventsSubscribeParams { after: None }),
            )
            .await?;
        let control = Client::connect(instance, CLIENT_CONTROL).await?;
        let hello = control.hello.clone();
        *lock(&self.control) = Some(Arc::new(tokio::sync::Mutex::new(control)));
        Ok((monitor, hello))
    }

    /// Drop the control link and stop every feed. Never waits on an in-flight call.
    fn on_disconnect(&self) {
        *lock(&self.control) = None;
        for (_, feed) in lock(&self.feeds).drain() {
            if let Some(task) = feed.task {
                task.abort();
            }
        }
    }

    /// One request on the control connection.
    pub async fn call(&self, what: &str, method: &str, params: Value) -> Result<Value, AmiriError> {
        let control = lock(&self.control)
            .clone()
            .ok_or_else(|| not_connected(what))?;
        let result = control.lock().await.call_value(method, params).await;
        result.map_err(|e| {
            if breaks_connection(&e) {
                // Only if no reconnect replaced it meanwhile.
                let mut slot = lock(&self.control);
                if slot.as_ref().is_some_and(|c| Arc::ptr_eq(c, &control)) {
                    *slot = None;
                    self.broken.notify_one();
                }
            }
            client_error(what, e)
        })
    }

    /// Attach on a new feed connection; output goes to the frontend as events tagged
    /// with `generation`. A newer attach or a detach meanwhile wins.
    pub async fn attach(
        &self,
        app: &AppHandle,
        id: String,
        generation: u32,
    ) -> Result<Value, AmiriError> {
        const WHAT: &str = "Attach to the shell";
        let instance = self.instance()?;
        if !self.state().connected {
            return Err(not_connected(WHAT));
        }
        if let Some(old) = lock(&self.feeds).insert(
            id.clone(),
            Feed {
                generation,
                task: None,
            },
        ) {
            if let Some(task) = old.task {
                task.abort();
            }
        }
        let mut feed = Client::connect(instance, CLIENT_FEED)
            .await
            .map_err(|e| client_error(WHAT, e))?;
        let result = feed
            .call_value(methods::SESSION_ATTACH, json!({ "id": id }))
            .await
            .map_err(|e| client_error(WHAT, e))?;
        let mut feeds = lock(&self.feeds);
        match feeds.get_mut(&id) {
            Some(f) if f.generation == generation => {
                let task =
                    tauri::async_runtime::spawn(read_feed(app.clone(), id, generation, feed));
                f.task = Some(task);
                Ok(result)
            }
            // Detached, replaced or disconnected while connecting: dropping `feed` detaches.
            _ => Err(AmiriError::new(WHAT, "ui", "the attach was cancelled")
                .next_step("Attach again if you still want this shell.")),
        }
    }

    /// Dropping the feed connection detaches; the shell keeps running.
    pub fn detach(&self, id: &str) {
        if let Some(Feed {
            task: Some(task), ..
        }) = lock(&self.feeds).remove(id)
        {
            task.abort();
        }
    }
}

/// Forward one session's output until it exits or the connection ends.
async fn read_feed(app: AppHandle, id: String, generation: u32, mut feed: Client) {
    let tag = |inner| Tagged { inner, generation };
    let why = loop {
        match feed.next_notification().await {
            Ok(n) if n.method == notifications::SESSION_OUTPUT => {
                match serde_json::from_value::<SessionOutput>(n.params) {
                    Ok(out) => emit(
                        &app,
                        ui_events::SESSION_OUTPUT,
                        Tagged {
                            inner: out,
                            generation,
                        },
                    ),
                    // The error text could quote terminal bytes: log only its category.
                    Err(e) => break format!("malformed output notification ({:?})", e.classify()),
                }
            }
            Ok(n) if n.method == notifications::SESSION_EXITED => {
                match serde_json::from_value::<SessionExited>(n.params) {
                    Ok(exited) => emit(
                        &app,
                        ui_events::SESSION_EXITED,
                        Tagged {
                            inner: exited,
                            generation,
                        },
                    ),
                    Err(e) => tracing::warn!("malformed exit notification ({:?})", e.classify()),
                }
                break "the shell exited".into();
            }
            Ok(_) => {}
            Err(e) => break e.to_string(),
        }
    };
    emit(
        &app,
        ui_events::SESSION_FEED_CLOSED,
        tag(FeedClosed { id, why }),
    );
}

fn emit<T: Serialize + Clone>(app: &AppHandle, event: &str, payload: T) {
    if let Err(e) = app.emit(event, payload) {
        tracing::warn!("emit {event}: {e}");
    }
}

/// Start the daemon once, then stay connected: follow the event stream, and after a
/// break try to reconnect every second (connect only; starting is the user's call).
pub fn supervise(app: AppHandle, daemon: Arc<Daemon>) {
    tauri::async_runtime::spawn(async move {
        let instance = match daemon.instance() {
            Ok(i) => i.clone(),
            Err(e) => return daemon.set_state(&app, daemon.down(Some(e))),
        };
        if let Err(e) = daemon.start().await {
            tracing::warn!("start daemon: {e}");
            daemon.set_state(&app, daemon.down(Some(e)));
        }
        loop {
            match daemon.connect(&instance).await {
                Ok((monitor, hello)) => {
                    stay_connected(&app, &daemon, &instance, monitor, hello).await
                }
                Err(e) => connect_failed(&app, &daemon, e),
            }
            tokio::time::sleep(RECONNECT_EVERY).await;
        }
    });
}

async fn stay_connected(
    app: &AppHandle,
    daemon: &Daemon,
    instance: &Instance,
    monitor: Client,
    hello: HelloResult,
) {
    let up = DaemonState {
        connected: true,
        pid: Some(hello.pid),
        version: Some(hello.daemon_version),
        instance: instance.name.clone(),
        error: None,
    };
    daemon.set_state(app, up);
    let why = tokio::select! {
        why = follow(app, monitor) => why,
        () = daemon.broken.notified() => AmiriError::new(
            "Stay connected to the AMIRI daemon", "ui", "a request failed mid-way; reconnecting",
        ).next_step("Retry the last action once the UI has reconnected."),
    };
    tracing::info!("daemon connection lost: {}", why.why);
    daemon.on_disconnect();
    daemon.set_state(app, daemon.down(Some(why)));
}

fn connect_failed(app: &AppHandle, daemon: &Daemon, e: ClientError) {
    // While the daemon simply isn't there, keep the earlier, more specific error
    // (e.g. why a start failed).
    let keep = matches!(e, ClientError::NotRunning) && daemon.state().error.is_some();
    if !keep {
        daemon.set_state(
            app,
            daemon.down(Some(client_error("Connect to the AMIRI daemon", e))),
        );
    }
}

/// Forward daemon events until the monitor connection breaks; returns why.
async fn follow(app: &AppHandle, mut monitor: Client) -> AmiriError {
    loop {
        match monitor.next_notification().await {
            Ok(n) if n.method == notifications::EVENT => {
                emit(app, ui_events::DAEMON_EVENT, n.params)
            }
            Ok(_) => {}
            Err(e) => return client_error("Stay connected to the AMIRI daemon", e),
        }
    }
}
