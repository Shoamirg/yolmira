//! A connection's outgoing lines, bounded in bytes (Phase 1 exit checks: a client that
//! read slower than a shell wrote once held ~190 MB of queued scrollback copies).
//!
//! Two kinds of line share one ordered queue:
//! - **control** (responses, events, exit notices): never dropped. A sender waits while
//!   the connection's queued control bytes are over its budget (backpressure). Only that
//!   connection's own tasks ever wait: shared delivery (the event broadcast) never does.
//! - **shell output**: never waits. It is refused when the connection's queued output
//!   would pass its budget, or all connections' queued output would pass the daemon-wide
//!   budget. The forwarder then drops that shell's queued output and, once a whole
//!   snapshot fits, sends one fresh scrollback snapshot (`session_handlers::forward`).
//!
//! A client stuck for `STALL_LIMIT` (a control sender waiting for room that long, or one
//! line not written that long) is **stalled**: the outbox closes and the connection ends.
//! The line being written still counts against the budgets until its write completes.
//! The daemon-wide budget counts each connection's queued output beyond `SHARED_FLOOR`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

/// Queued shell output one connection may hold.
pub const OUTPUT_BUDGET: usize = 8 * 1024 * 1024;
/// Queued shell output all connections together may hold.
pub const DAEMON_OUTPUT_BUDGET: usize = 64 * 1024 * 1024;
/// Queued responses and events one connection may hold before senders wait. These are
/// small JSON lines; one larger line still goes out alone.
pub const CONTROL_BUDGET: usize = 256 * 1024;
/// Each connection's first bytes of queued output don't count against the daemon-wide
/// budget: a client that keeps up stays below this, so slow clients filling the shared
/// budget can never get a fast one refused (and resynced).
pub const SHARED_FLOOR: usize = 512 * 1024;
/// A client that makes no room for this long is disconnected.
pub const STALL_LIMIT: Duration = Duration::from_secs(30);
/// Scrollback snapshots being taken and encoded at once, daemon-wide: each is a copy of
/// the scrollback (2 MiB) plus its encoding (~2.8 MB) until it is queued.
const SNAPSHOTS_AT_ONCE: usize = 4;

/// What happened to offered output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    Queued,
    /// Over the connection's or the daemon's budget; nothing was queued.
    Full,
    /// The connection is gone.
    Closed,
}

/// The daemon-wide output budget, shared by every connection's outbox.
pub struct OutputBudget {
    limit: usize,
    used: AtomicUsize,
    /// Highest daemon-wide and per-connection queued output seen (`debug.output_stats`).
    high: AtomicUsize,
    conn_high: AtomicUsize,
    resyncs: AtomicU64,
    freed: Notify,
    snapshots: Arc<Semaphore>,
}

impl OutputBudget {
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicUsize::new(0),
            high: AtomicUsize::new(0),
            conn_high: AtomicUsize::new(0),
            resyncs: AtomicU64::new(0),
            freed: Notify::new(),
            snapshots: Arc::new(Semaphore::new(SNAPSHOTS_AT_ONCE)),
        })
    }

    fn try_take(&self, n: usize) -> bool {
        let mut used = self.used.load(Ordering::Acquire);
        loop {
            if used + n > self.limit {
                return false;
            }
            match self.used.compare_exchange_weak(
                used,
                used + n,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.high.fetch_max(used + n, Ordering::AcqRel);
                    return true;
                }
                Err(now) => used = now,
            }
        }
    }

    fn give(&self, n: usize) {
        if n > 0 {
            self.used.fetch_sub(n, Ordering::AcqRel);
            self.freed.notify_waiters();
        }
    }

    fn fits(&self, n: usize) -> bool {
        self.used.load(Ordering::Acquire) + n <= self.limit
    }

    /// Permission to take and encode one scrollback snapshot.
    pub async fn snapshot_permit(&self) -> OwnedSemaphorePermit {
        Arc::clone(&self.snapshots)
            .acquire_owned()
            .await
            .expect("the snapshot semaphore is never closed")
    }

    pub fn count_resync(&self) {
        self.resyncs.fetch_add(1, Ordering::Relaxed);
    }

    /// (limit, used, high water, per-connection high water, resyncs).
    pub fn stats(&self) -> (usize, usize, usize, usize, u64) {
        (
            self.limit,
            self.used.load(Ordering::Acquire),
            self.high.load(Ordering::Acquire),
            self.conn_high.load(Ordering::Acquire),
            self.resyncs.load(Ordering::Relaxed),
        )
    }
}

