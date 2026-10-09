//! Yolmira domain types shared by the daemon, the store, the IPC layer and clients.
//! This crate does no I/O (ARCHITECTURE §4.2).

pub mod diff;
pub mod error;
pub mod event;
pub mod policy;
pub mod time;

pub use error::YolmiraError;
pub use event::{Event, NewEvent};
