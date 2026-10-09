//! Raw VT console for `yolmira shell attach`: keys go to the remote shell as bytes
//! (Ctrl+C included), output is written as VT, code pages are UTF-8. Dropping the
//! guard resets terminal modes the remote side may have switched on and restores the
//! console, on every exit path.

use std::io::{self, Write};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Globalization::CP_UTF8;
use windows_sys::Win32::System::Console::{
    GetConsoleCP, GetConsoleMode, GetConsoleOutputCP, GetConsoleScreenBufferInfo, GetStdHandle,
    ReadConsoleW, SetConsoleCP, SetConsoleMode, SetConsoleOutputCP, CONSOLE_MODE,
    CONSOLE_SCREEN_BUFFER_INFO, DISABLE_NEWLINE_AUTO_RETURN, ENABLE_PROCESSED_OUTPUT,
    ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE,
};
use yolmira_core::YolmiraError;

/// Modes a remote program may have switched on in this terminal: win32-input-mode,
/// focus events, alternate screen, bracketed paste, mouse tracking, application cursor
/// and keypad, scroll region, character set; then show the cursor and reset colours.
const RESTORE_TERMINAL: &[u8] = b"\x1b[?9001l\x1b[?1004l\x1b[?1049l\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1l\x1b>\x1b[r\x1b(B\x1b[?25h\x1b[0m";

pub struct RawConsole {
    input: HANDLE,
    output: HANDLE,
    saved_in: CONSOLE_MODE,
    saved_out: CONSOLE_MODE,
    saved_cp: u32,
    saved_out_cp: u32,
}

// SAFETY: console handles are process-wide and usable from any thread.
unsafe impl Send for RawConsole {}

fn not_console() -> YolmiraError {
    YolmiraError::new(
        "Attach to the shell",
        "cli",
        "attach needs an interactive console (stdin/stdout are redirected)",
    )
    .next_step("Run `yolmira shell attach` in a terminal window.")
}

impl RawConsole {
    pub fn enable() -> Result<Self, YolmiraError> {
        // SAFETY: standard handles are owned by the process; the mode out-params are valid.
        let me = unsafe {
            let input = GetStdHandle(STD_INPUT_HANDLE);
            let output = GetStdHandle(STD_OUTPUT_HANDLE);
            let (mut saved_in, mut saved_out) = (0, 0);
            if GetConsoleMode(input, &mut saved_in) == 0
                || GetConsoleMode(output, &mut saved_out) == 0
            {
                return Err(not_console());
            }
            Self {
                input,
                output,
                saved_in,
                saved_out,
                saved_cp: GetConsoleCP(),
                saved_out_cp: GetConsoleOutputCP(),
            }
        };
        let out_mode = ENABLE_PROCESSED_OUTPUT
            | ENABLE_VIRTUAL_TERMINAL_PROCESSING
            | DISABLE_NEWLINE_AUTO_RETURN;
        // SAFETY: valid console handles from above. Input: VT sequences only; no line
        // editing, echo or Ctrl+C processing.
        let ok = unsafe {
            SetConsoleMode(me.input, ENABLE_VIRTUAL_TERMINAL_INPUT) != 0
                && SetConsoleMode(me.output, out_mode) != 0
                && SetConsoleCP(CP_UTF8) != 0
                && SetConsoleOutputCP(CP_UTF8) != 0
        };
        if !ok {
            let e = io::Error::last_os_error();
            drop(me); // restores what was changed
            return Err(YolmiraError::new(
                "Attach to the shell",
                "cli",
                format!("console setup failed: {e}"),
            )
            .next_step("Use Windows Terminal or a recent conhost (Windows 10 1809+)."));
        }
        Ok(me)
    }

    pub fn input_handle(&self) -> HANDLE {
        self.input
    }
}

impl Drop for RawConsole {
    fn drop(&mut self) {
        let mut stdout = io::stdout().lock();
        let _ = stdout
            .write_all(RESTORE_TERMINAL)
            .and_then(|_| stdout.flush());
        // SAFETY: restoring the values read in `enable`. Failures leave nothing better
        // to do while exiting; the console is the user's again either way.
        unsafe {
            SetConsoleMode(self.input, self.saved_in);
            SetConsoleMode(self.output, self.saved_out);
            SetConsoleCP(self.saved_cp);
            SetConsoleOutputCP(self.saved_out_cp);
        }
    }
}

/// Visible window size (cols, rows) of this process's console, without changing modes.
pub fn window_size() -> Option<(u16, u16)> {
    // SAFETY: all-zero is a valid CONSOLE_SCREEN_BUFFER_INFO; the handle is the
    // process's standard output; `info` is an out-param.
    let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
    if unsafe { GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) } == 0 {
        return None;
    }
    let w = info.srWindow;
    Some(((w.Right - w.Left + 1) as u16, (w.Bottom - w.Top + 1) as u16))
}

/// Reads console input as UTF-16 (`ReadConsoleW`: with VT input the keys arrive as
/// characters and VT sequences) and returns UTF-8. A surrogate pair split across two
/// reads is kept until complete.
pub struct InputReader {
    handle: HANDLE,
    pending_high: Option<u16>,
}

// SAFETY: the console input handle is process-wide.
unsafe impl Send for InputReader {}

impl InputReader {
    pub fn new(handle: HANDLE) -> Self {
        Self {
            handle,
            pending_high: None,
        }
    }

    /// Blocking. `Ok(None)` when the console is gone.
    pub fn read(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut buf = [0u16; 1024];
        let mut n = 0u32;
        // SAFETY: valid console input handle; `buf` holds `buf.len()` UTF-16 units.
        let ok = unsafe {
            ReadConsoleW(
                self.handle,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                &mut n,
                std::ptr::null(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut units: Vec<u16> = self.pending_high.take().into_iter().collect();
        units.extend_from_slice(&buf[..n as usize]);
        if let Some(&last) = units.last() {
            if (0xD800..0xDC00).contains(&last) {
                self.pending_high = units.pop();
            }
        }
        Ok(Some(String::from_utf16_lossy(&units).into_bytes()))
    }
}
