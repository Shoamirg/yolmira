//! `amiri`: the command-line client. Every command talks to the daemon over its pipe.

mod approve;
mod attach;
mod commands;
mod console;
pub mod doctor;
mod identity;
mod output;
mod project;
mod shell;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(name = "amiri", version, about = "AMIRI command-line client")]
pub struct Cli {
    /// Named daemon instance (tests); omit for your own daemon.
    #[arg(long, global = true)]
    pub instance: Option<String>,
    /// Data folder (default %LOCALAPPDATA%\AMIRI).
    #[arg(long, global = true)]
    pub home: Option<PathBuf>,
    /// Machine-readable JSON output.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Start, stop or inspect the daemon.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Terminal sessions run by the daemon (they survive this window).
    Shell {
        #[command(subcommand)]
        action: ShellAction,
    },
    /// Check this machine for problems that break AMIRI or other tools (no daemon needed).
    Doctor,
    /// Answer a request the policy waits on (the code is shown in the AMIRI window).
    Approve {
        /// The request's id (from `--list` or the AMIRI window).
        id: Option<String>,
        /// List the requests waiting for an answer.
        #[arg(long)]
        list: bool,
        /// The one-time code the AMIRI window shows for this request.
        #[arg(long)]
        code: Option<String>,
        /// Deny instead of approving.
        #[arg(long)]
        deny: bool,
        /// Approve the same action (and project) until the daemon stops.
        #[arg(long)]
        session: bool,
    },
    /// Who this machine acts for (person) and which install it is.
    Identity {
        #[command(subcommand)]
        action: IdentityAction,
    },
    /// Registered projects: a name for a folder that shells can start in.
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    /// Show the event log.
    Events {
        /// Only events with seq greater than this.
        #[arg(long, default_value_t = 0)]
        after: i64,
        #[arg(long, default_value_t = 100)]
        limit: u32,
        /// Only kinds starting with this prefix, e.g. `daemon.`.
        #[arg(long)]
        kind: Option<String>,
        /// Keep printing new events.
        #[arg(long)]
        follow: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ShellAction {
    /// Start a shell in the daemon.
    New {
        /// cmd, powershell, pwsh or wsl.
        #[arg(long, default_value = "cmd")]
        kind: String,
        /// WSL distribution (with --kind wsl).
        #[arg(long)]
        distro: Option<String>,
        /// Working folder (default: the project's folder, else your profile folder).
        #[arg(long)]
        cwd: Option<String>,
        /// A registered project: start in its folder; its policy rules apply.
        #[arg(long)]
        project: Option<String>,
        /// Attach right away.
        #[arg(long)]
        attach: bool,
    },
    /// List sessions, running and ended.
    List,
    /// Attach this console to a session (Ctrl+] detaches; the shell keeps running).
    Attach { id: String },
    /// End a session's shell.
    Kill { id: String },
}

#[derive(Subcommand, Debug)]
pub enum IdentityAction {
    /// Show the person and install ids.
    Show,
    /// Act for the person in another machine's identity.toml (this install keeps its id).
    Import {
        file: PathBuf,
        /// Replace a different person already set up here.
        #[arg(long)]
        replace: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ProjectAction {
    /// Register a folder under a name (registering a name again moves it).
    Add { name: String, path: String },
    /// List registered projects.
    List,
    /// Forget a project (its folder is untouched).
    Remove { name: String },
}

#[derive(Subcommand, Debug)]
pub enum DaemonAction {
    /// Start the daemon if it isn't running (detached; survives this terminal).
    Start {
        /// Allow an Administrator token (ARCHITECTURE §6.5).
        #[arg(long)]
        allow_elevated: bool,
    },
    /// Stop the daemon (ends every shell it runs).
    Stop,
    /// Show whether the daemon runs, and its details.
    Status,
}

/// Exit codes: 0 ok, 1 error (for `doctor`: a check failed), 2 invalid arguments (clap),
/// 10 daemon not running (status/stop).
pub mod exit {
    pub const OK: u8 = 0;
    pub const ERROR: u8 = 1;
    pub const NOT_RUNNING: u8 = 10;
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("amiri: cannot start async runtime: {e}");
            return ExitCode::from(exit::ERROR);
        }
    };
    ExitCode::from(rt.block_on(commands::run(cli)))
}
