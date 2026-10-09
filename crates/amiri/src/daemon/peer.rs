//! Who is on the other end of a connection, as Windows reports it (the client can't
//! choose these answers): its process, whether that process runs inside one of the
//! daemon's agent jobs, and whether it is the genuine AMIRI UI.
//!
//! Agents will run as the user, with a shell, and can read the pipe token
//! (ARCHITECTURE §9.6). So the token alone must not let a process approve requests:
//! approvals from inside an agent job are refused, and the one-time approval code is
//! only handed to the real UI (policy.rs).

use amiri_ipc::win::process::{pipe_client_pid, Job, ProcessWatch};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// The UI the daemon trusts: `amiri-app.exe` next to `amirid.exe`.
const UI_EXE: &str = "amiri-app.exe";
/// Debug builds only: a test may name its own executable as the trusted UI in this
/// file of the instance's config folder (tests can't run as amiri-app.exe). A file,
/// not a variable: the daemon's environment is an allowlist. Release builds ignore it.
#[cfg(debug_assertions)]
pub const TEST_TRUSTED_UI_FILE: &str = "debug-trusted-ui.txt";

/// The client process of one connection. The handle is opened as soon as the client
/// connects and held, so a reused pid can't be mistaken for it later.
pub struct Peer {
    pub pid: Option<u32>,
    process: Option<ProcessWatch>,
    image: Option<PathBuf>,
}

impl Peer {
    pub fn of_pipe(pipe: std::os::windows::io::RawHandle) -> Self {
        let pid = pipe_client_pid(pipe)
            .map_err(|e| tracing::warn!(error = %e, "pipe client pid unknown"))
            .ok();
        let process = pid.and_then(|p| {
            ProcessWatch::open(p)
                .map_err(|e| tracing::warn!(pid = p, error = %e, "pipe client not opened"))
                .ok()
        });
        let image = process.as_ref().and_then(|p| p.image_path().ok());
        Self {
            pid,
            process,
            image,
        }
    }

    #[cfg(test)]
    pub fn unknown() -> Self {
        Self {
            pid: None,
            process: None,
            image: None,
        }
    }

    pub fn image(&self) -> Option<&Path> {
        self.image.as_deref()
    }
}

/// Jobs holding agent processes (ARCHITECTURE §9.6). Phase 1 starts none outside
/// tests; Phase 4's adapters spawn agents through `spawn`.
#[derive(Default)]
pub struct AgentJobs {
    jobs: Mutex<Vec<Arc<Job>>>,
}

impl AgentJobs {
    /// A new kill-on-close job without breakaway, kept for the daemon's lifetime.
    pub fn create(&self) -> std::io::Result<Arc<Job>> {
        let job = Arc::new(Job::new(true, false)?);
        self.jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::clone(&job));
        Ok(job)
    }

    /// Whether the peer runs inside any agent job. Unknown counts as inside: a
    /// process the daemon can't identify gets no say over approvals.
    pub fn contains(&self, peer: &Peer) -> bool {
        let Some(process) = &peer.process else {
            return true;
        };
        let jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        jobs.iter().any(|job| process.in_job(job).unwrap_or(true))
    }
}

/// The genuine UI: the expected executable and not inside an agent job (an agent
/// could start the UI binary itself; it would run inside the agent's job).
pub fn is_trusted_ui(peer: &Peer, jobs: &AgentJobs, config_dir: &Path) -> bool {
    let Some(image) = peer.image() else {
        return false;
    };
    !jobs.contains(peer)
        && trusted_ui_paths(config_dir)
            .iter()
            .any(|p| same_path(p, image))
}

fn trusted_ui_paths(_config_dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::env::current_exe()
        .ok()
        .map(|daemon| daemon.with_file_name(UI_EXE))
        .into_iter()
        .collect();
    #[cfg(debug_assertions)]
    if let Ok(test) = std::fs::read_to_string(_config_dir.join(TEST_TRUSTED_UI_FILE)) {
        paths.push(PathBuf::from(test.trim()));
    }
    paths
}

/// Windows paths compare case-insensitively.
fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unidentified_peer_is_treated_as_an_agent_and_not_as_the_ui() {
        let jobs = AgentJobs::default();
        let peer = Peer::unknown();
        assert!(jobs.contains(&peer));
        assert!(!is_trusted_ui(&peer, &jobs, Path::new(".")));
    }

    #[test]
    fn paths_compare_without_case() {
        assert!(same_path(
            Path::new(r"C:\A\amiri-app.exe"),
            Path::new(r"c:\a\AMIRI-APP.EXE")
        ));
        assert!(!same_path(
            Path::new(r"C:\A\amiri-app.exe"),
            Path::new(r"C:\B\amiri-app.exe")
        ));
    }
}
