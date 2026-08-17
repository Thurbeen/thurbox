//! Wall-clock-free counters for the render loop.
//!
//! v1 learned to assert on counters rather than timings: a test that says
//! "idle should be fast" is flaky on shared hardware, while one that says "an
//! idle loop painted no frames" is exact. These are the v2 equivalents of
//! `MetricsState::perf`, and they exist so the demand-driven redraw (design.md
//! D12) is provable rather than hoped for.

use std::sync::atomic::{AtomicU64, Ordering};

/// One counter per thing worth knowing about the loop.
#[derive(Default)]
pub struct Counters {
    /// Times round the loop, painted or not.
    pub iterations: AtomicU64,
    /// Frames actually painted.
    pub frames: AtomicU64,
    /// Frames skipped because nothing had changed.
    pub skipped: AtomicU64,
    /// Plugin render calls.
    pub renders: AtomicU64,
    /// Plugin failures, of any phase.
    pub failures: AtomicU64,
    /// Whole-VM reloads.
    pub reloads: AtomicU64,
}

impl Counters {
    pub fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

    /// A snapshot of every counter, for a HUD or a test.
    pub fn read(&self) -> Snapshot {
        Snapshot {
            iterations: Self::get(&self.iterations),
            frames: Self::get(&self.frames),
            skipped: Self::get(&self.skipped),
            renders: Self::get(&self.renders),
            failures: Self::get(&self.failures),
            reloads: Self::get(&self.reloads),
        }
    }
}

/// The counters at one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub iterations: u64,
    pub frames: u64,
    pub skipped: u64,
    pub renders: u64,
    pub failures: u64,
    pub reloads: u64,
}

impl Snapshot {
    /// Difference from an earlier reading, for asserting on a window rather
    /// than on absolute totals.
    pub fn since(&self, earlier: &Snapshot) -> Snapshot {
        Snapshot {
            iterations: self.iterations.saturating_sub(earlier.iterations),
            frames: self.frames.saturating_sub(earlier.frames),
            skipped: self.skipped.saturating_sub(earlier.skipped),
            renders: self.renders.saturating_sub(earlier.renders),
            failures: self.failures.saturating_sub(earlier.failures),
            reloads: self.reloads.saturating_sub(earlier.reloads),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_start_at_zero_and_count_up() {
        let counters = Counters::default();
        assert_eq!(counters.read(), Snapshot::default());
        Counters::bump(&counters.frames);
        Counters::bump(&counters.frames);
        assert_eq!(counters.read().frames, 2);
    }

    #[test]
    fn a_window_is_the_difference_between_two_readings() {
        // Asserting on absolute totals would make a test depend on everything
        // that ran before it.
        let counters = Counters::default();
        Counters::bump(&counters.frames);
        let before = counters.read();
        Counters::bump(&counters.frames);
        Counters::bump(&counters.skipped);
        let window = counters.read().since(&before);
        assert_eq!(window.frames, 1);
        assert_eq!(window.skipped, 1);
        assert_eq!(window.iterations, 0);
    }
}
