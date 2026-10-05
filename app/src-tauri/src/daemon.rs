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
//! - **Flow control:** a feed stops reading while more than [`FLOW_WINDOW`] bytes of
//!   its output wait to be written by the terminal; the frontend acknowledges them
//!   (`session_ack`). The daemon then sees a slow attachment, and when it falls behind
//!   it resends the scrollback (reset + snapshot, at most 2 MiB): memory stays bounded.
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
use tokio::sync::{Notify, Semaphore};

const START_TIMEOUT: Duration = Duration::from_secs(10);
/// Opening a feed: connect plus the attach call (whose replay is streamed afterwards).
const ATTACH_TIMEOUT: Duration = Duration::from_secs(15);
/// Unacknowledged output per feed, in bytes of the events' base64 data.
pub const FLOW_WINDOW: usize = 1024 * 1024;
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

/// Output, with the credit the frontend returns once the terminal has written it.
#[derive(Debug, Clone, Serialize)]
struct Credited {
    #[serde(flatten)]
    out: SessionOutput,
    credit: u32,
}

#[derive(Debug, Clone, Serialize)]
struct FeedClosed {
    id: String,
    why: String,
}

/// One control connection. `broken` belongs to it, so a signal left over from an old
/// connection can never end a newer one.
struct Link {
    client: tokio::sync::Mutex<Client>,
    broken: Notify,
}

/// The attachment a session currently should have. `task` is `None` while it connects.
struct Feed {
    generation: u32,
    task: Option<JoinHandle<()>>,
    /// Bytes the feed may still emit before the frontend acknowledges some.
    flow: Arc<Semaphore>,
}

impl Feed {
    fn stop(self) {
        if let Some(task) = self.task {
            task.abort();
        }
    }
}

pub struct Daemon {
    instance: Result<Instance, AmiriError>,
    /// Swapped without waiting, so a hung call never delays a disconnect. Lock order
    /// where both are held: `feeds`, then `control`.
    control: Mutex<Option<Arc<Link>>>,
    feeds: Mutex<HashMap<String, Feed>>,
    state: Mutex<DaemonState>,
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

    fn link(&self) -> Option<Arc<Link>> {
        lock(&self.control).clone()
    }

    fn is_current(&self, link: &Arc<Link>) -> bool {
        lock(&self.control)
            .as_ref()
            .is_some_and(|c| Arc::ptr_eq(c, link))
    }

    /// Open the monitor (subscribed to new events) and the control connection.
    async fn connect(
        &self,
        instance: &Instance,
    ) -> Result<(Client, HelloResult, Arc<Link>), ClientError> {
        let mut monitor = Client::connect(instance, CLIENT_MONITOR).await?;
        let _: Value = monitor
            .call(
                methods::EVENTS_SUBSCRIBE,
                json!(EventsSubscribeParams { after: None }),
            )
            .await?;
        let control = Client::connect(instance, CLIENT_CONTROL).await?;
        let hello = control.hello.clone();
        let link = Arc::new(Link {
            client: tokio::sync::Mutex::new(control),
            broken: Notify::new(),
        });
        *lock(&self.control) = Some(Arc::clone(&link));
        Ok((monitor, hello, link))
    }

    /// Drop the control link and stop every feed. Never waits on an in-flight call.
    fn on_disconnect(&self) {
        *lock(&self.control) = None;
        for (_, feed) in lock(&self.feeds).drain() {
            feed.stop();
        }
    }

    /// One request on the control connection.
    pub async fn call(&self, what: &str, method: &str, params: Value) -> Result<Value, AmiriError> {
        let link = self.link().ok_or_else(|| not_connected(what))?;
        let mut client = link.client.lock().await;
        // Queued behind a call that broke this connection: fail now rather than wait
        // out another timeout on it.
        if !self.is_current(&link) {
            return Err(not_connected(what));
        }
        let result = client.call_value(method, params).await;
        drop(client);
        result.map_err(|e| {
            if breaks_connection(&e) {
                // Only if no reconnect replaced it meanwhile.
                let mut slot = lock(&self.control);
                if slot.as_ref().is_some_and(|c| Arc::ptr_eq(c, &link)) {
                    *slot = None;
                }
                link.broken.notify_one();
            }
            client_error(what, e)
        })
    }

