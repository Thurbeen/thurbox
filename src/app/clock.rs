//! Monotonic "now" for the app layer's timers, fast-forwardable in tests.
//!
//! Every wall-clock comparison the TUI makes on the UI thread (status-message
//! expiry, the delete-undo window, the forced-redraw floor, the global-search
//! debounce, remote-reconnect retry pacing) reads time through [`now`] instead
//! of `Instant::now()`. In a normal build [`now`] *is* `Instant::now()`; under
//! `cfg(test)` it adds a thread-local offset that [`advance`] grows, so a test
//! can jump past any timeout deterministically — no sleeping, no flakes.
//!
//! The offset is thread-local because the `App` runs single-threaded (the main
//! event loop, or a test body / current-thread Tokio runtime): timestamps are
//! only ever taken *and* compared on that one thread. Background threads keep
//! their own time bases (`now_millis` in the agent layer) and are unaffected.

use std::time::{Duration, Instant};

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static TEST_OFFSET: Cell<Duration> = const { Cell::new(Duration::ZERO) };
}

/// The app layer's monotonic clock. Equals `Instant::now()` in production;
/// in tests it is shifted forward by whatever [`advance`] has accumulated on
/// the current thread.
pub(crate) fn now() -> Instant {
    #[cfg(test)]
    {
        Instant::now() + TEST_OFFSET.with(Cell::get)
    }
    #[cfg(not(test))]
    {
        Instant::now()
    }
}

/// How long ago `t` was on this clock — the [`now`]-based replacement for
/// `t.elapsed()`. Saturates to zero if `t` is in the future (a timestamp taken
/// after the offset grew).
pub(crate) fn elapsed_since(t: Instant) -> Duration {
    now().saturating_duration_since(t)
}

/// Fast-forward the current thread's clock by `d`. Timestamps taken via
/// [`now`] before the call age by `d` relative to ones taken after, so an
/// `elapsed >= TIMEOUT` guard trips without real waiting.
#[cfg(test)]
pub(crate) fn advance(d: Duration) {
    TEST_OFFSET.with(|o| o.set(o.get() + d));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_ages_prior_timestamps() {
        let t0 = now();
        advance(Duration::from_secs(30));
        let elapsed = elapsed_since(t0);
        assert!(elapsed >= Duration::from_secs(30));
        // Well under a real 31 s: proves no actual sleeping happened.
        assert!(elapsed < Duration::from_secs(31));
    }
}
