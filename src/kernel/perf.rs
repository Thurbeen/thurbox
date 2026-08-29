//! Wall-clock-free counters for the render loop.
//!
//! v1 learned to assert on counters rather than timings: a test that says
//! "idle should be fast" is flaky on shared hardware, while one that says "an
//! idle loop painted no frames" is exact. These are the v2 equivalents of
//! `MetricsState::perf`, and they exist so the demand-driven redraw is
//! provable rather than hoped for.
//!
//! On top of them sits the **observability layer** of ADR-P11: fixed-bucket
//! duration histograms, a ring of named slow operations, and the startup phase
//! breakdown. Those are wall-clock, so they are display and logging only and
//! are never CI-asserted — the counters above remain the sole regression gate.
//! They are populated only while timing is active (`THURBOX_PERF_LOG` or an
//! open perf HUD), so a default run pays one cached bool per iteration.

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
    /// Pane renders served from a pure pane's tree cache instead of run.
    pub renders_skipped: AtomicU64,
    /// Published `thurbox.*` groups reused instead of rebuilt.
    pub groups_reused: AtomicU64,
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
            renders_skipped: Self::get(&self.renders_skipped),
            groups_reused: Self::get(&self.groups_reused),
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
    pub renders_skipped: u64,
    pub groups_reused: u64,
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
            renders_skipped: self.renders_skipped.saturating_sub(earlier.renders_skipped),
            groups_reused: self.groups_reused.saturating_sub(earlier.groups_reused),
        }
    }
}

/// Upper bounds (µs) of the fixed histogram buckets: powers of two from 250µs
/// to ~1s, with a final bucket catching everything slower. Coarse on purpose —
/// this answers "is a frame 1ms or 30ms", not microbenchmarks.
const HISTO_BUCKETS_US: [u64; 13] = [
    250, 500, 1_000, 2_000, 4_000, 8_000, 16_000, 33_000, 66_000, 100_000, 250_000, 500_000,
    1_000_000,
];

/// Fixed-bucket duration histogram — display/logging only, never asserted in
/// CI (ADR-P2: wall-clock stats are observability, counters are the gate).
#[derive(Default, Clone, Copy, Debug)]
pub struct DurationHistogram {
    /// One count per [`HISTO_BUCKETS_US`] bound, plus a final overflow bucket.
    counts: [u64; HISTO_BUCKETS_US.len() + 1],
    total: u64,
    max_us: u64,
}

