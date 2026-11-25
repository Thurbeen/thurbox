use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::node::{Node, SurfaceSource};
use super::runs::RunEvent;

#[derive(Default)]
pub struct Counters {
    pub iterations: AtomicU64,
    pub frames: AtomicU64,
    pub skipped: AtomicU64,
    pub renders: AtomicU64,
    pub failures: AtomicU64,
    pub reloads: AtomicU64,
    pub renders_skipped: AtomicU64,
    pub groups_reused: AtomicU64,
    pub echoes: AtomicU64,
    pub echo_frames: AtomicU64,
}

impl Counters {
    pub fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

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
            echoes: Self::get(&self.echoes),
            echo_frames: Self::get(&self.echo_frames),
        }
    }
}

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
    pub echoes: u64,
    pub echo_frames: u64,
}

impl Snapshot {
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
            echoes: self.echoes.saturating_sub(earlier.echoes),
            echo_frames: self.echo_frames.saturating_sub(earlier.echo_frames),
        }
    }
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

const HISTO_BUCKETS_US: [u64; 13] = [
    250, 500, 1_000, 2_000, 4_000, 8_000, 16_000, 33_000, 66_000, 100_000, 250_000, 500_000,
    1_000_000,
];

#[derive(Default, Clone, Copy, Debug)]
pub struct DurationHistogram {
    counts: [u64; HISTO_BUCKETS_US.len() + 1],
    total: u64,
    max_us: u64,
    sum_us: u64,
}

impl DurationHistogram {
    pub fn record(&mut self, d: Duration) {
        let us = micros(d);
        let idx = HISTO_BUCKETS_US
            .iter()
            .position(|&bound| us <= bound)
            .unwrap_or(HISTO_BUCKETS_US.len());
        self.counts[idx] = self.counts[idx].wrapping_add(1);
        self.total = self.total.wrapping_add(1);
        self.max_us = self.max_us.max(us);
        self.sum_us = self.sum_us.saturating_add(us);
    }

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

