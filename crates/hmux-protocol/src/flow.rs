//! Per-view rendering credit. The owner provides synchronization and wakeups;
//! this state machine cannot block the shared Home reader.
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

pub const CAPABILITY: &str = "terminal-output-flow-v1";
pub const CHUNK: usize = 16 << 10;
pub const FRAMES: usize = 32;
pub const BYTES: usize = CHUNK * FRAMES;
pub const STALL: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
pub struct OutputWindow {
    frames: VecDeque<(usize, Instant)>,
    bytes: usize,
}
impl OutputWindow {
    pub fn reserve(&mut self, n: usize, now: Instant) -> bool {
        if n == 0 || n > CHUNK || self.frames.len() >= FRAMES || n > BYTES - self.bytes {
            return false;
        }
        self.frames.push_back((n, now));
        self.bytes += n;
        true
    }
    pub fn acknowledge(&mut self, n: i64) -> bool {
        if self
            .frames
            .front()
            .is_none_or(|(size, _)| n != *size as i64)
        {
            return false;
        }
        self.bytes -= self.frames.pop_front().expect("front exists").0;
        true
    }
    pub fn stalled(&self, now: Instant) -> bool {
        self.frames
            .front()
            .is_some_and(|(_, queued)| now.saturating_duration_since(*queued) >= STALL)
    }
    pub fn retained_bytes(&self) -> usize {
        self.bytes
    }
    pub fn retained_frames(&self) -> usize {
        self.frames.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds_tiny_and_large_output_and_recovers_credit() {
        for size in [1, CHUNK] {
            let mut window = OutputWindow::default();
            let now = Instant::now();
            for _ in 0..FRAMES {
                assert!(window.reserve(size, now));
            }
            assert!(!window.reserve(size, now));
            for wrong in [-1, 0, size as i64 + 1] {
                assert!(!window.acknowledge(wrong));
            }
            assert_eq!(window.retained_bytes(), size * FRAMES);
            assert!(window.acknowledge(size as i64));
            assert!(window.reserve(size, now));
            for _ in 0..FRAMES {
                assert!(window.acknowledge(size as i64));
            }
            assert!(!window.acknowledge(size as i64));
            assert_eq!(window.retained_bytes(), 0);
            assert_eq!(window.retained_frames(), 0);
        }
    }
    #[test]
    fn partial_progress_does_not_extend_oldest_deadline() {
        let mut window = OutputWindow::default();
        let now = Instant::now();
        assert!(!window.reserve(0, now));
        assert!(!window.reserve(CHUNK + 1, now));
        assert!(window.reserve(1, now));
        assert!(window.reserve(2, now));
        assert!(window.reserve(3, now + STALL));
        assert!(!window.stalled(now + STALL - Duration::from_nanos(1)));
        assert!(window.acknowledge(1));
        assert!(window.stalled(now + STALL));
        assert!(!window.acknowledge(3));
        assert!(window.stalled(now + STALL));
        assert!(window.acknowledge(2));
        assert!(!window.stalled(now + STALL));
        assert!(window.acknowledge(3));
        assert!(!window.stalled(now + STALL * 10));
    }
}
