//! Local execution: interactive shells in ConPTY (phase1-plan Slice B).

pub mod pty;
pub mod ring;
pub mod shell;

pub use pty::{PtyEvent, PtySession, PtySpec};
pub use ring::Ring;
pub use shell::{resolve, ShellCommand, ShellKind};

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("{0} is not installed")]
    NotInstalled(&'static str),
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("could not start the shell: {0}")]
    Spawn(String),
    #[error("terminal I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
