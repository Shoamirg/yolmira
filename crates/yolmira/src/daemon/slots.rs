//! Who may hold a connection (Phase 1 exit checks). Each connection holds an outbox
//! (`outbox`), so their number bounds the daemon's memory, and a misbehaving local process
//! must not lock the user out of their own UI:
//! - at most `MAX_CONNECTIONS` for everyone;
//! - beyond that, `UI_RESERVED` more only for the verified UI (`peer::is_trusted_ui`);
//! - at most `PER_PROCESS` from any one process; the verified UI may use `UI_PER_PROCESS`
//!   (a window uses a few connections plus one per attached shell).
//!
//! Admission happens on accept, before `hello`: a refused connection is closed at once.

use super::sessions::MAX_LIVE_SESSIONS;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

pub const MAX_CONNECTIONS: usize = 128;
pub const PER_PROCESS: usize = 16;
pub const UI_PER_PROCESS: usize = MAX_LIVE_SESSIONS + 16;
pub const UI_RESERVED: usize = UI_PER_PROCESS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    TooManyConnections,
    TooManyFromProcess,
}

impl Refused {
    pub fn reason(self) -> &'static str {
        match self {
            Self::TooManyConnections => "too_many_connections",
            Self::TooManyFromProcess => "too_many_connections_from_process",
        }
    }
}

#[derive(Default)]
struct Counts {
    general: usize,
    reserved: usize,
    /// Open connections per client process (`None`: the pid couldn't be read; all such
    /// connections share one allowance).
    per_process: HashMap<Option<u32>, usize>,
}

#[derive(Clone)]
pub struct Slots {
    counts: Arc<Mutex<Counts>>,
    limits: (usize, usize, usize, usize),
}

impl Default for Slots {
    fn default() -> Self {
        Self::with_limits(MAX_CONNECTIONS, UI_RESERVED, PER_PROCESS, UI_PER_PROCESS)
    }
}

/// A held connection slot; released on drop.
pub struct Slot {
    counts: Arc<Mutex<Counts>>,
    pid: Option<u32>,
    reserved: bool,
}

impl Slots {
    pub fn with_limits(total: usize, ui_reserved: usize, per: usize, ui_per: usize) -> Self {
        Self {
            counts: Arc::default(),
            limits: (total, ui_reserved, per, ui_per),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Counts> {
        self.counts.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn admit(&self, pid: Option<u32>, verified_ui: bool) -> Result<Slot, Refused> {
        let (total, ui_reserved, per, ui_per) = self.limits;
        let mut c = self.lock();
        let mine = c.per_process.get(&pid).copied().unwrap_or(0);
        if mine >= if verified_ui { ui_per } else { per } {
            return Err(Refused::TooManyFromProcess);
        }
        let reserved = if c.general < total {
            c.general += 1;
            false
        } else if verified_ui && c.reserved < ui_reserved {
            c.reserved += 1;
            true
        } else {
            return Err(Refused::TooManyConnections);
        };
        *c.per_process.entry(pid).or_insert(0) += 1;
        Ok(Slot {
            counts: Arc::clone(&self.counts),
            pid,
            reserved,
        })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut c = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        if self.reserved {
            c.reserved -= 1;
        } else {
            c.general -= 1;
        }
        if let Some(n) = c.per_process.get_mut(&self.pid) {
            *n -= 1;
            if *n == 0 {
                c.per_process.remove(&self.pid);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_process_gets_at_most_its_share() {
        let slots = Slots::with_limits(10, 2, 3, 5);
        let held: Vec<_> = (0..3)
            .map(|_| slots.admit(Some(1), false).unwrap())
            .collect();
        assert_eq!(
            slots.admit(Some(1), false).err(),
            Some(Refused::TooManyFromProcess)
        );
        assert!(
            slots.admit(Some(2), false).is_ok(),
            "another process still gets in"
        );
        drop(held);
        assert!(slots.admit(Some(1), false).is_ok(), "released on drop");
    }

    #[test]
    fn the_verified_ui_gets_in_when_everyone_elses_slots_are_taken() {
        let slots = Slots::with_limits(4, 2, 2, 3);
        let _others: Vec<_> = (0..4)
            .map(|i| slots.admit(Some(10 + i / 2), false).unwrap())
            .collect();
        assert_eq!(
            slots.admit(Some(20), false).err(),
            Some(Refused::TooManyConnections)
        );
        let ui1 = slots
            .admit(Some(99), true)
            .expect("the UI uses the reserve");
        let _ui2 = slots
            .admit(Some(99), true)
            .expect("the UI uses the reserve");
        assert_eq!(
            slots.admit(Some(98), true).err(),
            Some(Refused::TooManyConnections),
            "the reserve is bounded too"
        );
        drop(ui1);
        assert!(
            slots.admit(Some(98), true).is_ok(),
            "a reserved slot comes back"
        );
    }

    #[test]
    fn processes_without_a_known_pid_share_one_allowance() {
        let slots = Slots::with_limits(10, 0, 2, 2);
        let _a = slots.admit(None, false).unwrap();
        let _b = slots.admit(None, false).unwrap();
        assert_eq!(
            slots.admit(None, false).err(),
            Some(Refused::TooManyFromProcess)
        );
    }
}
