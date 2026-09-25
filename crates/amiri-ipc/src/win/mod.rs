//! Thin safe wrappers over the Win32 calls AMIRI needs. Every `unsafe` block lives
//! under this module and states its invariant.

pub mod env;
pub mod process;
pub mod security;
pub mod sync;

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};

/// NUL-terminated UTF-16 for `PCWSTR` parameters.
pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// UTF-16 up to (not including) the first NUL.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// The calling thread's last Win32 error code. Locale-neutral, unlike its message.
pub fn last_error() -> u32 {
    // SAFETY: no preconditions.
    unsafe { GetLastError() }
}

/// An owned kernel handle, closed on drop.
#[derive(Debug)]
pub struct OwnedHandle(HANDLE);

// SAFETY: kernel handles may be used and closed from any thread.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl OwnedHandle {
    /// Takes ownership of `h`; `None` for null or `INVALID_HANDLE_VALUE`.
    pub fn new(h: HANDLE) -> Option<Self> {
        (!h.is_null() && h != INVALID_HANDLE_VALUE).then_some(Self(h))
    }

    pub fn raw(&self) -> HANDLE {
        self.0
    }

    /// Give up ownership without closing.
    pub fn into_raw(self) -> HANDLE {
        let h = self.0;
        std::mem::forget(self);
        h
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: we own the handle and close it exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_round_trips_non_ascii() {
        let w = wide("C:\\Проект\\oʻzbek");
        assert_eq!(w.last(), Some(&0));
        assert_eq!(from_wide(&w), "C:\\Проект\\oʻzbek");
    }
}
