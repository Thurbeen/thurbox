//! `thurbox-cli perf` — read the perf snapshot a running TUI publishes.
//!
//! The TUI writes a JSON snapshot (counters, frame/republish/tick timing
//! percentiles, slow ops, startup phase breakdown, and the per-plugin report)
//! into the SQLite `metadata` table while perf timing is active —
//! `THURBOX_PERF_LOG=1` or an open perf HUD (F12). `kernel::perf::snapshot_json`
//! owns the shape; this renders it. This command prints the latest one, so a
//! running instance can be inspected from outside without tailing
//! `thurbox.log`. `--plugins` prints the per-pane half. See
//! `docs/PERFORMANCE.md`.

use serde_json::{json, Value};

use crate::storage::Database;

use super::output::{kv, CommandOutput};

#[derive(clap::Args, Debug)]
pub struct PerfArgs {
    /// One row per loaded plugin instead of the loop's totals: render and
    /// handler time, reuse, runs, store writes and tree size, most expensive
    /// first, with hints naming the likely cause.
    #[arg(long)]
    pub plugins: bool,
}

/// Human hint shown when no snapshot exists (or it can't be parsed).
const NO_SNAPSHOT_HINT: &str = "No perf snapshot published. Run the TUI with THURBOX_PERF_LOG=1 \
     or open its perf HUD (F12), then retry.";

/// Shown for a snapshot published by a thurbox that predates `--plugins`.
const NO_PLUGINS_HINT: &str = "This perf snapshot has no per-plugin section: the running TUI \
     is older than `perf --plugins`.";

/// Run the `perf` command: print the last published snapshot.
pub fn run(db: &Database, plugins: bool) -> Result<CommandOutput, String> {
    let raw = db
        .get_perf_snapshot()
        .map_err(|e| format!("failed to read perf snapshot: {e}"))?;
    let Some(raw) = raw else {
        return Ok(CommandOutput::failed(
            json!({ "snapshot": null }),
            NO_SNAPSHOT_HINT,
            "no perf snapshot",
        ));
    };
    let snapshot: Value =
        serde_json::from_str(&raw).map_err(|e| format!("perf snapshot is not valid JSON: {e}"))?;
    if plugins {
        return Ok(render_plugins(&snapshot));
    }
    let human = render_human(&snapshot);
    Ok(CommandOutput::new(snapshot, human))
}

fn u(v: &Value, path: &[&str]) -> u64 {
    let mut cur = v;
    for p in path {
        cur = &cur[*p];
    }
    cur.as_u64().unwrap_or(0)
}

/// Format a µs value compactly (mirrors the HUD's formatting).
fn fmt_us(us: u64) -> String {
    if us < 1_000 {
        format!("{us}µs")
    } else if us < 1_000_000 {
        format!("{:.1}ms", us as f64 / 1_000.0)
    } else {
        format!("{:.1}s", us as f64 / 1_000_000.0)
    }
}

/// Seconds since the snapshot was captured.
fn age(s: &Value) -> u64 {
    let captured_at = u(s, &["captured_at"]);
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().saturating_sub(captured_at))
        .unwrap_or(0)
}

