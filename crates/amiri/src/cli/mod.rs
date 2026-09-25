//! `amiri`: the command-line client. Every command talks to the daemon over its pipe.

mod commands;
mod output;

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

/// Exit codes: 0 ok, 1 error, 2 invalid arguments (clap), 10 daemon not running (status/stop).
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