struct Item {
    /// Ends with `\n`.
    line: String,
    /// The session whose output this is; `None` for control lines.
    output_of: Option<Arc<str>>,
}

#[derive(Default)]
struct Queue {
    items: VecDeque<Item>,
    output: usize,
    control: usize,
    /// The line the writer is writing: (bytes, is output). Counted until written.
    in_flight: Option<(usize, bool)>,
    /// No more lines will be sent; the writer drains and ends.
    finished: bool,
    /// Nothing more is written (the writer failed, or the client stalled).
    closed: bool,
    stalled: Option<&'static str>,
}

pub struct Outbox {
    queue: Mutex<Queue>,
    /// A line was queued (one waiter: the writer).
    item: Notify,
    /// Room was made in this connection's queue.
    room: Notify,
    /// The outbox closed.
    gone: Notify,
    global: Arc<OutputBudget>,
    output_budget: usize,
    control_budget: usize,
    stall_limit: Duration,
    floor: usize,
}

/// One frame: the line plus its newline, with no spare capacity (what is counted is
/// what is held).
fn frame(mut line: String) -> String {
    line.push('\n');
    line.shrink_to_fit();
    line
}

impl Outbox {
    pub fn new(global: Arc<OutputBudget>) -> Arc<Self> {
        Self::with_limits(
            global,
            OUTPUT_BUDGET,
            CONTROL_BUDGET,
            STALL_LIMIT,
            SHARED_FLOOR,
        )
    }

