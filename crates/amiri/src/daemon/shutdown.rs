//! Shutdown requests from any thread (console handler, session-end window, IPC),
//! and a completion signal the requester can wait on before Windows ends the process.

use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

static REQUESTS: OnceLock<UnboundedSender<String>> = OnceLock::new();
static DONE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

/// Called once by the server before anything can request a shutdown.
pub fn install(tx: UnboundedSender<String>) {
    let _ = REQUESTS.set(tx);
}

/// Ask the server to stop, recording `reason`. Safe from any thread.
pub fn request(reason: &str) {
    let delivered = REQUESTS
        .get()
        .is_some_and(|tx| tx.send(reason.to_string()).is_ok());
    if !delivered {
        // Still starting (store open, migration) or already stopped: nothing to stop cleanly.
        tracing::warn!(reason, "shutdown requested before the server was running");
    }
}

/// Block until the server has recorded `daemon.stopped`, or `timeout` passes.
pub fn wait_done(timeout: Duration) -> bool {
    let Ok(guard) = DONE.0.lock() else {
        return false;
    };
    DONE.1
        .wait_timeout_while(guard, timeout, |done| !*done)
        .map(|(g, _)| *g)
        .unwrap_or(false)
}

pub fn mark_done() {
    if let Ok(mut done) = DONE.0.lock() {
        *done = true;
        DONE.1.notify_all();
    }
}
