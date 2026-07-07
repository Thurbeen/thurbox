//! System/process metrics state for the info panel.
//!
//! Grouped out of the [`App`](super::App) god object. Fields are `pub(crate)`
//! so call-sites keep direct access (`self.metrics.tick_count`).

use crate::ui::info_panel;

/// Per-frame / per-tick performance counters.
///
/// These are deterministic, wall-clock-free proxies for the render and tick
/// hot paths, so the acceptance harness can assert on them without flaky
/// timing. They prove the redraw-throttling and per-frame caching optimizations
/// (a fix that "skips work" should show a counter that stops climbing). Cheap
/// `u64` bumps; never rendered, so they can't perturb snapshots.
///
/// See `docs/PERFORMANCE.md` for what each counter means and how to assert on it.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct PerfCounters {
    /// Frames actually painted (`App::view` ran). With redraw throttling this
    /// grows only when state changed or a forced redraw was due.
    pub(crate) frames_rendered: u64,
    /// Loop iterations where a redraw was requested (dirty or force-due).
    pub(crate) redraws_requested: u64,
    /// Loop iterations where the draw was skipped because nothing changed.
    pub(crate) redraws_skipped: u64,
    /// `refresh_session_statuses` passes (one per tick).
    pub(crate) status_refreshes: u64,
    /// Times the session list ordering was rebuilt (vs. served from cache).
    pub(crate) ordered_sessions_rebuilds: u64,
    /// Times the central pane locked a session's vt100 parser to render (one
    /// per terminal frame). The terminal's O(1) scrollback read happens here
    /// too — bounded by redraw throttling, not cached.
    pub(crate) parser_locks_render: u64,
    /// Times the automations-pane entry list was built (once per render of the
    /// pane; cheap for the typical handful of automations).
    pub(crate) automation_entries_built: u64,
    /// Times `refresh_session_statuses` actually reloaded the persisted hook
    /// columns from the DB (`Database::load_hook_states`). Gated on a
    /// `data_version` change (ADR-P6), so it stays flat while idle and ticks up
    /// once per external `session signal` — not once per tick.
    pub(crate) hook_state_loads: u64,
    /// Times `poll_external_changes` ran its cheap `PRAGMA data_version` check
    /// (throttled to the poll interval, not per tick).
    pub(crate) external_poll_checks: u64,
    /// Subset of `external_poll_checks` where the version actually changed and a
    /// full shared-state reload ran. The ratio of reloads to checks shows how
    /// often the poll does real work vs. spins.
    pub(crate) external_poll_reloads: u64,
}

impl PerfCounters {
    /// Field-wise wrapping difference `self - prev`, for windowed reporting
    /// (the counters stay cumulative; the perf-window log line and snapshot
    /// publish deltas so idle windows read as zeros).
    pub(crate) fn delta(&self, prev: &PerfCounters) -> PerfCounters {
        PerfCounters {
            frames_rendered: self.frames_rendered.wrapping_sub(prev.frames_rendered),
            redraws_requested: self.redraws_requested.wrapping_sub(prev.redraws_requested),
            redraws_skipped: self.redraws_skipped.wrapping_sub(prev.redraws_skipped),
            status_refreshes: self.status_refreshes.wrapping_sub(prev.status_refreshes),
            ordered_sessions_rebuilds: self
                .ordered_sessions_rebuilds
                .wrapping_sub(prev.ordered_sessions_rebuilds),
            parser_locks_render: self
                .parser_locks_render
                .wrapping_sub(prev.parser_locks_render),
            automation_entries_built: self
                .automation_entries_built
                .wrapping_sub(prev.automation_entries_built),
            hook_state_loads: self.hook_state_loads.wrapping_sub(prev.hook_state_loads),
            external_poll_checks: self
                .external_poll_checks
                .wrapping_sub(prev.external_poll_checks),
            external_poll_reloads: self
                .external_poll_reloads
                .wrapping_sub(prev.external_poll_reloads),
        }
    }
}