fn render_human(s: &Value) -> String {
    // One row per histogram, so the three costs of a frame stay side by side:
    // painting, the table rebuild that precedes it, and the rest of the loop.
    let histogram = |key: &str| {
        format!(
            "{} / {} / {}  ({} samples)",
            fmt_us(u(s, &[key, "p50_us"])),
            fmt_us(u(s, &[key, "p95_us"])),
            fmt_us(u(s, &[key, "max_us"])),
            u(s, &[key, "samples"]),
        )
    };

    let mut pairs: Vec<(&str, String)> = vec![
        (
            "captured",
            format!("{}s ago (pid {})", age(s), u(s, &["pid"])),
        ),
        ("sessions", u(s, &["session_count"]).to_string()),
        ("iterations", u(s, &["counters", "iterations"]).to_string()),
        ("frames", u(s, &["counters", "frames"]).to_string()),
        ("idle skips", u(s, &["counters", "skipped"]).to_string()),
        ("plugin renders", u(s, &["counters", "renders"]).to_string()),
        (
            "plugin failures",
            u(s, &["counters", "failures"]).to_string(),
        ),
        ("reloads", u(s, &["counters", "reloads"]).to_string()),
        (
            "renders skipped",
            u(s, &["counters", "renders_skipped"]).to_string(),
        ),
        (
            "groups reused",
            u(s, &["counters", "groups_reused"]).to_string(),
        ),
        ("frame p50/p95/max", histogram("frame")),
        ("republish p50/p95/max", histogram("republish")),
        ("tick p50/p95/max", histogram("tick")),
    ];

    if let Some(startup) = s.get("startup").filter(|v| v.is_object()) {
        pairs.push((
            "startup",
            format!(
                "config {}ms · db {}ms · heal {}ms · ui {}ms · first frame {}ms",
                u(startup, &["config_init_ms"]),
                u(startup, &["db_open_ms"]),
                u(startup, &["extension_heal_ms"]),
                u(startup, &["ui_build_ms"]),
                u(startup, &["first_frame_ms"]),
            ),
        ));
    }

    let mut out = kv(&pairs);
    match s.get("slow_ops").and_then(Value::as_array) {
        Some(ops) if !ops.is_empty() => {
            out.push_str("\n\nslow ops (recent first):");
            for op in ops {
                out.push_str(&format!(
                    "\n  {:<20} {:>6}ms  {}",
                    op["op"].as_str().unwrap_or("?"),
                    u(op, &["ms"]),
                    op["plugin"].as_str().unwrap_or(""),
                ));
            }
        }
        _ => out.push_str("\n\nslow ops: none recorded"),
    }
    out.push_str("\n\nper pane: thurbox-cli perf --plugins");
    out
}

