//! Console control (decisions "Ctrl+C in shells", verified by S1d) and session-end
//! handling for the detached daemon, which has no console.

use super::shutdown;
use std::time::Duration;
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Console::{
    SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT,
    CTRL_SHUTDOWN_EVENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW,
    TranslateMessage, MSG, WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSW,
};

/// Windows gives a console handler about 5 s after CTRL_CLOSE; finish within that.
const CLOSE_BUDGET: Duration = Duration::from_millis(3500);
const MAIN_EXIT_GRACE: Duration = Duration::from_millis(700);

/// Clear the ignore-Ctrl+C flag a CREATE_NEW_PROCESS_GROUP launcher leaves behind (children
/// inherit it), and install AMIRI's handler: survive Ctrl+C/Break, shut down cleanly on
/// close, logoff and shutdown. Returns false if either call failed.
pub fn install() -> bool {
    // SAFETY: null handler + FALSE clears this process's ignore flag; the handler is a
    // static `extern "system"` function with the signature Windows expects.
    unsafe { SetConsoleCtrlHandler(None, 0) != 0 && SetConsoleCtrlHandler(Some(handler), 1) != 0 }
}

extern "system" fn handler(ctrl: u32) -> BOOL {
    match ctrl {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => {
            tracing::info!(ctrl, "ignored console Ctrl+C/Ctrl+Break");
            1
        }
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
            let reason = match ctrl {
                CTRL_CLOSE_EVENT => "console_close",
                CTRL_LOGOFF_EVENT => "logoff",
                _ => "system_shutdown",
            };
            shutdown::request(reason);
            if shutdown::wait_done(CLOSE_BUDGET) {
                // The main thread is returning from `main` now, which flushes the log;
                // give it that moment before ending the process from this thread.
                std::thread::sleep(MAIN_EXIT_GRACE);
            }
            std::process::exit(0);
        }
        _ => 0,
    }
}

/// A hidden top-level window: without a console, a detached process learns about logoff
/// and shutdown only through WM_QUERYENDSESSION / WM_ENDSESSION.
pub fn spawn_session_end_listener() {
    std::thread::spawn(|| {
        let class = amiri_ipc::win::wide("AMIRI_amirid_session");
        // SAFETY: standard window-class registration with a static window procedure; the
        // class name buffer outlives the calls; the message loop runs on this thread only.
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = Some(window_proc);
            wc.hInstance = instance;
            wc.lpszClassName = class.as_ptr();
            if RegisterClassW(&wc) == 0 {
                tracing::warn!("session-end window class registration failed");
                return;
            }
            // Top-level (not message-only): message-only windows don't get broadcasts.
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                tracing::warn!("session-end window creation failed");
                return;
            }
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    });
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wparam != 0 {
                shutdown::request("session_end");
                shutdown::wait_done(CLOSE_BUDGET);
            }
            0
        }
        // SAFETY: forwarding with the arguments Windows passed us.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