    pub fn with_limits(
        global: Arc<OutputBudget>,
        output_budget: usize,
        control_budget: usize,
        stall_limit: Duration,
        floor: usize,
    ) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(Queue::default()),
            item: Notify::new(),
            room: Notify::new(),
            gone: Notify::new(),
            global,
            output_budget,
            control_budget,
            stall_limit,
            floor,
        })
    }

    /// The part of `output` queued bytes that counts against the daemon-wide budget.
    fn shared(&self, output: usize) -> usize {
        output.saturating_sub(self.floor)
    }

    /// Lower the connection's queued output by `m`; returns the shared bytes freed.
    fn shrink(&self, q: &mut Queue, m: usize) -> usize {
        let before = self.shared(q.output);
        q.output -= m;
        before - self.shared(q.output)
    }

    pub fn global(&self) -> &OutputBudget {
        &self.global
    }

    pub fn stall_limit(&self) -> Duration {
        self.stall_limit
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queue a control line, waiting while this connection's control backlog is over
    /// budget. `false` when the connection is gone, or the client stalled (no room for
    /// `stall_limit`), which closes the outbox.
    pub async fn send(&self, line: String) -> bool {
        let line = frame(line);
        let deadline = tokio::time::Instant::now() + self.stall_limit;
        loop {
            let room = self.room.notified();
            tokio::pin!(room);
            room.as_mut().enable();
            {
                let mut q = self.lock();
                if q.closed || q.finished {
                    return false;
                }
                // A single line larger than the budget still goes out on its own.
                if q.control == 0 || q.control + line.len() <= self.control_budget {
                    q.control += line.len();
                    q.items.push_back(Item {
                        line,
                        output_of: None,
                    });
                    drop(q);
                    self.item.notify_one();
                    return true;
                }
            }
            if tokio::time::timeout_at(deadline, room).await.is_err() {
                self.stall("responses and events over budget");
                return false;
            }
        }
    }

    /// Queue `lines` of `session`'s output, all or none, without waiting.
    pub fn offer_output(&self, session: &Arc<str>, lines: Vec<String>) -> Offer {
        let lines: Vec<String> = lines.into_iter().map(frame).collect();
        let n: usize = lines.iter().map(String::len).sum();
        let mut q = self.lock();
        if q.closed || q.finished {
            return Offer::Closed;
        }
        let shared = self.shared(q.output + n) - self.shared(q.output);
        if q.output + n > self.output_budget || !self.global.try_take(shared) {
            return Offer::Full;
        }
        q.output += n;
        self.global.conn_high.fetch_max(q.output, Ordering::AcqRel);
        q.items.extend(lines.into_iter().map(|line| Item {
            line,
            output_of: Some(Arc::clone(session)),
        }));
        drop(q);
        self.item.notify_one();
        Offer::Queued
    }

    /// Remove `session`'s queued output (a slow client gets a fresh snapshot instead).
    /// A line already being written stays.
    pub fn drop_output(&self, session: &Arc<str>) {
        let mut q = self.lock();
        let mut freed = 0;
        q.items.retain(|it| match &it.output_of {
            Some(s) if s == session => {
                freed += it.line.len();
                false
            }
            _ => true,
        });
        let shared = self.shrink(&mut q, freed);
        drop(q);
        self.global.give(shared);
        self.room.notify_waiters();
    }

    /// Wait until `n` more bytes of output would fit this connection's and the daemon's
    /// budgets. `false` when the connection is gone.
    pub async fn wait_output_room(&self, n: usize) -> bool {
        loop {
            let room = self.room.notified();
            let freed = self.global.freed.notified();
            tokio::pin!(room, freed);
            room.as_mut().enable();
            freed.as_mut().enable();
            {
                let q = self.lock();
                if q.closed || q.finished {
                    return false;
                }
                let shared = self.shared(q.output + n) - self.shared(q.output);
                if q.output + n <= self.output_budget && self.global.fits(shared) {
                    return true;
                }
            }
            tokio::select! {
                _ = &mut room => {}
                _ = &mut freed => {}
            }
        }
    }

    /// Release the line the writer finished (it counted until now).
    fn release_in_flight(&self, q: &mut Queue) -> usize {
        match q.in_flight.take() {
            Some((n, true)) => self.shrink(q, n),
            Some((n, false)) => {
                q.control -= n;
                0
            }
            None => 0,
        }
    }

    /// The next line to write; the previous one counts as written. `None` once finished
    /// and drained, or closed.
    pub async fn next(&self) -> Option<String> {
        {
            let mut q = self.lock();
            let freed = self.release_in_flight(&mut q);
            drop(q);
            self.global.give(freed);
            self.room.notify_waiters();
        }
        loop {
            let item = self.item.notified();
            tokio::pin!(item);
            item.as_mut().enable();
            {
                let mut q = self.lock();
                if q.closed {
                    return None;
                }
                if let Some(it) = q.items.pop_front() {
                    q.in_flight = Some((it.line.len(), it.output_of.is_some()));
                    return Some(it.line);
                }
                if q.finished {
                    return None;
                }
            }
            item.await;
        }
    }

    /// No more lines will be queued; the writer drains what's there and ends.
    pub fn finish(&self) {
        self.lock().finished = true;
        self.item.notify_one();
        self.room.notify_waiters();
    }

    /// The writer failed: drop everything queued and refuse more.
    pub fn close(&self) {
        let mut q = self.lock();
        q.closed = true;
        let freed = self.shared(q.output);
        q.output = 0;
        q.control = 0;
        q.in_flight = None;
        q.items.clear();
        drop(q);
        self.global.give(freed);
        self.item.notify_one();
        self.room.notify_waiters();
        self.gone.notify_waiters();
    }

    /// The client made no room for `stall_limit`: close, remembering why.
    pub fn stall(&self, why: &'static str) {
        self.lock().stalled.get_or_insert(why);
        self.close();
    }

    /// Why the client was disconnected as stalled, if it was.
    pub fn stalled(&self) -> Option<&'static str> {
        self.lock().stalled
    }

    /// Resolves once the outbox is closed.
    pub async fn closed(&self) {
        loop {
            let gone = self.gone.notified();
            tokio::pin!(gone);
            gone.as_mut().enable();
            if self.lock().closed {
                return;
            }
            gone.await;
        }
    }
}

impl Drop for Outbox {
    fn drop(&mut self) {
        let output = std::mem::take(&mut self.lock().output);
        self.global.give(self.shared(output));
    }
}

#[cfg(test)]
mod tests;
