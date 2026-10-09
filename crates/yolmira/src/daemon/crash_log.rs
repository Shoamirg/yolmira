//! Why a detached daemon died, written where it survives the death (Phase 1 exit
//! checks: one daemon exited with 0xC0000409 and left no trace). A detached daemon has no
//! console, so stderr went nowhere, and the log writer is non-blocking, so its last lines
//! are lost on an abort. Now:
//! - stderr is `logs\yolmirad-stderr.log` (appended): Rust's own messages before an abort
//!   ("has overflowed its stack", a panic during a panic) land there;
//! - a panic hook writes the panic and a backtrace there synchronously, then logs it.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use windows_sys::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};

pub const FILE: &str = "yolmirad-stderr.log";

/// Point this process's stderr at the crash log. Call early, for a detached daemon only
/// (a foreground one keeps its console).
pub fn install(log_dir: &Path) {
    let file = match OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join(FILE))
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("yolmirad: crash log not opened: {e}");
            return;
        }
    };
    // SAFETY: a valid, open file handle; it is leaked below, so it stays valid for the
    // rest of the process.
    if unsafe { SetStdHandle(STD_ERROR_HANDLE, file.as_raw_handle()) } == 0 {
        return;
    }
    let file: &'static File = Box::leak(Box::new(file));
    let _ = writeln!(
        &*file,
        "--- yolmirad pid {} started {}",
        std::process::id(),
        yolmira_core::time::now_rfc3339()
    );
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let backtrace = std::backtrace::Backtrace::force_capture();
        let _ = writeln!(
            &*file,
            "panic in thread {:?} at {}: {info}\n{backtrace}",
            thread.name().unwrap_or("<unnamed>"),
            yolmira_core::time::now_rfc3339()
        );
        tracing::error!(thread = thread.name(), panic = %info, "panic");
    }));
}