/// Upper bounds (µs) of the fixed histogram buckets: powers of two from 250µs
/// to ~1s, with the last bucket catching everything slower. Coarse on purpose —
/// the histogram answers "is a frame 1ms or 30ms", not microbenchmarks.
const HISTO_BUCKETS_US: [u64; 13] = [
    250, 500, 1_000, 2_000, 4_000, 8_000, 16_000, 33_000, 66_000, 100_000, 250_000, 500_000,
    1_000_000,
];

/// Fixed-bucket duration histogram for display/logging only — never asserted
/// in CI (ADR-P2: wall-clock stats are observability, deterministic counters
/// are the regression gate).
#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct DurationHistogram {
    /// One count per `HISTO_BUCKETS_US` bound, plus a final overflow bucket.
    counts: [u64; HISTO_BUCKETS_US.len() + 1],
    /// Total recorded samples.
    total: u64,
    /// Largest single sample seen (µs).
    max_us: u64,
}

impl DurationHistogram {
    pub(crate) fn record(&mut self, d: std::time::Duration) {
        let us = u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
        let idx = HISTO_BUCKETS_US
            .iter()
            .position(|&bound| us <= bound)
            .unwrap_or(HISTO_BUCKETS_US.len());
        self.counts[idx] = self.counts[idx].wrapping_add(1);
        self.total = self.total.wrapping_add(1);
        self.max_us = self.max_us.max(us);
    }

    /// Approximate percentile (0–100) as the upper bound (µs) of the bucket
    /// containing that rank; the overflow bucket reports the observed max.
    pub(crate) fn percentile_us(&self, p: u8) -> u64 {
        if self.total == 0 {
            return 0;
        }
        let rank = (self.total * u64::from(p)).div_ceil(100).max(1);
        let mut seen = 0u64;
        for (i, &count) in self.counts.iter().enumerate() {
            seen += count;
            if seen >= rank {
                return HISTO_BUCKETS_US.get(i).copied().unwrap_or(self.max_us);
            }
        }
        self.max_us
    }

    pub(crate) fn max_us(&self) -> u64 {
        self.max_us
    }

    #[cfg(test)]
    pub(crate) fn total(&self) -> u64 {
        self.total
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

/// One measured slow operation (a named synchronous UI-thread op that took
/// long enough to matter), kept in a small ring for the HUD / perf log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SlowOp {
    pub(crate) name: &'static str,
    pub(crate) ms: u64,
    /// `tick_count` when recorded, so the HUD can show recency without a clock.
    pub(crate) tick: u64,
}

/// Fixed-capacity ring of the most recent [`SlowOp`]s (newest first on read).
#[derive(Default, Clone, Debug)]
pub(crate) struct SlowOps {
    ops: std::collections::VecDeque<SlowOp>,
}

impl SlowOps {
    const CAP: usize = 16;

    pub(crate) fn push(&mut self, op: SlowOp) {
        if self.ops.len() == Self::CAP {
            self.ops.pop_front();
        }
        self.ops.push_back(op);
    }

    /// Newest-first iteration (the HUD and log line show recent ops first).
    pub(crate) fn iter_recent(&self) -> impl Iterator<Item = &SlowOp> {
        self.ops.iter().rev()
    }

