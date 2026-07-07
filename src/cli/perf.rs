//! `thurbox-cli perf` — read the perf snapshot a running TUI publishes.
//!
//! The TUI writes a JSON snapshot (counters, frame/tick timing percentiles,
//! slow ops, startup phase breakdown) into the SQLite `metadata` table while
//! perf timing is active — `THURBOX_PERF_LOG=1` or an open perf HUD (F12).
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

    let mut pairs: Vec<(&str, String)> = vec![
        ("captured", format!("{age}s ago (pid {})", u(s, &["pid"]))),
        ("sessions", u(s, &["session_count"]).to_string()),
        ("ticks", u(s, &["tick_count"]).to_string()),
        ("frames", u(s, &["counters", "frames_rendered"]).to_string()),
        (
            "idle skips",
            u(s, &["counters", "redraws_skipped"]).to_string(),
        ),
        (
            "order rebuilds",
            u(s, &["counters", "ordered_sessions_rebuilds"]).to_string(),
        ),
        (
            "hook loads",
            u(s, &["counters", "hook_state_loads"]).to_string(),
        ),
        (
            "frame p50/p95/max",
            format!(
                "{} / {} / {}",
                fmt_us(u(s, &["frame", "p50_us"])),
                fmt_us(u(s, &["frame", "p95_us"])),
                fmt_us(u(s, &["frame", "max_us"])),
            ),
        ),
        (
            "tick p50/p95/max",
            format!(
                "{} / {} / {}",
                fmt_us(u(s, &["tick", "p50_us"])),
                fmt_us(u(s, &["tick", "p95_us"])),
                fmt_us(u(s, &["tick", "max_us"])),
            ),
        ),
    ];

    if let Some(startup) = s.get("startup").filter(|v| v.is_object()) {
        pairs.push((
            "startup",
            format!(
                "config {}ms · db {}ms · heal {}ms · restore {}ms",
                u(startup, &["config_init_ms"]),
                u(startup, &["db_open_ms"]),
                u(startup, &["extension_heal_ms"]),
                u(startup, &["restore_ms"]),
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
                "counters":{"frames_rendered":7,"redraws_skipped":90,
                            "ordered_sessions_rebuilds":1,"hook_state_loads":2},
                "frame":{"p50_us":900,"p95_us":4000,"max_us":30000},
                "tick":{"p50_us":250,"p95_us":500,"max_us":1000},
                "slow_ops":[{"op":"code_review_build","ms":320}]}"#,
        )
        .unwrap();
        let out = run(&db).unwrap();
        assert!(out.failure.is_none());
        assert!(out.human.contains("pid 42"));
        assert!(out.human.contains("code_review_build"));
        assert!(out.human.contains("320ms"));
        assert_eq!(out.json["counters"]["frames_rendered"], 7);
    }
}
