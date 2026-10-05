//! AMIRI desktop UI: a client of `amirid` (phase1-plan Slice C). It starts the daemon
//! if needed, through the same `ensure_daemon()` as the CLI, and never stops it, so
//! shells outlive the window.

// A GUI program in every build: no console of its own to close under it.
#![windows_subsystem = "windows"]

mod args;
mod commands;
mod daemon;
mod errors;

use amiri_core::AmiriError;
use amiri_ipc::Instance;
use daemon::Daemon;
use std::sync::Arc;
use tauri::webview::NewWindowResponse;
use tauri::{Url, WebviewUrl, WebviewWindowBuilder};

/// Tests give each UI its own WebView2 data folder, so two windows get two browser
/// processes (and two debugging ports). Unset for normal use.
const WEBVIEW_DATA_ENV: &str = "AMIRI_UI_WEBVIEW_DATA";
/// `npm run tauri dev` serves the frontend here (tauri.conf.json `devUrl`).
const DEV_ORIGIN: (&str, u16) = ("127.0.0.1", 5173);

fn resolve_instance() -> Result<Instance, AmiriError> {
    let fail = |why: String| {
        AmiriError::new("Start the AMIRI UI", "ui", why)
            .next_step("Run it without arguments, or with --instance <name> --home <folder>.")
    };
    let a = args::parse(std::env::args().skip(1)).map_err(fail)?;
    Instance::resolve(a.instance, a.home).map_err(|e| fail(e.to_string()))
}

/// Daily log files in the instance's log folder, next to the daemon's.
fn init_logging(
    instance: &Result<Instance, AmiriError>,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let dir = instance.as_ref().ok()?.log_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let (writer, guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::daily(dir, "ui.log"));
    let filter = tracing_subscriber::EnvFilter::try_from_env("AMIRI_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .init();
    Some(guard)
}

/// The webview only ever shows the app's own pages: no navigation elsewhere (e.g. a
/// link printed into a terminal), and no new windows.
fn is_app_url(url: &Url) -> bool {
    let host = url.host_str();
    host == Some("tauri.localhost")
        || (tauri::is_dev() && host == Some(DEV_ORIGIN.0) && url.port() == Some(DEV_ORIGIN.1))
}

/// A GUI program has no console: show a fatal error where the user sees it.
fn fatal(e: &AmiriError) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let text = wide(&e.to_string());
    let title = wide("AMIRI");
    // SAFETY: both pointers are NUL-terminated UTF-16 buffers that outlive the call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}

fn main() {
    let instance = resolve_instance();
    let log = init_logging(&instance);
    let daemon = Daemon::new(instance);
    let for_setup = Arc::clone(&daemon);
    let result = tauri::Builder::default()
        .manage(daemon)
        .invoke_handler(tauri::generate_handler![
            commands::daemon_state,
            commands::daemon_start,
            commands::session_list,
            commands::session_create,
            commands::session_attach,
            commands::session_detach,
            commands::session_input,
            commands::session_resize,
            commands::session_kill,
        ])
        .setup(move |app| {
            let mut window =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                    .title("AMIRI")
                    .inner_size(1240.0, 780.0)
                    .min_inner_size(720.0, 420.0)
                    .on_navigation(is_app_url)
                    .on_new_window(|_, _| NewWindowResponse::Deny);
            if let Some(dir) = std::env::var_os(WEBVIEW_DATA_ENV) {
                window = window.data_directory(dir.into());
            }
            window.build()?;
            daemon::supervise(app.handle().clone(), for_setup);
            Ok(())
        })
        .run(tauri::generate_context!());
    if let Err(e) = result {
        let err = AmiriError::new("Run the AMIRI UI", "ui", e.to_string())
            .next_step("Check that the Microsoft Edge WebView2 Runtime is installed.")
            .next_step("Run `amiri doctor`.");
        tracing::error!("{err}");
        drop(log); // flush the log before exiting
        fatal(&err);
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_apps_own_pages_may_load() {
        let ok = |u: &str| is_app_url(&Url::parse(u).unwrap());
        assert!(ok("http://tauri.localhost/index.html"));
        assert!(!ok("https://example.com/"));
        assert!(!ok("file:///C:/Windows/System32/"));
        assert!(!ok("ms-msdt:/id"));
        assert!(!ok("http://tauri.localhost.example.com/"));
    }
}