    pub(crate) fn clear(&mut self) {
        self.ops.clear();
    }
}

/// Wall-clock timing stats for the render/tick hot paths plus named slow ops.
/// Populated only while timing is active (`THURBOX_PERF_LOG` or the perf HUD);
/// display/logging only, never CI-asserted (ADR-P2).
#[derive(Default)]
pub(crate) struct PerfTimings {
    /// `terminal.draw` duration per painted frame.
    pub(crate) frame: DurationHistogram,
    /// `App::tick` duration per loop iteration.
    pub(crate) tick: DurationHistogram,
    /// Recent named synchronous UI-thread operations.
    pub(crate) slow_ops: SlowOps,
}

impl PerfTimings {
    /// Clear per-window state after a perf-window report so each window's
    /// percentiles and slow-op list stand alone.
    pub(crate) fn reset_window(&mut self) {
        self.frame.reset();
        self.tick.reset();
        self.slow_ops.clear();
    }
}

/// CPU/RAM metrics collection plus the app-wide tick counter that paces the
/// periodic refreshes (metrics, git stats, usage).
pub(crate) struct MetricsState {
    /// Monotonic tick counter; drives the periodic refresh cadences.
    pub(crate) tick_count: u64,
    /// System info collector for CPU/RAM metrics.
    pub(crate) sys: Option<sysinfo::System>,
    /// Cached system metrics for the info panel.
    pub(crate) system_metrics: info_panel::SystemMetrics,
    /// Deterministic render/tick performance counters (perf regression tests).
    pub(crate) perf: PerfCounters,
    /// Wall-clock timing stats (frame/tick histograms + slow-op ring),
    /// recorded only while perf timing is active.
    pub(crate) timings: PerfTimings,
}

impl MetricsState {
    pub(crate) fn new() -> Self {
        Self {
            tick_count: 0,
            sys: Some(sysinfo::System::new()),
            system_metrics: info_panel::SystemMetrics {
                cpu_percent: 0.0,
                memory_used: 0,
                memory_total: 0,
                session_cpu_percent: 0.0,
                session_memory_bytes: 0,
            },
            perf: PerfCounters::default(),
            timings: PerfTimings::default(),
        }
    }

    /// Increment the perf counter named by `select`, e.g.
    /// `metrics.bump(|p| &mut p.frames_rendered)`. Keeps the hot-path call sites
    /// to one readable line instead of a doubled field path. Wrapping so a
    /// diagnostic tally can never panic on overflow.
    #[inline]
    pub(crate) fn bump(&mut self, select: impl FnOnce(&mut PerfCounters) -> &mut u64) {
        let counter = select(&mut self.perf);
        *counter = counter.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn histogram_percentiles_from_explicit_samples() {
        let mut h = DurationHistogram::default();
        // 9 fast frames (~1ms bucket) and 1 slow one (~33ms bucket).
        for _ in 0..9 {
            h.record(Duration::from_micros(900));
        }
        h.record(Duration::from_millis(30));
        assert_eq!(h.total(), 10);
        assert_eq!(h.percentile_us(50), 1_000);
        assert_eq!(h.percentile_us(95), 33_000);
        assert_eq!(h.max_us(), 30_000);
    }

    #[test]
    fn histogram_overflow_bucket_reports_observed_max() {
        let mut h = DurationHistogram::default();
        h.record(Duration::from_secs(3));
        assert_eq!(h.percentile_us(50), 3_000_000);
        assert_eq!(h.percentile_us(100), 3_000_000);
    }

    #[test]
    fn histogram_empty_and_reset_read_zero() {
        let mut h = DurationHistogram::default();
        assert_eq!(h.percentile_us(95), 0);
        h.record(Duration::from_millis(5));
        h.reset();
        assert_eq!(h.total(), 0);
        assert_eq!(h.percentile_us(95), 0);
        assert_eq!(h.max_us(), 0);
    }

    #[test]
    fn slow_ops_ring_drops_oldest_past_capacity() {
        let mut ring = SlowOps::default();
        for i in 0..20u64 {
            ring.push(SlowOp {
                name: "op",
                ms: i,
                tick: i,
            });
        }
        let recent: Vec<u64> = ring.iter_recent().map(|o| o.ms).collect();
        assert_eq!(recent.len(), SlowOps::CAP);
        assert_eq!(recent.first(), Some(&19), "newest first");
        assert_eq!(recent.last(), Some(&4), "oldest surviving entry");
    }

    #[test]
    fn perf_window_line_uses_counter_deltas() {
        let base = PerfCounters {
            frames_rendered: 10,
            redraws_skipped: 100,
            ..Default::default()
        };
        let now = PerfCounters {
            frames_rendered: 14,
            redraws_skipped: 350,
            ..base
        };
        let d = now.delta(&base);
        assert_eq!(d.frames_rendered, 4);
        assert_eq!(d.redraws_skipped, 250);
        assert_eq!(d.hook_state_loads, 0);
    }
}
