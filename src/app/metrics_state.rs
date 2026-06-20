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