    /// Attach on a new feed connection; output goes to the frontend as events tagged
    /// with `generation`. A newer attach, a detach or a disconnect meanwhile wins.
    pub async fn attach(
        self: &Arc<Self>,
        app: &AppHandle,
        id: String,
        generation: u32,
    ) -> Result<Value, AmiriError> {
        const WHAT: &str = "Attach to the shell";
        let instance = self.instance()?;
        let flow = Arc::new(Semaphore::new(FLOW_WINDOW));
        let link = {
            // The link is read under the feeds lock: a disconnect clears the link before
            // it drains the feeds, so it either removes this entry or this sees no link.
            let mut feeds = lock(&self.feeds);
            let link = self.link().ok_or_else(|| not_connected(WHAT))?;
            let placeholder = Feed {
                generation,
                task: None,
                flow: Arc::clone(&flow),
            };
            if let Some(old) = feeds.insert(id.clone(), placeholder) {
                old.stop();
            }
            link
        };
        let opened = tokio::time::timeout(ATTACH_TIMEOUT, open_feed(instance, &id))
            .await
            .unwrap_or(Err(ClientError::Timeout));
        let (feed, result) = match opened {
            Ok(x) => x,
            Err(e) => {
                self.forget(&id, generation);
                return Err(client_error(WHAT, e));
            }
        };
        let mut feeds = lock(&self.feeds);
        match feeds.get_mut(&id) {
            Some(f) if f.generation == generation && self.is_current(&link) => {
                let reader = read_feed(app.clone(), Arc::clone(self), id, generation, feed, flow);
                f.task = Some(tauri::async_runtime::spawn(reader));
                Ok(result)
            }
            // Detached, replaced or disconnected while connecting: dropping `feed` detaches.
            _ => Err(AmiriError::new(WHAT, "ui", "the attach was cancelled")
                .next_step("Attach again if you still want this shell.")),
        }
    }

    /// Dropping the feed connection detaches; the shell keeps running.
    pub fn detach(&self, id: &str) {
        if let Some(feed) = lock(&self.feeds).remove(id) {
            feed.stop();
        }
    }

    /// The terminal has written `credit` bytes of this attachment's output.
    pub fn ack(&self, id: &str, generation: u32, credit: usize) {
        if let Some(f) = lock(&self.feeds).get(id) {
            if f.generation == generation {
                f.flow.add_permits(credit.min(FLOW_WINDOW));
            }
        }
    }

    /// Remove the entry if it still belongs to this attachment, without aborting it:
    /// the caller is that attachment ending, or one that never started.
    fn forget(&self, id: &str, generation: u32) {
        let mut feeds = lock(&self.feeds);
        if feeds.get(id).is_some_and(|f| f.generation == generation) {
            feeds.remove(id);
        }
    }
}

async fn open_feed(instance: &Instance, id: &str) -> Result<(Client, Value), ClientError> {
    let mut feed = Client::connect(instance, CLIENT_FEED).await?;
    let result = feed
        .call_value(methods::SESSION_ATTACH, json!({ "id": id }))
        .await?;
    Ok((feed, result))
}

/// Forward one session's output until it exits or the connection ends.
async fn read_feed(
    app: AppHandle,
    daemon: Arc<Daemon>,
    id: String,
    generation: u32,
    mut feed: Client,
    flow: Arc<Semaphore>,
) {
    let why = loop {
        match feed.next_notification().await {
            Ok(n) if n.method == notifications::SESSION_OUTPUT => {
                match serde_json::from_value::<SessionOutput>(n.params) {
                    Ok(out) => {
                        let credit = out.data.len().clamp(1, FLOW_WINDOW) as u32;
                        // Wait for the terminal to catch up; meanwhile this connection
                        // isn't read, and the daemon treats the attachment as slow.
                        match flow.acquire_many(credit).await {
                            Ok(permit) => permit.forget(),
                            Err(_) => break "flow control closed".into(),
                        }
                        emit(
                            &app,
                            ui_events::SESSION_OUTPUT,
                            Tagged {
                                inner: Credited { out, credit },
                                generation,
                            },
                        );
                    }
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
    daemon.forget(&id, generation);
    emit(
        &app,
        ui_events::SESSION_FEED_CLOSED,
        Tagged {
            inner: FeedClosed { id, why },
            generation,
        },
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
                Ok((monitor, hello, link)) => {
                    stay_connected(&app, &daemon, &instance, monitor, hello, &link).await
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
    link: &Link,
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
        () = link.broken.notified() => AmiriError::new(
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
