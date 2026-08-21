//! `thurbox-cli perf` — read the perf snapshot a running TUI publishes.
//!
//! The TUI writes a JSON snapshot (counters, frame/republish/tick timing
//! percentiles, slow ops, startup phase breakdown) into the SQLite `metadata`
//! table while perf timing is active — `THURBOX_PERF_LOG=1` or an open perf
//! HUD (F12). `kernel::perf::snapshot_json` owns the shape; this renders it.
//! This command prints the latest one, so a running instance can be inspected
//! from outside without tailing `thurbox.log`. See `docs/PERFORMANCE.md`.

use serde_json::Value;

use crate::storage::Database;

use super::output::{kv, CommandOutput};

/// Human hint shown when no snapshot exists (or it can't be parsed).
const NO_SNAPSHOT_HINT: &str = "No perf snapshot published. Run the TUI with THURBOX_PERF_LOG=1 \
     or open its perf HUD (F12), then retry.";

/// Run the `perf` command: print the last published snapshot.
pub fn run(db: &Database) -> Result<CommandOutput, String> {
    let raw = db
        .get_perf_snapshot()
        .map_err(|e| format!("failed to read perf snapshot: {e}"))?;
    let Some(raw) = raw else {
        return Ok(CommandOutput::failed(
            serde_json::json!({ "snapshot": null }),
            NO_SNAPSHOT_HINT,
            "no perf snapshot",
        ));
    };
    let snapshot: Value =
        serde_json::from_str(&raw).map_err(|e| format!("perf snapshot is not valid JSON: {e}"))?;
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

fn render_human(s: &Value) -> String {
    let captured_at = u(s, &["captured_at"]);
    let age = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().saturating_sub(captured_at))
        .unwrap_or(0);

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
        ("captured", format!("{age}s ago (pid {})", u(s, &["pid"]))),
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
                    "\n  {:<20} {}ms",
                    op["op"].as_str().unwrap_or("?"),
                    u(op, &["ms"]),
                ));
            }
        }
        _ => out.push_str("\n\nslow ops: none recorded"),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_snapshot_exits_nonzero_with_hint() {
        let db = Database::open_in_memory().unwrap();
        let out = run(&db).unwrap();
        assert!(out.human.contains("THURBOX_PERF_LOG=1"));
        assert!(out.failure.is_some(), "no snapshot → non-zero exit");
    }

    #[test]
    fn snapshot_renders_counters_and_slow_ops() {
        let db = Database::open_in_memory().unwrap();
        db.set_perf_snapshot(
            r#"{"pid":42,"captured_at":0,"session_count":3,"tick_count":100,
                "counters":{"iterations":100,"frames":7,"skipped":90,
                            "renders":21,"failures":0,"reloads":1,
                            "renders_skipped":88,"groups_reused":140},
                "frame":{"p50_us":900,"p95_us":4000,"max_us":30000,"samples":7},
                "republish":{"p50_us":800,"p95_us":2000,"max_us":9000,"samples":7},
                "tick":{"p50_us":250,"p95_us":500,"max_us":1000,"samples":100},
                "startup":{"config_init_ms":3,"db_open_ms":11,"extension_heal_ms":15,
                           "ui_build_ms":40,"first_frame_ms":120},
                "slow_ops":[{"op":"interface_reload","ms":320}]}"#,
        )
        .unwrap();
        let out = run(&db).unwrap();
        assert!(out.failure.is_none());
        assert!(out.human.contains("pid 42"));
        assert!(out.human.contains("interface_reload"));
        assert!(out.human.contains("320ms"));
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
}
