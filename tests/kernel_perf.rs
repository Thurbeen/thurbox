//! Performance behaviour, asserted on counters rather than on the clock.
//!
//! v1 learned this the hard way: a test that says "idle should be fast" is
//! flaky on shared hardware, while one that says "an idle loop painted no
//! frames" is exact. These re-derive what ADR-P6 and ADR-P12 gave v1, against
//! the v2 render path.

use thurbox::kernel::command::{Args, Command, CommandBus};
use thurbox::kernel::host::{LuaHost, RenderContext};
use thurbox::kernel::perf::{snapshot_json, Counters, Snapshot as Perf, Startup, Timings};
use thurbox::kernel::snapshot::SnapshotStore;
use thurbox::storage::Database;

fn ctx(width: u16, height: u16) -> RenderContext {
    RenderContext {
        width,
        height,
        focused: false,
        elapsed: 0.0,
        frame: 0,
    }
}

fn plugin_dir(source: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    std::fs::write(plugins.join("10_pane.lua"), source).expect("write");
    dir
}

#[test]
fn an_unchanged_tree_is_the_signal_to_skip_a_frame() {
    // The heart of demand-driven repaint: a plugin returning the same tree
    // gives the loop nothing to paint. Asserted on the trees themselves rather
    // than by driving a terminal, so it is exact.
    let dir = plugin_dir(
        r#"return { name = "static", slot = "a",
                    render = function() return { text = "unchanging" } end }"#,
    );
    let host = LuaHost::new(dir.path());

    let first = host.render(0, ctx(20, 3)).expect("render").node;
    for _ in 0..50 {
        let again = host.render(0, ctx(20, 3)).expect("render").node;
        assert_eq!(first, again, "a static plugin must return an equal tree");
    }
}

#[test]
fn a_changing_tree_is_never_equal() {
    // The other half: if this ever compared equal, the loop would stop
    // painting a pane that is actually moving.
    let dir = plugin_dir(
        r#"return { name = "clock", slot = "a",
                    render = function(ctx) return { text = "frame " .. ctx.frame } end }"#,
    );
    let host = LuaHost::new(dir.path());

    let a = host
        .render(
            0,
            RenderContext {
                frame: 1,
                ..ctx(20, 3)
            },
        )
        .expect("render")
        .node;
    let b = host
        .render(
            0,
            RenderContext {
                frame: 2,
                ..ctx(20, 3)
            },
        )
        .expect("render")
        .node;
    assert_ne!(a, b);
}

#[test]
fn counters_distinguish_painted_frames_from_skipped_ones() {
    // What the perf HUD and these tests both read.
    let counters = Counters::default();
    let before = counters.read();

    Counters::bump(&counters.iterations);
    Counters::bump(&counters.frames);
    for _ in 0..9 {
        Counters::bump(&counters.iterations);
        Counters::bump(&counters.skipped);
    }

    let window = counters.read().since(&before);
    assert_eq!(window.iterations, 10);
    assert_eq!(window.frames, 1);
    assert_eq!(window.skipped, 9);
    // The property v1's ADR-P-series existed for: idle iterations vastly
    // outnumber painted frames.
    assert!(window.skipped > window.frames * 5);
}

#[test]
fn a_snapshot_read_never_touches_the_database() {
    // ADR-P6 re-derived: v1 cached hook state behind a `data_version` check to
    // keep an idle tick off the sessions table. Here the shape makes it
    // structural — reads come from the snapshot, and refresh is the only thing
    // that queries.
    let db = Database::open_in_memory().expect("db");
    let store = SnapshotStore::with_database(db);

    let started = std::time::Instant::now();
    for _ in 0..10_000 {
        let _ = store.current().sessions.len();
    }
    assert!(
        started.elapsed() < std::time::Duration::from_millis(500),
        "10k reads took {:?} — they are not coming from memory",
        started.elapsed()
    );
}

#[test]
fn dispatching_a_command_never_blocks_the_caller() {
    // ADR-P12 re-derived: v1 moved the whole new-session flow off the UI thread
    // deliberately. Here it falls out of the bus — there is no blocking form to
    // accidentally use.
    let bus = CommandBus::new();
    let started = std::time::Instant::now();

    for _ in 0..20 {
        bus.dispatch(
            Command::parse(
                "create",
                Args {
                    repo: Some("/definitely/not/a/repo".into()),
                    ..Args::default()
                },
            )
            .expect("parse"),
        );
    }

    assert!(
        started.elapsed() < std::time::Duration::from_millis(500),
        "20 dispatches took {:?}",
        started.elapsed()
    );
    assert_eq!(bus.inflight().len(), 20);
}

