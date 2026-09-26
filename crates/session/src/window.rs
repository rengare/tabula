//! Flow control for clients that acknowledge decoded frames.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Frames that never get acked (dropped on a full network queue, or by the
/// client's decoder) stop counting against the window after this long.
const EXPIRY: Duration = Duration::from_millis(500);

pub(crate) struct InFlight {
    max: usize,
    frames: Mutex<VecDeque<(u64, Instant)>>,
}

impl InFlight {
    pub(crate) fn new(max: usize) -> Self {
        Self { max, frames: Mutex::new(VecDeque::new()) }
    }

    pub(crate) fn push(&self, pts: u64) {
        self.frames.lock().unwrap().push_back((pts, Instant::now()));
    }

    /// Acks are cumulative: everything up to `pts` has left the decoder.
    pub(crate) fn ack(&self, pts: u64) {
        let mut frames = self.frames.lock().unwrap();
        while frames.front().is_some_and(|&(p, _)| p <= pts) {
            frames.pop_front();
        }
    }

    pub(crate) fn clear(&self) {
        self.frames.lock().unwrap().clear();
    }

    pub(crate) fn is_full(&self) -> bool {
        let mut frames = self.frames.lock().unwrap();
        let now = Instant::now();
        while frames.front().is_some_and(|&(_, t)| now - t > EXPIRY) {
            frames.pop_front();
        }
        frames.len() >= self.max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_fills_and_drains() {
        let w = InFlight::new(2);
        w.push(10);
        assert!(!w.is_full());
        w.push(20);
        assert!(w.is_full());
        w.ack(10);
        assert!(!w.is_full());
        w.push(30);
        w.ack(30); // cumulative: also covers 20
        assert!(!w.is_full());
        w.push(40);
        w.push(50);
        w.clear();
        assert!(!w.is_full());
    }
}