impl DurationHistogram {
    pub fn record(&mut self, d: std::time::Duration) {
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
    /// holding that rank, clamped to the observed max so a high percentile can
    /// never read above the max printed beside it.
    pub fn percentile_us(&self, p: u8) -> u64 {
        if self.total == 0 {
            return 0;
        }
        let rank = (self.total * u64::from(p)).div_ceil(100).max(1);
        let mut seen = 0u64;
        for (i, &count) in self.counts.iter().enumerate() {
            seen += count;
            if seen >= rank {
                return HISTO_BUCKETS_US
                    .get(i)
                    .copied()
                    .unwrap_or(self.max_us)
                    .min(self.max_us);
            }
        }
        self.max_us
    }

    pub fn max_us(&self) -> u64 {
        self.max_us
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// One measured slow operation: a named synchronous UI-thread op that took
/// long enough to be felt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlowOp {
    pub name: &'static str,
    pub ms: u64,
}

/// A sample lands in the ring at this threshold.
pub const SLOW_OP_RING_MS: u64 = 5;
/// And is additionally logged as a warning at this one, so an interactive
/// stall is attributable even when nobody was watching a HUD.
pub const SLOW_OP_WARN_MS: u64 = 100;

/// Fixed-capacity ring of the most recent [`SlowOp`]s (newest first on read).
#[derive(Default, Clone, Debug)]
pub struct SlowOps {
    ops: std::collections::VecDeque<SlowOp>,
}

impl SlowOps {
    const CAP: usize = 16;

    pub fn push(&mut self, op: SlowOp) {
        if self.ops.len() == Self::CAP {
            self.ops.pop_front();
        }
        self.ops.push_back(op);
    }

    /// Newest first — the HUD, the log line and the JSON all show recent ops
    /// first.
    pub fn iter_recent(&self) -> impl Iterator<Item = &SlowOp> {
        self.ops.iter().rev()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn clear(&mut self) {
        self.ops.clear();
    }
}

/// Wall-clock stats for the hot paths, plus the slow-op ring.
///
/// `republish` has a histogram of its own because it is the one per-frame cost
/// that is neither the draw nor the rest of the tick: it rebuilds every
/// `thurbox.*` table, so telling it apart from painting is the difference
/// between "frames are expensive" and knowing why.
#[derive(Default)]
pub struct Timings {
    /// `terminal.draw` duration per painted frame.
    pub frame: DurationHistogram,
    /// One whole loop iteration.
    pub tick: DurationHistogram,
    /// `App::republish` — the per-frame rebuild of the published tables.
    pub republish: DurationHistogram,
    /// Recent named synchronous UI-thread operations.
    pub slow_ops: SlowOps,
}

impl Timings {
    /// Clear per-window state after a report, so each window stands alone.
    pub fn reset_window(&mut self) {
        self.frame.reset();
        self.tick.reset();
        self.republish.reset();
        self.slow_ops.clear();
    }

    /// Record a named op, returning whether it crossed the warn threshold so
    /// the caller can log it (this module does no logging of its own).
    pub fn record_op(&mut self, name: &'static str, elapsed: std::time::Duration) -> bool {
        let ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        if ms >= SLOW_OP_RING_MS {
            self.slow_ops.push(SlowOp { name, ms });
        }
        ms >= SLOW_OP_WARN_MS
    }
}

/// How long each startup phase took, and when the first frame landed.
///
/// The phases are v2's own, not v1's: there is no `restore_ms` because v2 has
/// no synchronous restore phase, and there is a `ui_build_ms` because building
/// the Lua interface is a startup cost v1 did not have.
#[derive(Default, Clone, Copy, Debug)]
pub struct Startup {
    pub config_init_ms: u64,
    pub db_open_ms: u64,
    pub theme_activate_ms: u64,
    pub extension_heal_ms: u64,
    pub heartbeat_ms: u64,
    pub ui_build_ms: u64,
    /// Process start to first painted frame; filled in by the loop.
    pub first_frame_ms: u64,
}

impl Startup {
    pub fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "config_init_ms": self.config_init_ms,
            "db_open_ms": self.db_open_ms,
            "theme_activate_ms": self.theme_activate_ms,
            "extension_heal_ms": self.extension_heal_ms,
            "heartbeat_ms": self.heartbeat_ms,
            "ui_build_ms": self.ui_build_ms,
            "first_frame_ms": self.first_frame_ms,
        })
    }
}

/// One histogram as the `{p50_us, p95_us, max_us, samples}` object the CLI and
/// the HUD both read.
fn histogram_json(h: &DurationHistogram) -> serde_json::Value {
    serde_json::json!({
        "p50_us": h.percentile_us(50),
        "p95_us": h.percentile_us(95),
        "max_us": h.max_us(),
        "samples": h.total(),
    })
}

