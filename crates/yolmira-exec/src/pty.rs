//! One shell in a ConPTY.
//! - The reader thread forwards output. It answers ConPTY's startup cursor request
//!   (DSR) itself and removes it from the output: ConPTY answers applications' own
//!   cursor queries internally, so this request is only its handshake with the
//!   terminal. Answering it here means no client ever answers twice, and nobody has to
//!   be attached for the shell to start (S1 finding 5).
//! - The watcher thread reports the exit code. ConPTY's output doesn't reach EOF when
//!   the shell exits, only when the pseudoconsole closes, so on exit the watcher closes
//!   it and waits for the reader to drain the last output before reporting the exit.

use crate::ExecError;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;
use yolmira_ipc::win::process::ProcessWatch;

const DSR: &[u8] = b"\x1b[6n";
const DSR_ANSWER: &[u8] = b"\x1b[1;1R";
const WATCH_TICK: Duration = Duration::from_millis(250);
/// After closing the pseudoconsole, how long to wait for the last output.
const DRAIN_WAIT: Duration = Duration::from_secs(2);
/// After a kill/close, how long to wait for the process to go.
const CLOSE_WAIT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub struct PtySpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// The complete environment (the allowlist); nothing is inherited.
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyEvent {
    Output(Vec<u8>),
    /// The shell exited (all its output was delivered first); `None` if the exit
    /// code couldn't be read.
    Exited(Option<u32>),
}

pub type EventSink = Arc<dyn Fn(PtyEvent) + Send + Sync>;

type Master = Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>;
type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

/// Poison-tolerant lock: a panic elsewhere must not make the terminal unusable.
fn lock<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

pub struct PtySession {
    master: Master,
    writer: Writer,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    pid: Option<u32>,
    closed: Arc<AtomicBool>,
}

impl PtySession {
    pub fn spawn(spec: &PtySpec, sink: EventSink) -> Result<Self, ExecError> {
        let spawn_err =
            |what: &str, e: &dyn std::fmt::Display| ExecError::Spawn(format!("{what}: {e}"));
        let size = PtySize {
            rows: spec.rows,
            cols: spec.cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = native_pty_system()
            .openpty(size)
            .map_err(|e| spawn_err("openpty", &e))?;
        let child = pair
            .slave
            .spawn_command(command(spec))
            .map_err(|e| spawn_err(&spec.program, &e))?;
        drop(pair.slave);
        let pid = child.process_id();
        // Open the watch at once, so the pid can't be reused under us.
        let watch = pid.and_then(|p| {
            ProcessWatch::open(p)
                .map_err(|e| tracing::warn!(pid = p, error = %e, "no process watch"))
                .ok()
        });
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| spawn_err("reader", &e))?;
        let writer: Writer = Arc::new(Mutex::new(
            pair.master
                .take_writer()
                .map_err(|e| spawn_err("writer", &e))?,
        ));
        let drained = Arc::new((Mutex::new(false), Condvar::new()));
        spawn_reader(
            reader,
            Arc::clone(&writer),
            Arc::clone(&sink),
            Arc::clone(&drained),
        );
        let session = Self {
            master: Arc::new(Mutex::new(Some(pair.master))),
            writer,
            child: Mutex::new(child),
            pid,
            closed: Arc::new(AtomicBool::new(false)),
        };
        spawn_watcher(
            watch,
            Arc::clone(&session.master),
            Arc::clone(&session.closed),
            drained,
            sink,
        );
        Ok(session)
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Blocking write to the shell's input (call from a blocking context).
    pub fn write(&self, data: &[u8]) -> std::io::Result<()> {
        let mut w = lock(&self.writer);
        w.write_all(data)?;
        w.flush()
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), ExecError> {
        let guard = lock(&self.master);
        let master = guard
            .as_ref()
            .ok_or_else(|| ExecError::Spawn("the terminal is closed".into()))?;
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| ExecError::Spawn(format!("resize: {e}")))
    }

    /// End the shell. Its children die when the pseudoconsole closes (S1).
    pub fn kill(&self) -> Result<(), ExecError> {
        {
            let mut child = lock(&self.child);
            if let Err(e) = child.kill() {
                // Already exited is fine; anything else is reported.
                if matches!(child.try_wait(), Ok(None)) {
                    return Err(ExecError::Spawn(format!("kill: {e}")));
                }
            }
        }
        self.close();
        Ok(())
    }

    /// Close the pseudoconsole (blocking: conhost may take a moment).
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        lock(&self.master).take();
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.close();
    }
}