/// The per-plugin table, in the order the snapshot ranked it.
fn render_plugins(s: &Value) -> CommandOutput {
    let Some(rows) = s.get("plugins").and_then(Value::as_array) else {
        return CommandOutput::failed(
            json!({ "plugins": null }),
            NO_PLUGINS_HINT,
            "no per-plugin data",
        );
    };
    let json = json!({
        "pid": s["pid"],
        "captured_at": s["captured_at"],
        "frames": u(s, &["plugin_window", "frames"]),
        "frame_total_us": u(s, &["plugin_window", "frame_total_us"]),
        "plugins": rows,
    });

    let mut out = format!(
        "captured {}s ago (pid {}) · {} frames measured",
        age(s),
        u(s, &["pid"]),
        u(s, &["plugin_window", "frames"]),
    );
    if rows.is_empty() {
        out.push_str("\n\nno plugins loaded");
        return CommandOutput::new(json, out);
    }
    let name_of = |row: &Value| row["name"].as_str().unwrap_or("?").to_string();
    let width = rows
        .iter()
        .map(|row| crate::kernel::perf::text_columns(&name_of(row)))
        .max()
        .unwrap_or(4)
        .clamp(4, 24);
    out.push_str(&format!(
        "\n\n{:<width$}  {:>8} {:>8} {:>8} {:>5} {:>7} {:>6} {:>8} {:>5} {:>8} {:>5} {:>4}",
        "pane",
        "total",
        "p95",
        "max",
        "share",
        "renders",
        "reused",
        "handlers",
        "runs",
        "writes/r",
        "nodes",
        "fail",
    ));
    for row in rows {
        let handlers: u64 = [
            "on_key",
            "on_action",
            "on_click",
            "on_scroll",
            "on_event",
            "decorate",
        ]
        .iter()
        .map(|hook| u(row, &["handlers", hook, "total_us"]))
        .sum();
        let name = crate::kernel::perf::fit_columns(&name_of(row), width);
        out.push_str(&format!(
            "\n{name}  {:>8} {:>8} {:>8} {:>4}% {:>7} {:>6} {:>8} {:>5} {:>8.2} {:>5} {:>4}",
            fmt_us(u(row, &["total_us"])),
            fmt_us(u(row, &["render", "p95_us"])),
            fmt_us(u(row, &["render", "max_us"])),
            (row["frame_share"].as_f64().unwrap_or(0.0) * 100.0).round() as u64,
            u(row, &["renders"]),
            u(row, &["reused"]),
            fmt_us(handlers),
            u(row, &["runs", "finished"]),
            row["store_writes_per_render"].as_f64().unwrap_or(0.0),
            u(row, &["tree", "nodes"]),
            u(row, &["failures"]),
        ));
        for hint in row["hints"].as_array().into_iter().flatten() {
            out.push_str(&format!(
                "\n{:width$}  ! {}",
                "",
                hint["text"].as_str().unwrap_or("?")
            ));
        }
    }
    CommandOutput::new(json, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_snapshot_exits_nonzero_with_hint() {
        let db = Database::open_in_memory().unwrap();
        let out = run(&db, false).unwrap();
        assert!(out.human.contains("THURBOX_PERF_LOG=1"));
        assert!(out.failure.is_some(), "no snapshot → non-zero exit");
    }

    const SNAPSHOT: &str = r#"{"pid":42,"captured_at":0,"session_count":3,"tick_count":100,
        "counters":{"iterations":100,"frames":7,"skipped":90,
                    "renders":21,"failures":0,"reloads":1,
                    "renders_skipped":88,"groups_reused":140},
        "frame":{"p50_us":900,"p95_us":4000,"max_us":30000,"samples":7},
        "republish":{"p50_us":800,"p95_us":2000,"max_us":9000,"samples":7},
        "tick":{"p50_us":250,"p95_us":500,"max_us":1000,"samples":100},
        "startup":{"config_init_ms":3,"db_open_ms":11,"extension_heal_ms":15,
                   "ui_build_ms":40,"first_frame_ms":120},
        "slow_ops":[{"op":"interface_reload","ms":320,"plugin":null},
                    {"op":"input_dispatch","ms":140,"plugin":"files"}]}"#;

    #[test]
    fn snapshot_renders_counters_and_slow_ops() {
        let db = Database::open_in_memory().unwrap();
        db.set_perf_snapshot(SNAPSHOT).unwrap();
        let out = run(&db, false).unwrap();
        assert!(out.failure.is_none());
        assert!(out.human.contains("pid 42"));
        assert!(out.human.contains("interface_reload"));
        assert!(out.human.contains("320ms"));
        // A slow op names the pane that spent it.
        assert!(out.human.contains("files"));
        // republish is reported beside frame: telling the table rebuild apart
        // from the paint is the whole reason it has its own histogram.
        assert!(out.human.contains("republish"));
        assert!(out.human.contains("ui 40ms"));
        // The two skip counters: a skipped frame is invisible in anything else.
        assert!(out.human.contains("renders skipped"));
        assert!(out.human.contains("88"));
        assert!(out.human.contains("groups reused"));
        assert_eq!(out.json["counters"]["frames"], 7);
    }

    #[test]
    fn a_snapshot_from_before_plugins_says_so_rather_than_printing_nothing() {
        let db = Database::open_in_memory().unwrap();
        db.set_perf_snapshot(SNAPSHOT).unwrap();
        let out = run(&db, true).unwrap();
        assert!(out.failure.is_some());
        assert!(out.human.contains("per-plugin"), "{}", out.human);
    }

    #[test]
    fn a_wide_pane_name_keeps_the_columns_after_it_aligned() {
        // Two rows with identical numbers: whatever the names are made of, both
        // lines must occupy the same terminal columns.
        let db = Database::open_in_memory().unwrap();
        let row =
            |name: &str| format!(r#"{{"name":"{name}","total_us":1200,"renders":3,"reused":1}}"#);
        db.set_perf_snapshot(&format!(
            r#"{{"pid":1,"captured_at":0,"plugin_window":{{"frames":3,"frame_total_us":9000}},
                "plugins":[{},{}]}}"#,
            row("files"),
            row("名前ペイン"),
        ))
        .unwrap();
        let out = run(&db, true).unwrap();
        let widths: Vec<usize> = out
            .human
            .lines()
            .filter(|line| line.contains("1.2ms"))
            .map(unicode_width::UnicodeWidthStr::width)
            .collect();
        assert_eq!(widths.len(), 2, "{}", out.human);
        assert_eq!(widths[0], widths[1], "{}", out.human);
    }
}