    pub fn sum_us(&self) -> u64 {
        self.sum_us
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlowOp {
    pub name: &'static str,
    pub ms: u64,
    pub owner: Option<String>,
}

pub const SLOW_OP_RING_MS: u64 = 5;
pub const SLOW_OP_WARN_MS: u64 = 100;

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

#[derive(Default)]
pub struct Timings {
    pub frame: DurationHistogram,
    pub tick: DurationHistogram,
    pub republish: DurationHistogram,
    pub slow_ops: SlowOps,
}

impl Timings {
    pub fn reset_window(&mut self) {
        self.frame.reset();
        self.tick.reset();
        self.republish.reset();
        self.slow_ops.clear();
    }

    pub fn record_op(
        &mut self,
        name: &'static str,
        elapsed: Duration,
        mod_name: Option<String>,
    ) -> bool {
        let ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        if ms >= SLOW_OP_RING_MS {
            self.slow_ops.push(SlowOp {
                name,
                ms,
                owner: mod_name,
            });
        }
        ms >= SLOW_OP_WARN_MS
    }
}

pub fn publish_snapshot(json: &str) -> std::io::Result<()> {
    let path = crate::paths::perf_snapshot_file()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no data directory"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::paths::write_atomically(&path, json)
}

#[derive(Default, Clone, Copy, Debug)]
pub struct Startup {
    pub config_init_ms: u64,
    pub snapshot_open_ms: u64,
    pub theme_activate_ms: u64,
    pub ui_build_ms: u64,
    pub first_frame_ms: u64,
}

impl Startup {
    pub fn to_json(self) -> Value {
        json!({
            "config_init_ms": self.config_init_ms,
            "snapshot_open_ms": self.snapshot_open_ms,
            "theme_activate_ms": self.theme_activate_ms,
            "ui_build_ms": self.ui_build_ms,
            "first_frame_ms": self.first_frame_ms,
        })
    }
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct HookStat {
    pub calls: u64,
    pub total_us: u64,
    pub max_us: u64,
}

impl HookStat {
    pub fn record(&mut self, took: Duration) {
        let us = micros(took);
        self.calls += 1;
        self.total_us = self.total_us.saturating_add(us);
        self.max_us = self.max_us.max(us);
    }

    fn to_json(self) -> Value {
        json!({ "calls": self.calls, "total_us": self.total_us, "max_us": self.max_us })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hook {
    Key,
    Action,
    Click,
    Scroll,
    Event,
    Decorate,
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hooks {
    pub key: HookStat,
    pub action: HookStat,
    pub click: HookStat,
    pub scroll: HookStat,
    pub event: HookStat,
    pub decorate: HookStat,
}

impl Hooks {
    pub fn stat_mut(&mut self, hook: Hook) -> &mut HookStat {
        match hook {
            Hook::Key => &mut self.key,
            Hook::Action => &mut self.action,
            Hook::Click => &mut self.click,
            Hook::Scroll => &mut self.scroll,
            Hook::Event => &mut self.event,
            Hook::Decorate => &mut self.decorate,
        }
    }

    pub fn total_us(&self) -> u64 {
        [
            self.key,
            self.action,
            self.click,
            self.scroll,
            self.event,
            self.decorate,
        ]
        .iter()
        .fold(0u64, |sum, stat| sum.saturating_add(stat.total_us))
    }

    fn to_json(self) -> Value {
        json!({
            "on_key": self.key.to_json(),
            "on_action": self.action.to_json(),
            "on_click": self.click.to_json(),
            "on_scroll": self.scroll.to_json(),
            "on_event": self.event.to_json(),
            "decorate": self.decorate.to_json(),
        })
    }
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunTiming {
    pub started: u64,
    pub finished: u64,
    pub total_us: u64,
    pub max_us: u64,
}

impl RunTiming {
    pub fn record(&mut self, took: Duration) {
        let us = micros(took);
        self.finished += 1;
        self.total_us = self.total_us.saturating_add(us);
        self.max_us = self.max_us.max(us);
    }
}

#[derive(Default, Clone, Debug)]
pub struct ModStats {
    pub renders: u64,
    pub reused: u64,
    pub failures: u64,
    pub render: DurationHistogram,
    pub hooks: Hooks,
    pub asked: u64,
    pub store_writes: u64,
    pub store_table_writes: u64,
    pub last_nodes: u64,
    pub last_spans: u64,
    pub max_nodes: u64,
    pub idle_renders: u64,
    pub closed_renders: u64,
    pub runs: RunTiming,
}

impl ModStats {
    pub fn note_tree(&mut self, node: &Node) {
        let (nodes, spans) = tree_size(node);
        self.last_nodes = nodes;
        self.last_spans = spans;
        self.max_nodes = self.max_nodes.max(nodes);
    }

    pub fn total_us(&self) -> u64 {
        self.render.sum_us().saturating_add(self.hooks.total_us())
    }
}

pub fn tree_size(node: &Node) -> (u64, u64) {
    let runs = |lines: &[Vec<super::node::Run>]| lines.iter().map(|l| l.len() as u64).sum();
    match node {
        Node::Text { lines, .. } => (1, runs(lines)),
        Node::Box { children, .. } => children.iter().fold((1, 0), |(n, s), child| {
            let (cn, cs) = tree_size(child);
            (n + cn, s + cs)
        }),
        Node::Input { .. } => (1, 0),
        Node::Surface {
            source: SurfaceSource::Cells(lines),
            ..
        } => (1, runs(lines)),
        Node::Surface { .. } => (1, 0),
    }
}

pub fn fit_columns(name: &str, cols: usize) -> String {
    let cut = super::host::take_left(name, cols);
    let pad = cols.saturating_sub(super::host::columns(cut));
    format!("{cut}{}", " ".repeat(pad))
}

pub fn text_columns(s: &str) -> usize {
    super::host::columns(s)
}

#[derive(Default, Debug)]
pub struct ModTable {
    pub frames: u64,
    pub frame_total_us: u64,
    stats: HashMap<String, ModStats>,
    opened_at: Option<Instant>,
}

impl ModTable {
    pub fn stats_mut(&mut self, path: &str) -> &mut ModStats {
        self.stats.entry(path.to_string()).or_default()
    }

    pub fn get(&self, path: &str) -> Option<&ModStats> {
        self.stats.get(path)
    }

    pub fn note_frame(&mut self, took: Duration) {
        self.frames += 1;
        self.frame_total_us = self.frame_total_us.saturating_add(micros(took));
    }

    pub fn note_run(&mut self, event: &RunEvent) {
        let opened = self.opened_at;
        let inside = |at: Instant| opened.is_none_or(|opened| at >= opened);
        match event {
            RunEvent::Started { owner, at } if inside(*at) => {
                self.stats_mut(owner).runs.started += 1;
            }
            RunEvent::Finished {
                owner,
                started,
                took,
            } if inside(*started) => {
                self.stats_mut(owner).runs.record(*took);
            }
            _ => {}
        }
    }

    pub fn clear(&mut self) {
        *self = Self {
            opened_at: Some(Instant::now()),
            ..Self::default()
        };
    }
}

pub const HINT_MIN_FRAMES: u64 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint {
    NotPure,
    ClosedFloat,
    StoreTables,
    RenderWrites,
    IdleRenders,
}

impl Hint {
    pub fn id(self) -> &'static str {
        match self {
            Hint::NotPure => "not_pure",
            Hint::ClosedFloat => "closed_float",
            Hint::StoreTables => "store_tables",
            Hint::RenderWrites => "render_writes",
            Hint::IdleRenders => "idle_renders",
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Hint::NotPure => {
                "not pure and renders every frame: declare pure = true if render only reads"
            }
            Hint::ClosedFloat => "float renders every frame while closed: declare pure = true",
            Hint::StoreTables => {
                "writes a fresh table to store while rendering: move it into a handler"
            }
            Hint::RenderWrites => {
                "writes store/state while rendering: every pure pane's cache is dropped"
            }
            Hint::IdleRenders => "pure but re-renders while idle: something it reads keeps moving",
        }
    }
}

pub struct ModMeta<'a> {
    pub name: &'a str,
    pub path: &'a str,
    pub pure: bool,
    pub floats: bool,
}

#[derive(Clone, Debug)]
pub struct ModRow {
    pub name: String,
    pub file: String,
    pub pure: bool,
    pub floats: bool,
    pub stats: ModStats,
    pub total_us: u64,
    pub frame_share: f64,
    pub hints: Vec<Hint>,
}

impl ModRow {
    fn hints(meta: &ModMeta, stats: &ModStats, frames: u64) -> Vec<Hint> {
        let every_frame = |n: u64| frames >= HINT_MIN_FRAMES && n * 10 >= frames * 9;
        let often = |n: u64| stats.renders >= HINT_MIN_FRAMES && n * 2 >= stats.renders;
        let mut hints = Vec::new();
        if meta.floats && every_frame(stats.closed_renders) {
            hints.push(Hint::ClosedFloat);
        } else if !meta.pure && every_frame(stats.renders) {
            hints.push(Hint::NotPure);
        }
        if often(stats.store_table_writes) {
            hints.push(Hint::StoreTables);
        } else if often(stats.store_writes) {
            hints.push(Hint::RenderWrites);
        }
        // An impure pane renders on every paint by definition, which the first
        // hint already says; only a pure one re-rendering is a surprise.
        if meta.pure && stats.idle_renders >= HINT_MIN_FRAMES {
            hints.push(Hint::IdleRenders);
        }
        hints
    }

    pub fn to_json(&self) -> Value {
        let s = &self.stats;
        let per_render = if s.renders == 0 {
            0.0
        } else {
            (s.store_writes as f64 / s.renders as f64 * 100.0).round() / 100.0
        };
        json!({
            "name": self.name,
            "file": self.file,
            "pure": self.pure,
            "floats": self.floats,
            "total_us": self.total_us,
            "frame_share": (self.frame_share * 1000.0).round() / 1000.0,
            "renders": s.renders,
            "reused": s.reused,
            "failures": s.failures,
            "render": {
                "p50_us": s.render.percentile_us(50),
                "p95_us": s.render.percentile_us(95),
                "max_us": s.render.max_us(),
                "samples": s.render.total(),
                "total_us": s.render.sum_us(),
            },
            "handlers": s.hooks.to_json(),
            "runs": {
                "asked": s.asked,
                "started": s.runs.started,
                "finished": s.runs.finished,
                "total_us": s.runs.total_us,
                "max_us": s.runs.max_us,
            },
            "store_writes": s.store_writes,
            "store_table_writes": s.store_table_writes,
            "store_writes_per_render": per_render,
            "tree": { "nodes": s.last_nodes, "spans": s.last_spans, "max_nodes": s.max_nodes },
            "idle_renders": s.idle_renders,
            "closed_renders": s.closed_renders,
            "hints": self.hints.iter().map(|h| json!({ "id": h.id(), "text": h.text() })).collect::<Vec<_>>(),
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct ModReport {
    pub frames: u64,
    pub frame_total_us: u64,
    pub rows: Vec<ModRow>,
}

impl ModReport {
    pub fn summary(&self, top: usize) -> String {
        self.rows
            .iter()
            .take(top)
            .filter(|row| row.total_us > 0)
            .map(|row| {
                let hints: Vec<&str> = row.hints.iter().map(|h| h.id()).collect();
                format!(
                    "{}:{}us:{}r/{}c{}{}",
                    row.name,
                    row.total_us,
                    row.stats.renders,
                    row.stats.reused,
                    if hints.is_empty() { "" } else { ":" },
                    hints.join("+"),
                )
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

pub fn mod_report<'a>(table: &ModTable, mods: impl IntoIterator<Item = ModMeta<'a>>) -> ModReport {
    let empty = ModStats::default();
    let mut rows: Vec<ModRow> = mods
        .into_iter()
        .map(|meta| {
            let stats = table.get(meta.path).unwrap_or(&empty);
            let frame_share = if table.frame_total_us == 0 {
                0.0
            } else {
                (stats.render.sum_us() as f64 / table.frame_total_us as f64).min(1.0)
            };
            ModRow {
                name: meta.name.to_string(),
                file: meta.path.to_string(),
                pure: meta.pure,
                floats: meta.floats,
                total_us: stats.total_us(),
                frame_share,
                hints: ModRow::hints(&meta, stats, table.frames),
                stats: stats.clone(),
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.total_us
            .cmp(&a.total_us)
            .then(b.stats.renders.cmp(&a.stats.renders))
            .then_with(|| a.name.cmp(&b.name))
    });
    ModReport {
        frames: table.frames,
        frame_total_us: table.frame_total_us,
        rows,
    }
}

fn histogram_json(h: &DurationHistogram) -> Value {
    json!({
        "p50_us": h.percentile_us(50),
        "p95_us": h.percentile_us(95),
        "max_us": h.max_us(),
        "samples": h.total(),
    })
}

pub fn snapshot_json(
    counters: &Snapshot,
    timings: &Timings,
    startup: &Startup,
    session_count: usize,
    mods: &ModReport,
) -> Value {
    let captured_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    json!({
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
            "echoes": counters.echoes,
            "echo_frames": counters.echo_frames,
        },
        "frame": histogram_json(&timings.frame),
        "tick": histogram_json(&timings.tick),
        "republish": histogram_json(&timings.republish),
        "slow_ops": timings
            .slow_ops
            .iter_recent()
            .map(|op| json!({ "op": op.name, "ms": op.ms, "plugin": op.owner }))
            .collect::<Vec<_>>(),
        "startup": startup.to_json(),
        "plugin_window": { "frames": mods.frames, "frame_total_us": mods.frame_total_us },
        "plugins": mods.rows.iter().map(ModRow::to_json).collect::<Vec<_>>(),
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
        assert_eq!(h.sum_us(), 0);
    }

    #[test]
    fn a_percentile_never_reads_above_the_max_beside_it() {
        // The bucket's UPPER bound is reported, so without the clamp a single
        // 300µs sample would print "p95 500µs / max 300µs" — a contradiction
        // in two adjacent columns.
        let mut h = DurationHistogram::default();
        h.record(Duration::from_micros(300));
        assert_eq!(h.max_us(), 300);
        assert!(h.percentile_us(95) <= h.max_us());
        assert!(h.percentile_us(50) <= h.max_us());
    }

    #[test]
    fn an_overflow_sample_is_counted_and_reported_as_the_max() {
        let mut h = DurationHistogram::default();
        h.record(Duration::from_secs(3));
        assert_eq!(h.total(), 1);
        assert_eq!(h.max_us(), 3_000_000);
        assert_eq!(h.percentile_us(95), 3_000_000);
    }

    #[test]
    fn the_slow_op_ring_keeps_the_newest_and_reads_newest_first() {
        let mut ops = SlowOps::default();
        for i in 0..(SlowOps::CAP as u64 + 4) {
            ops.push(SlowOp {
                name: "op",
                ms: i,
                owner: None,
            });
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
        assert!(!t.record_op("fast", Duration::from_micros(200), None));
        assert!(t.slow_ops.is_empty(), "a fast op is not worth a ring slot");

        assert!(!t.record_op("middling", Duration::from_millis(SLOW_OP_RING_MS), None));
        assert!(!t.slow_ops.is_empty());

        assert!(
            t.record_op("stall", Duration::from_millis(SLOW_OP_WARN_MS), None),
            "an op over the warn threshold asks the caller to log it"
        );
    }

    #[test]
    fn a_window_reset_clears_timings_but_not_the_counters() {
        let mut t = Timings::default();
        t.frame.record(Duration::from_millis(5));
        t.tick.record(Duration::from_millis(1));
        t.republish.record(Duration::from_millis(2));
        t.record_op("op", Duration::from_millis(50), None);
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
        timings.frame.record(Duration::from_millis(6));
        timings.record_op(
            "interface_reload",
            Duration::from_millis(120),
            Some("sessions".to_string()),
        );
        let startup = Startup {
            config_init_ms: 3,
            ui_build_ms: 40,
            ..Startup::default()
        };

        let json = snapshot_json(
            &counters.read(),
            &timings,
            &startup,
            4,
            &ModReport::default(),
        );

        assert_eq!(json["session_count"], 4);
        assert_eq!(json["counters"]["frames"], 1);
        assert_eq!(json["startup"]["ui_build_ms"], 40);
        assert_eq!(json["slow_ops"][0]["op"], "interface_reload");
        assert_eq!(json["slow_ops"][0]["ms"], 120);
        assert_eq!(json["slow_ops"][0]["plugin"], "sessions");
        assert!(json["plugins"].is_array());
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

    fn meta<'a>(name: &'a str, pure: bool, floats: bool) -> ModMeta<'a> {
        ModMeta {
            name,
            path: name,
            pure,
            floats,
        }
    }

    #[test]
    fn a_hint_needs_enough_frames_to_mean_anything() {
        // Three impure renders in three frames is every frame, and says nothing.
        let mut table = ModTable::default();
        for _ in 0..3 {
            table.note_frame(Duration::from_millis(1));
            table.stats_mut("a").renders += 1;
        }
        let report = mod_report(&table, [meta("a", false, false)]);
        assert!(report.rows[0].hints.is_empty(), "{:?}", report.rows[0]);
    }

    #[test]
    fn an_unmeasured_mod_still_gets_a_row_and_ranks_last() {
        let mut table = ModTable::default();
        table
            .stats_mut("busy")
            .render
            .record(Duration::from_millis(4));
        let report = mod_report(
            &table,
            [meta("idle", true, false), meta("busy", true, false)],
        );
        let names: Vec<&str> = report.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["busy", "idle"]);
        assert_eq!(report.rows[1].total_us, 0);
    }

    #[test]
    fn tree_size_counts_nodes_and_runs_through_boxes() {
        let text = |runs: usize| Node::Text {
            lines: vec![vec![super::super::node::Run::plain("x"); runs]],
            align: Default::default(),
            wrap: false,
            scroll: 0,
            style: Default::default(),
            frame: None,
            size: Default::default(),
            identity: Default::default(),
        };
        let tree = Node::Box {
            axis: super::super::node::Axis::Vertical,
            gap: 0,
            children: vec![text(2), text(3)],
            frame: None,
            size: Default::default(),
            identity: Default::default(),
        };
        assert_eq!(tree_size(&tree), (3, 5));
    }

    #[test]
    fn fit_columns_pads_and_cuts_by_terminal_columns() {
        assert_eq!(fit_columns("abc", 5), "abc  ");
        assert_eq!(fit_columns("abcdef", 3), "abc");
        // A double-width glyph that would straddle the edge is left out and the
        // gap padded, so the cell is still exactly three columns.
        assert_eq!(fit_columns("名前", 3), "名 ");
        assert_eq!(text_columns(&fit_columns("名前ペイン", 7)), 7);
    }
}