fn command(spec: &PtySpec) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(&spec.program);
    cmd.args(&spec.args);
    cmd.cwd(&spec.cwd);
    cmd.env_clear();
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    cmd
}

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    writer: Writer,
    sink: EventSink,
    drained: Arc<(Mutex<bool>, Condvar)>,
) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 16 * 1024];
        let mut filter = DsrFilter::default();
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let (out, requests) = filter.feed(&buf[..n]);
                    for _ in 0..requests {
                        let mut w = lock(&writer);
                        if let Err(e) = w.write_all(DSR_ANSWER).and_then(|_| w.flush()) {
                            tracing::warn!(error = %e, "could not answer the terminal's cursor request");
                        }
                    }
                    if !out.is_empty() {
                        sink(PtyEvent::Output(out));
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "terminal output ended with an error");
                    break;
                }
            }
        }
        let rest = filter.finish();
        if !rest.is_empty() {
            sink(PtyEvent::Output(rest));
        }
        let (done, cv) = &*drained;
        *lock(done) = true;
        cv.notify_all();
    });
}

fn spawn_watcher(
    watch: Option<ProcessWatch>,
    master: Master,
    closed: Arc<AtomicBool>,
    drained: Arc<(Mutex<bool>, Condvar)>,
    sink: EventSink,
) {
    std::thread::spawn(move || {
        let code = wait_for_exit(watch.as_ref(), &closed);
        // Close the pseudoconsole so the reader reaches EOF, then let it deliver the rest.
        lock(&master).take();
        let (done, cv) = &*drained;
        let guard = lock(done);
        if let Ok((g, timeout)) = cv.wait_timeout_while(guard, DRAIN_WAIT, |d| !*d) {
            if timeout.timed_out() && !*g {
                tracing::warn!("terminal output did not finish draining; reporting exit anyway");
            }
        }
        sink(PtyEvent::Exited(code));
    });
}

fn wait_for_exit(watch: Option<&ProcessWatch>, closed: &AtomicBool) -> Option<u32> {
    let watch = watch?;
    loop {
        match watch.wait(WATCH_TICK) {
            Ok(Some(code)) => return Some(code),
            // Closed but still running: give the pseudoconsole close a moment to end it.
            Ok(None) if closed.load(Ordering::SeqCst) => {
                return watch.wait(CLOSE_WAIT).ok().flatten()
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(error = %e, "process watch failed");
                return None;
            }
        }
    }
}

/// Removes ConPTY's cursor request from the output stream, counting occurrences,
/// including one split across reads.
#[derive(Debug, Default)]
pub struct DsrFilter {
    carry: Vec<u8>,
}

impl DsrFilter {
    /// Output to pass on, and how many requests to answer.
    pub fn feed(&mut self, data: &[u8]) -> (Vec<u8>, usize) {
        let mut buf = std::mem::take(&mut self.carry);
        buf.extend_from_slice(data);
        let mut out = Vec::with_capacity(buf.len());
        let mut requests = 0;
        let mut i = 0;
        while i < buf.len() {
            if buf[i..].starts_with(DSR) {
                requests += 1;
                i += DSR.len();
            } else if DSR.starts_with(&buf[i..]) {
                // A possible request cut off at the end of this read: keep it.
                self.carry = buf[i..].to_vec();
                break;
            } else {
                out.push(buf[i]);
                i += 1;
            }
        }
        (out, requests)
    }

    /// Whatever was held back when the stream ends.
    pub fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.carry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_and_counts_requests() {
        let mut f = DsrFilter::default();
        assert_eq!(f.feed(b"a\x1b[6nb\x1b[6n"), (b"ab".to_vec(), 2));
    }

    #[test]
    fn handles_a_request_split_across_reads() {
        let mut f = DsrFilter::default();
        assert_eq!(f.feed(b"ab\x1b["), (b"ab".to_vec(), 0));
        assert_eq!(f.feed(b"6ncd"), (b"cd".to_vec(), 1));
    }

    #[test]
    fn a_held_back_prefix_that_isnt_a_request_is_released() {
        let mut f = DsrFilter::default();
        assert_eq!(f.feed(b"x\x1b"), (b"x".to_vec(), 0));
        assert_eq!(f.feed(b"[2J"), (b"\x1b[2J".to_vec(), 0));
        assert_eq!(f.feed(b"\x1b["), (vec![], 0));
        assert_eq!(f.finish(), b"\x1b[".to_vec());
    }
}