/// The JSON `thurbox-cli perf` reads, published into the `metadata` table while
/// timing is active.
///
/// Built here rather than in the loop so the shape has one owner: the CLI
/// renders whatever this produces, and `tests/kernel_perf.rs` pins the keys.
pub fn snapshot_json(
    counters: &Snapshot,
    timings: &Timings,
    startup: &Startup,
    session_count: usize,
) -> serde_json::Value {
    let captured_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    serde_json::json!({
        "pid": std::process::id(),
        "captured_at": captured_at,
        "session_count": session_count,
        "tick_count": counters.iterations,
        "counters": {
            "iterations": counters.iterations,
            "frames": counters.frames,
            "skipped": counters.skipped,
            "renders": counters.renders,
            "failures": counters.failures,
            "reloads": counters.reloads,
            "renders_skipped": counters.renders_skipped,
            "groups_reused": counters.groups_reused,
        },
        "frame": histogram_json(&timings.frame),
        "tick": histogram_json(&timings.tick),
        "republish": histogram_json(&timings.republish),
        "slow_ops": timings
            .slow_ops
            .iter_recent()
            .map(|op| serde_json::json!({ "op": op.name, "ms": op.ms }))
            .collect::<Vec<_>>(),
        "startup": startup.to_json(),
    })
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
    fn an_empty_histogram_reports_zero_rather_than_a_bucket_bound() {
        // The percentile of nothing is not "250µs", which is what returning the
        // first bucket bound would print into a HUD on a run that painted
        // nothing.
        let h = DurationHistogram::default();
        assert_eq!(h.percentile_us(50), 0);
        assert_eq!(h.percentile_us(95), 0);
        assert_eq!(h.max_us(), 0);
        assert_eq!(h.total(), 0);
    }

    #[test]
    fn a_percentile_never_reads_above_the_max_beside_it() {
        // The bucket's UPPER bound is reported, so without the clamp a single
        // 300µs sample would print "p95 500µs / max 300µs" — a contradiction
        // in two adjacent columns.
        let mut h = DurationHistogram::default();
        h.record(std::time::Duration::from_micros(300));
        assert_eq!(h.max_us(), 300);
        assert!(h.percentile_us(95) <= h.max_us());
        assert!(h.percentile_us(50) <= h.max_us());
    }

    #[test]
    fn an_overflow_sample_is_counted_and_reported_as_the_max() {
        let mut h = DurationHistogram::default();
        h.record(std::time::Duration::from_secs(3));
        assert_eq!(h.total(), 1);
        assert_eq!(h.max_us(), 3_000_000);
        assert_eq!(h.percentile_us(95), 3_000_000);
    }

    #[test]
    fn the_slow_op_ring_keeps_the_newest_and_reads_newest_first() {
        let mut ops = SlowOps::default();
        for i in 0..(SlowOps::CAP as u64 + 4) {
            ops.push(SlowOp { name: "op", ms: i });
        }
        let seen: Vec<u64> = ops.iter_recent().map(|op| op.ms).collect();
        assert_eq!(seen.len(), SlowOps::CAP);
        // Newest first, and the four oldest were dropped rather than the newest.
        assert_eq!(seen[0], SlowOps::CAP as u64 + 3);
        assert!(!seen.contains(&0));
    }

    #[test]
    fn only_ops_over_the_ring_threshold_are_kept_and_only_slow_ones_warn() {
        let mut t = Timings::default();
        assert!(!t.record_op("fast", std::time::Duration::from_micros(200)));
        assert!(t.slow_ops.is_empty(), "a fast op is not worth a ring slot");

        assert!(!t.record_op(
            "middling",
            std::time::Duration::from_millis(SLOW_OP_RING_MS)
        ));
        assert!(!t.slow_ops.is_empty());

        assert!(
            t.record_op("stall", std::time::Duration::from_millis(SLOW_OP_WARN_MS)),
            "an op over the warn threshold asks the caller to log it"
        );
    }

    #[test]
    fn a_window_reset_clears_timings_but_not_the_counters() {
        let mut t = Timings::default();
        t.frame.record(std::time::Duration::from_millis(5));
        t.tick.record(std::time::Duration::from_millis(1));
        t.republish.record(std::time::Duration::from_millis(2));
        t.record_op("op", std::time::Duration::from_millis(50));
        t.reset_window();
        assert_eq!(t.frame.total(), 0);
        assert_eq!(t.tick.total(), 0);
        assert_eq!(t.republish.total(), 0);
        assert!(t.slow_ops.is_empty());
    }

    #[test]
    fn the_published_snapshot_carries_every_key_the_cli_renders() {
        let counters = Counters::default();
        Counters::bump(&counters.frames);
        let mut timings = Timings::default();
        timings.frame.record(std::time::Duration::from_millis(6));
        timings.record_op("interface_reload", std::time::Duration::from_millis(120));
        let startup = Startup {
            config_init_ms: 3,
            ui_build_ms: 40,
            ..Startup::default()
        };

        let json = snapshot_json(&counters.read(), &timings, &startup, 4);

        assert_eq!(json["session_count"], 4);
        assert_eq!(json["counters"]["frames"], 1);
        assert_eq!(json["startup"]["ui_build_ms"], 40);
        assert_eq!(json["slow_ops"][0]["op"], "interface_reload");
        assert_eq!(json["slow_ops"][0]["ms"], 120);
        // Each histogram is present even when it recorded nothing, so the CLI
        // never has to distinguish "absent" from "no samples".
        for key in ["frame", "tick", "republish"] {
            assert!(json[key]["p50_us"].is_number(), "{key} p50 missing");
            assert!(json[key]["max_us"].is_number(), "{key} max missing");
            assert!(json[key]["samples"].is_number(), "{key} samples missing");
        }
        assert!(json["pid"].as_u64().is_some_and(|p| p > 0));
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
