//! System/process metrics state for the info panel.
//!
//! Grouped out of the [`App`](super::App) god object. Fields are `pub(crate)`
//! so call-sites keep direct access (`self.metrics.tick_count`).

use crate::ui::info_panel;

/// CPU/RAM metrics collection plus the app-wide tick counter that paces the
/// periodic refreshes (metrics, git stats, usage).
pub(crate) struct MetricsState {
    /// Monotonic tick counter; drives the periodic refresh cadences.
    pub(crate) tick_count: u64,
    /// System info collector for CPU/RAM metrics.
    pub(crate) sys: sysinfo::System,
    /// Cached system metrics for the info panel.
    pub(crate) system_metrics: info_panel::SystemMetrics,
}

impl MetricsState {
    pub(crate) fn new() -> Self {
        Self {
            tick_count: 0,
            sys: sysinfo::System::new(),
            system_metrics: info_panel::SystemMetrics {
                cpu_percent: 0.0,
                memory_used: 0,
                memory_total: 0,
                session_cpu_percent: 0.0,
                session_memory_bytes: 0,
            },
        }
    }
}
