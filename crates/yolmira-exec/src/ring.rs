//! Bounded scrollback: the most recent output, replayed to clients that attach.

use std::collections::VecDeque;

/// Default scrollback per session (phase1-plan Slice B).
pub const DEFAULT_CAPACITY: usize = 2 * 1024 * 1024;

#[derive(Debug)]
pub struct Ring {
    buf: VecDeque<u8>,
    capacity: usize,
    /// Bytes ever written, including those dropped from the front.
    total: u64,
}

impl Ring {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity.min(64 * 1024)),
            capacity,
            total: 0,
        }
    }

    pub fn push(&mut self, data: &[u8]) {
        self.total += data.len() as u64;
        let data = if data.len() > self.capacity {
            &data[data.len() - self.capacity..]
        } else {
            data
        };
        let overflow = (self.buf.len() + data.len()).saturating_sub(self.capacity);
        self.buf.drain(..overflow);
        self.buf.extend(data);
    }

    pub fn snapshot(&self) -> Vec<u8> {
        self.buf.iter().copied().collect()
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn total(&self) -> u64 {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newest_bytes_within_capacity() {
        let mut r = Ring::new(5);
        r.push(b"abc");
        r.push(b"defg");
        assert_eq!(r.snapshot(), b"cdefg");
        r.push(b"0123456789");
        assert_eq!(r.snapshot(), b"56789");
        assert_eq!(r.total(), 17);
        assert_eq!(r.len(), 5);
    }
}
