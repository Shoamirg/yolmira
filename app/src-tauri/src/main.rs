//! AMIRI desktop UI: a client of `amirid` (phase1-plan Slice C). It starts the daemon
//! if needed, through the same `ensure_daemon()` as the CLI, and never stops it, so
//! shells outlive the window.

// A GUI program in every build: no console of its own to close under it.
#![windows_subsystem = "windows"]

mod args;
mod commands;
mod daemon;
mod errors;
mod lifecycle;

use amiri_core::AmiriError;
use amiri_ipc::Instance;
use daemon::Daemon;
use lifecycle::RunMarker;
use std::sync::Arc;
use tauri::webview::NewWindowResponse;
use tauri::{RunEvent, Url, WebviewUrl, WebviewWindowBuilder};

/// Tests give each UI its own WebView2 data folder, so two windows get two browser
/// processes (and two debugging ports). Read only by debug builds.
#[cfg(debug_assertions)]
const WEBVIEW_DATA_ENV: &str = "AMIRI_UI_WEBVIEW_DATA";
/// Tests can place the window ("x,y", e.g. beyond every monitor so test windows
/// don't pop up on a person's desktop). Read only by debug builds.
#[cfg(debug_assertions)]
const WINDOW_POSITION_ENV: &str = "AMIRI_UI_WINDOW_POSITION";

#[cfg(debug_assertions)]
fn window_position() -> Option<(f64, f64)> {
    let v = std::env::var(WINDOW_POSITION_ENV).ok()?;
    let (x, y) = v.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// WebView2 configures itself from `WEBVIEW2_*` variables: browser arguments (e.g. an
/// unauthenticated remote-debugging port, which can drive the session commands), the
/// browser folder, the data folder. The E2E tests rely on that in debug builds; a
/// release build takes none of them from its environment.
#[cfg(any(test, not(debug_assertions)))]
fn is_webview2_override(name: &str) -> bool {
    name.get(..9)
        .is_some_and(|p| p.eq_ignore_ascii_case("WEBVIEW2_"))
}

#[cfg(not(debug_assertions))]
fn drop_webview2_overrides() {
    let names: Vec<_> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .filter(|k| is_webview2_override(k))
        .collect();
    for name in names {
        // Called first thing in main, before any other thread exists.
        std::env::remove_var(name);
    }
}

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

/// Daily log files in the instance's log folder, next to the daemon's. Written
/// without a buffer: Tauri ends the process from its event loop, so a buffered
/// writer would lose the last lines, which say why the UI exited (lifecycle.rs).
fn init_logging(instance: &Result<Instance, AmiriError>) {
    let Some(dir) = instance.as_ref().ok().map(Instance::log_dir) else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let filter = tracing_subscriber::EnvFilter::try_from_env("AMIRI_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let result = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(tracing_appender::rolling::daily(&dir, "ui.log"))
        .with_ansi(false)
        .try_init();
    if let Err(e) = result {
        // No console and no log: leave the reason where the log would be.
        let _ = std::fs::write(dir.join("ui-logging-failed.txt"), e.to_string());
    }
}

/// Mark this UI as running (and report earlier ones that ended unrecorded).
fn run_marker(instance: &Result<Instance, AmiriError>) -> RunMarker {
    let dir = instance
        .as_ref()
        .ok()
        .map(|i| i.home.join("run").join("ui"));
    RunMarker::start(
        dir.as_deref(),
        lifecycle::current_process(),
        lifecycle::is_alive,
    )
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
    #[cfg(not(debug_assertions))]
    drop_webview2_overrides();
    let instance = resolve_instance();
    init_logging(&instance);
    tracing::info!(
        "UI starting: pid {}, version {}",
        std::process::id(),
        env!("CARGO_PKG_VERSION")
    );
    let marker = run_marker(&instance);
    // A GUI program has no console: a panic would otherwise vanish without a trace.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        default_hook(info);
    }));
    let daemon = Daemon::new(instance);
    let for_setup = Arc::clone(&daemon);
    let result = tauri::Builder::default()
        .manage(daemon)
        // Closing the window ends the UI (never the daemon); log it, so an exit that
        // looks like a crash can be told apart from a closed window.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                tracing::info!("window {} close requested", window.label());
                lifecycle::set_exit_reason(format!(
                    "the {} window was closed (close button, Alt+F4, the taskbar, or WM_CLOSE from another program)",
                    window.label()
                ));
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::daemon_state,
            commands::daemon_start,
            commands::session_list,
            commands::session_create,
            commands::session_attach,
            commands::session_ack,
            commands::session_detach,
            commands::session_input,
            commands::session_resize,
            commands::session_kill,
            commands::project_list,
            commands::project_add,
            commands::project_remove,
            commands::policy_pending,
            commands::policy_code,
            commands::policy_answer,
        ])
        .setup(move |app| {
            let window =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                    .title("AMIRI")
                    .inner_size(1240.0, 780.0)
                    .min_inner_size(720.0, 420.0)
                    .on_navigation(is_app_url)
                    .on_new_window(|_, _| NewWindowResponse::Deny);
            #[cfg(debug_assertions)]
            let window = match std::env::var_os(WEBVIEW_DATA_ENV) {
                Some(dir) => window.data_directory(dir.into()),
                None => window,
            };
            let built = window.build()?;
            // Windows ignores an off-screen position at creation; moving the built
            // window works.
            #[cfg(debug_assertions)]
            if let Some((x, y)) = window_position() {
                built.set_position(tauri::LogicalPosition::new(x, y))?;
                tracing::info!("window moved to {:?}", built.outer_position().ok());
            }
            #[cfg(not(debug_assertions))]
            let _ = built;
            daemon::supervise(app.handle().clone(), for_setup);
            Ok(())
        })
        .build(tauri::generate_context!());
    let app = match result {
        Ok(app) => app,
        Err(e) => {
            let err = AmiriError::new("Run the AMIRI UI", "ui", e.to_string())
                .next_step("Check that the Microsoft Edge WebView2 Runtime is installed.")
                .next_step("Run `amiri doctor`.");
            tracing::error!("{err}");
            lifecycle::set_exit_reason(format!("failed to start: {}", err.why));
            marker.exiting("failed to start");
            fatal(&err);
            std::process::exit(1);
        }
    };
    // Tauri ends the process when this loop ends; the reason is logged first.
    app.run(move |_, event| match event {
        RunEvent::ExitRequested { code, .. } => {
            lifecycle::set_exit_reason(format!("exit requested (code {code:?})"));
        }
        RunEvent::Exit => marker.exiting("the event loop ended"),
        _ => {}
    });
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

    #[test]
    fn webview2_overrides_are_recognised_in_any_case() {
        assert!(is_webview2_override(
            "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS"
        ));
        assert!(is_webview2_override("webview2_browser_executable_folder"));
        assert!(!is_webview2_override("WEBVIEW2"));
        assert!(!is_webview2_override("AMIRI_UI_WEBVIEW_DATA"));
        assert!(!is_webview2_override("Ж")); // non-ASCII: no slicing panic
    }
}
