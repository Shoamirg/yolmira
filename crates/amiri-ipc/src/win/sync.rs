//! Named mutexes: the daemon's single-instance guard and the launcher's start lock.

use super::{last_error, wide, OwnedHandle};
use std::io;
use std::marker::PhantomData;
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

pub struct NamedMutex {
    handle: OwnedHandle,
}

/// Held while alive; released on drop. Windows ties mutex ownership to the acquiring
/// thread, so the guard is `!Send` (the raw-pointer marker) and can't be dropped elsewhere.
pub struct MutexGuard<'a> {
    mutex: &'a NamedMutex,
    _not_send: PhantomData<*const ()>,
}

impl NamedMutex {
    /// Open or create. `existed` tells whether another process already created it.
    pub fn open(name: &str) -> io::Result<(Self, bool)> {
        let n = wide(name);
        // SAFETY: `n` is NUL-terminated; default security (the creator's token).
        let h = unsafe { CreateMutexW(std::ptr::null(), 0, n.as_ptr()) };
        let existed = last_error() == ERROR_ALREADY_EXISTS;
        let handle = OwnedHandle::new(h).ok_or_else(io::Error::last_os_error)?;
        Ok((Self { handle }, existed))
    }

    /// Wait up to `timeout`. An abandoned mutex (owner died) counts as acquired.
    pub fn acquire(&self, timeout: Duration) -> io::Result<Option<MutexGuard<'_>>> {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: valid handle owned by `self`.
        match unsafe { WaitForSingleObject(self.handle.raw(), ms) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Some(MutexGuard {
                mutex: self,
                _not_send: PhantomData,
            })),
            WAIT_TIMEOUT => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

impl Drop for MutexGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: this thread owns the mutex (acquired above).
        unsafe { ReleaseMutex(self.mutex.handle.raw()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_open_sees_existing_and_acquire_times_out_while_held() {
        let name = format!("Local\\AMIRI-test-{}", uuid::Uuid::new_v4());
        let (a, existed_a) = NamedMutex::open(&name).unwrap();
        assert!(!existed_a);
        let guard = a.acquire(Duration::from_millis(100)).unwrap();
        assert!(guard.is_some());
        let name2 = name.clone();
        let held = std::thread::spawn(move || {
            let (b, existed_b) = NamedMutex::open(&name2).unwrap();
            let acquired = b.acquire(Duration::from_millis(100)).unwrap().is_some();
            (existed_b, acquired)
        })
        .join()
        .unwrap();
        assert_eq!(held, (true, false));
    }
}
