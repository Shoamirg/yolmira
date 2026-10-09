//! Why each UI process ended, in `ui.log`, so an exit never has to be guessed at.
//!
//! - The log is written without buffering (`init_logging`): Tauri ends the process
//!   from its event loop, so nothing that waits for a flush at the end would run.
//! - Every exit the UI sees records its reason ("UI exiting: …"): the window closed,
//!   an exit request, a startup failure. Panics are logged where they happen.
//! - An exit the UI can't see (killed, crashed, power lost) leaves its run marker
//!   behind; the next UI start reports it as "ended without recording why".

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// What the UI knows about why it is about to exit; set before the exit happens.
static EXIT_REASON: Mutex<Option<String>> = Mutex::new(None);

pub fn set_exit_reason(reason: impl Into<String>) {
    let mut r = EXIT_REASON.lock().unwrap_or_else(|e| e.into_inner());
    // The first cause wins: a window close is followed by Tauri's exit request.
    if r.is_none() {
        *r = Some(reason.into());
    }
}

fn take_exit_reason() -> Option<String> {
    EXIT_REASON.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// One running UI: `<home>\run\ui\<pid>.run` holding the process start time.
pub struct RunMarker {
    path: Option<PathBuf>,
}

/// A process identity that survives pid reuse: pid plus its creation time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessId {
    pub pid: u32,
    pub created: u64,
}

impl RunMarker {
    /// Report earlier UIs that ended without an exit record, then mark this one.
    /// `dir` is `None` when the instance couldn't be resolved: no marker then.
    pub fn start(dir: Option<&Path>, me: ProcessId, alive: impl Fn(ProcessId) -> bool) -> Self {
        let Some(dir) = dir else {
            return Self { path: None };
        };
        for stale in stale_markers(dir, &alive) {
            tracing::warn!(
                "an earlier UI (pid {}) ended without recording why: killed (Task Manager, taskkill /F), crashed, or the machine stopped",
                stale.pid
            );
            let _ = std::fs::remove_file(marker_path(dir, stale.pid));
        }
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!("run marker folder {}: {e}", dir.display());
            return Self { path: None };
        }
        let path = marker_path(dir, me.pid);
        match std::fs::write(&path, me.created.to_string()) {
            Ok(()) => Self { path: Some(path) },
            Err(e) => {
                tracing::warn!("run marker {}: {e}", path.display());
                Self { path: None }
            }
        }
    }

    /// The UI is exiting in a way it can see: log why and drop the marker.
    pub fn exiting(&self, fallback: &str) {
        let reason = take_exit_reason().unwrap_or_else(|| fallback.to_string());
        tracing::info!("UI exiting: {reason}");
        if let Some(p) = &self.path {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn marker_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("{pid}.run"))
}

/// Markers whose process is gone (or whose pid now belongs to another process).
fn stale_markers(dir: &Path, alive: &impl Fn(ProcessId) -> bool) -> Vec<ProcessId> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let pid = name.strip_suffix(".run")?.parse().ok()?;
            let created = std::fs::read_to_string(e.path())
                .ok()?
                .trim()
                .parse()
                .ok()?;
            Some(ProcessId { pid, created })
        })
        .filter(|p| !alive(*p))
        .collect()
}

/// This process's identity.
pub fn current_process() -> ProcessId {
    let pid = std::process::id();
    ProcessId {
        pid,
        created: win::creation_time(pid).unwrap_or(0),
    }
}

/// True if `p` still runs: same pid and the same creation time.
pub fn is_alive(p: ProcessId) -> bool {
    win::creation_time(p.pid).is_some_and(|c| c == p.created)
}

mod win {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// Creation time of a running process (100 ns units), or `None` if it isn't running.
    pub fn creation_time(pid: u32) -> Option<u64> {
        // SAFETY: plain Win32 calls on a handle owned and closed here; the out
        // parameters are valid local variables.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            let mut code = 0u32;
            let running = GetExitCodeProcess(h, &mut code) != 0 && code == STILL_ACTIVE as u32;
            let zero = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
            let ok = GetProcessTimes(h, &mut created, &mut exited, &mut kernel, &mut user) != 0;
            CloseHandle(h);
            (running && ok).then(|| {
                (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime)
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(pid: u32, created: u64) -> ProcessId {
        ProcessId { pid, created }
    }

    #[test]
    fn markers_of_dead_or_reused_pids_are_stale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("10.run"), "111").unwrap(); // still running
        std::fs::write(dir.path().join("20.run"), "222").unwrap(); // gone
        std::fs::write(dir.path().join("30.run"), "333").unwrap(); // pid reused (other start time)
        std::fs::write(dir.path().join("junk.txt"), "x").unwrap();
        let alive = |p: ProcessId| p == id(10, 111) || p == id(30, 999);
        let mut stale = stale_markers(dir.path(), &alive);
        stale.sort_by_key(|p| p.pid);
        assert_eq!(stale, vec![id(20, 222), id(30, 333)]);
    }

    #[test]
    fn start_removes_stale_markers_and_exit_removes_its_own() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("20.run"), "222").unwrap();
        let m = RunMarker::start(Some(dir.path()), id(40, 444), |_| false);
        assert!(!dir.path().join("20.run").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("40.run")).unwrap(),
            "444"
        );
        m.exiting("test");
        assert!(!dir.path().join("40.run").exists());
    }

    #[test]
    fn this_process_is_alive_and_a_changed_start_time_is_not() {
        let me = current_process();
        assert!(me.created > 0);
        assert!(is_alive(me));
        assert!(!is_alive(ProcessId {
            created: me.created + 1,
            ..me
        }));
    }
}