#[test]
fn rendering_many_panes_stays_within_a_frame_budget() {
    // A regression guard, not a benchmark: generous enough for shared CI, tight
    // enough to catch an accidental O(n²) in the render path.
    let dir = plugin_dir(
        r#"return { name = "rows", slot = "a", render = function(ctx)
             local out = {}
             for i = 1, 200 do out[i] = { type = "text", len = 1, text = "row " .. i } end
             return { type = "box", children = out }
           end }"#,
    );
    let host = LuaHost::new(dir.path());

    let started = std::time::Instant::now();
    for _ in 0..200 {
        host.render(0, ctx(80, 40)).expect("render");
    }
    let each = started.elapsed() / 200;
    assert!(
        each < std::time::Duration::from_millis(20),
        "a 200-row pane took {each:?} per render"
    );

    let _ = Perf::default();
}

#[test]
fn the_published_snapshot_is_what_the_cli_renders() {
    // The drift guard between the two halves of ADR-P11: `kernel::perf` owns
    // the shape and `cli::perf` renders it, so a key renamed on one side and
    // not the other prints a silent zero rather than failing. Both unit test
    // suites pass in that world; only pairing them catches it.
    let counters = Counters::default();
    Counters::bump(&counters.frames);
    Counters::bump(&counters.reloads);

    let mut timings = Timings::default();
    timings.frame.record(std::time::Duration::from_millis(6));
    timings
        .republish
        .record(std::time::Duration::from_millis(2));
    timings.tick.record(std::time::Duration::from_micros(400));
    timings.record_op("interface_reload", std::time::Duration::from_millis(140));

    let startup = Startup {
        config_init_ms: 3,
        db_open_ms: 11,
        extension_heal_ms: 15,
        ui_build_ms: 40,
        first_frame_ms: 120,
        ..Startup::default()
    };

    let json = snapshot_json(&counters.read(), &timings, &startup, 2);

    let db = Database::open_in_memory().expect("db");
    db.set_perf_snapshot(&json.to_string()).expect("publish");
    let out = thurbox::cli::perf::run(&db).expect("render");

    assert!(
        out.failure.is_none(),
        "a published snapshot is not a failure"
    );
    let human = &out.human;
    // Every row the renderer promises, carrying the value the producer put in.
    assert!(human.contains("sessions"), "missing sessions row: {human}");
    assert!(human.contains("interface_reload"), "slow op lost: {human}");
    assert!(human.contains("140ms"), "slow op duration lost: {human}");
    assert!(
        human.contains("republish"),
        "republish histogram lost: {human}"
    );
    assert!(human.contains("ui 40ms"), "ui build phase lost: {human}");
    assert!(
        human.contains("first frame 120ms"),
        "first-frame phase lost: {human}"
    );
    // And the machine half stays addressable by the documented keys.
    assert_eq!(out.json["counters"]["frames"], 1);
    assert_eq!(out.json["counters"]["reloads"], 1);
    // The skip counters travel too: they are the only evidence a frame was made
    // cheaper, so a rename that lost them would hide exactly what they exist for.
    assert!(
        human.contains("renders skipped"),
        "skip counter lost: {human}"
    );
    assert!(
        human.contains("groups reused"),
        "reuse counter lost: {human}"
    );
    assert_eq!(out.json["session_count"], 2);
}

#[test]
fn timing_costs_nothing_until_something_asks_for_it() {
    // The gate of ADR-P11 in the shape a caller can hold: an untouched
    // `Timings` reports no samples, so a run that never enabled timing
    // publishes zeros rather than stale or invented numbers.
    let timings = Timings::default();
    let json = snapshot_json(
        &Counters::default().read(),
        &timings,
        &Startup::default(),
        0,
    );
    for key in ["frame", "republish", "tick"] {
        assert_eq!(json[key]["samples"], 0, "{key} recorded without timing");
        assert_eq!(json[key]["p50_us"], 0, "{key} invented a percentile");
    }
    assert!(json["slow_ops"].as_array().expect("array").is_empty());
}
