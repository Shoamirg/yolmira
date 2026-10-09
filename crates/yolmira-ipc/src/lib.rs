//! The daemon protocol (shared by `yolmirad`, the CLI and the UI), the client, the
//! launcher, and the Windows APIs they need.

pub mod client;
pub mod framing;
pub mod launch;
pub mod paths;
pub mod protocol;
pub mod win;

pub use client::{Client, ClientError};
pub use launch::{ensure_daemon, ensure_daemon_blocking, LaunchOptions, Launched};
pub use paths::Instance;
