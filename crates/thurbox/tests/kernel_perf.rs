use std::time::{Duration, Instant};

use thurbox::kernel::command::{Args, Command, CommandBus};
use thurbox::kernel::events::Event;
use thurbox::kernel::host::{Epoch, LuaHost, Published, RenderContext};
use thurbox::kernel::perf::{
    snapshot_json, Counters, Hint, ModReport, ModRow, Snapshot as Perf, Startup, Timings,
};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::runs::RunEvent;
use thurbox::kernel::snapshot::{Snapshot, SnapshotStore};
use thurbox::kernel::theme::Themes;

fn ctx(width: u16, height: u16) -> RenderContext {
    RenderContext {
        width,
        height,
        focused: false,
        elapsed: 0.0,
        frame: 0,
    }
}

fn mod_dir(source: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    std::fs::write(mods.join("10_pane.lua"), source).expect("write");
    dir
}

#[test]
fn an_unchanged_tree_is_the_signal_to_skip_a_frame() {
    let dir = mod_dir(
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
    let dir = mod_dir(
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
    assert_ne!(
        a, b,
        "an equal tree would stop the loop painting a pane that is moving"
    );
}

#[test]
fn counters_distinguish_painted_frames_from_skipped_ones() {
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
    assert!(
        window.skipped > window.frames * 5,
        "idle iterations must vastly outnumber painted frames"
    );
}

#[test]
fn a_snapshot_read_never_touches_the_database() {
    let store = SnapshotStore::without_sessions(&Default::default());

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
    let bus = CommandBus::fleet(thurbox::fleet_link::FleetLink::local(
        "thurbox-test-no-fleet",
    ));
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
    let dir = mod_dir(
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
        "a 200-row pane took {each:?} per render: an accidental O(n²) in the render path?"
    );

    let _ = Perf::default();
}

#[test]
fn the_published_snapshot_is_what_the_cli_renders() {
    let counters = Counters::default();
    Counters::bump(&counters.frames);
    Counters::bump(&counters.reloads);

    let mut timings = Timings::default();
    timings.frame.record(std::time::Duration::from_millis(6));
    timings
        .republish
        .record(std::time::Duration::from_millis(2));
    timings.tick.record(std::time::Duration::from_micros(400));
    timings.record_op(
        "interface_reload",
        std::time::Duration::from_millis(140),
        None,
    );

    let startup = Startup {
        config_init_ms: 3,
        snapshot_open_ms: 11,
        ui_build_ms: 40,
        first_frame_ms: 120,
        ..Startup::default()
    };

    let json = snapshot_json(&counters.read(), &timings, &startup, 2, &Default::default());

    let _profile = publish_perf(&json.to_string());
    let out = thurbox::cli::perf::run(false).expect("render");

    assert!(
        out.failure.is_none(),
        "a published snapshot is not a failure"
    );
    let human = &out.human;
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
    assert_eq!(out.json["counters"]["frames"], 1);
    assert_eq!(out.json["counters"]["reloads"], 1);
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
    let timings = Timings::default();
    let json = snapshot_json(
        &Counters::default().read(),
        &timings,
        &Startup::default(),
        0,
        &Default::default(),
    );
    for key in ["frame", "republish", "tick"] {
        assert_eq!(json[key]["samples"], 0, "{key} recorded without timing");
        assert_eq!(json[key]["p50_us"], 0, "{key} invented a percentile");
    }
    assert!(json["slow_ops"].as_array().expect("array").is_empty());
}

const BUSY: &str = "local s = 0 for i = 1, 300000 do s = s + i end";

fn interface() -> (tempfile::TempDir, LuaHost) {
    interface_with(&[])
}

fn interface_with(extra: &[(&str, &str)]) -> (tempfile::TempDir, LuaHost) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    let write = |file: &str, body: String| {
        std::fs::write(mods.join(file), body).expect("write");
    };
    write(
        "10_heavy.lua",
        format!(
            r#"return {{ name = "heavy", slot = "center", render = function()
                 {BUSY}
                 store.rows = {{ s }}
                 return {{ type = "box", children = {{ {{ text = "a" }}, {{ text = "b" }} }} }}
               end }}"#
        ),
    );
    write(
        "20_cheap.lua",
        r#"return { name = "cheap", slot = "left", pure = true,
             events = { "user.ping" },
             on_event = function() end,
             render = function() return { text = "cheap" } end }"#
            .to_string(),
    );
    write(
        "30_listener.lua",
        format!(
            r#"return {{ name = "listener", slot = "right", pure = true,
                 events = {{ "user.ping" }},
                 on_event = function() {BUSY} end,
                 render = function() return {{ text = "listener" }} end }}"#
        ),
    );
    write(
        "40_modal.lua",
        r#"return { name = "modal", floats = true,
             render = function() return { text = "" } end }"#
            .to_string(),
    );
    for (file, body) in extra {
        write(file, body.to_string());
    }
    let host = LuaHost::new(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);
    (dir, host)
}

fn publish(host: &LuaHost) {
    let themes = Themes::load(None);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::new();
    host.publish(&Published {
        epoch: Epoch::default(),
        snapshot: &Snapshot::default(),
        attach_errors: &Default::default(),
        starts: &Default::default(),
        inflight: &[],
        themes: &themes,
        registry: &registry,
        diffs: &diffs,
        links: &Default::default(),
        search: None,
        meta: &Default::default(),
        metrics: &Default::default(),
        status_rows: 0,
        can_open: true,
        inventory: &[],
        ui_dir: "ui",
        settings: &Default::default(),
        repos: &repos,
        wants: &Default::default(),
        focus: None,
        selection: None,
        hovered: None,
        printing: &Default::default(),
    })
    .expect("publish");
}

fn paint(host: &LuaHost, frames: usize) {
    for _ in 0..frames {
        for index in 0..host.mods.len() {
            host.render(index, ctx(40, 4)).expect("render");
        }
        host.note_frame(Duration::from_millis(1));
    }
}

fn row<'a>(report: &'a ModReport, name: &str) -> &'a ModRow {
    report
        .rows
        .iter()
        .find(|row| row.name == name)
        .unwrap_or_else(|| panic!("no row for {name}: {report:?}"))
}

fn ping() -> Event {
    Event {
        name: "user.ping".to_string(),
        payload: Vec::new(),
        depth: 0,
        only: None,
    }
}

#[test]
fn the_expensive_pane_ranks_first_and_the_pure_one_shows_its_reuse() {
    let (_dir, host) = interface();
    host.set_perf_timing(true);
    publish(&host);
    paint(&host, 20);

    let report = host.mod_report();
    assert_eq!(report.frames, 20);
    assert_eq!(
        report.rows[0].name, "heavy",
        "the pane doing the work must rank first: {report:?}"
    );

    let heavy = row(&report, "heavy");
    assert_eq!(heavy.stats.renders, 20, "an impure pane runs every frame");
    assert_eq!(heavy.stats.reused, 0);
    assert!(heavy.stats.store_table_writes >= 20, "{heavy:?}");
    assert_eq!(heavy.stats.last_nodes, 3, "a box and its two texts");
    assert!(heavy.hints.contains(&Hint::NotPure), "{heavy:?}");
    assert!(heavy.hints.contains(&Hint::StoreTables), "{heavy:?}");

    let cheap = row(&report, "cheap");
    assert_eq!(cheap.stats.renders, 1, "a pure pane runs once and settles");
    assert_eq!(cheap.stats.reused, 19);
    assert!(
        cheap.hints.is_empty(),
        "a settled pure pane is fine: {cheap:?}"
    );

    let modal = row(&report, "modal");
    assert!(modal.hints.contains(&Hint::ClosedFloat), "{modal:?}");
}

#[test]
fn a_slow_on_event_is_attributed_to_its_mod() {
    let (_dir, host) = interface();
    host.set_perf_timing(true);
    publish(&host);

    host.begin_op();
    let failures = host.dispatch_event(&ping());
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(
        host.end_op().as_deref(),
        Some("listener"),
        "a slow op names the plugin that spent it, not merely the last one called"
    );

    let report = host.mod_report();
    let listener = row(&report, "listener");
    let cheap = row(&report, "cheap");
    assert_eq!(listener.stats.hooks.event.calls, 1);
    assert_eq!(cheap.stats.hooks.event.calls, 1);
    assert!(listener.stats.hooks.event.total_us > cheap.stats.hooks.event.total_us);
    assert_eq!(report.rows[0].name, "listener", "{report:?}");
}

#[test]
fn a_pure_pane_that_keeps_re_rendering_while_idle_is_flagged() {
    let (_dir, host) = interface_with(&[(
        "50_restless.lua",
        r#"return { name = "restless", slot = "bottom", pure = true,
             render = function()
               state.tick = (state.tick or 0) + 1
               return { text = "restless" }
             end }"#,
    )]);
    host.set_perf_timing(true);
    publish(&host);
    host.set_idle(true);
    paint(&host, 12);

    let report = host.mod_report();
    let restless = row(&report, "restless");
    assert_eq!(restless.stats.idle_renders, 12, "{restless:?}");
    assert!(restless.hints.contains(&Hint::IdleRenders), "{restless:?}");
    assert!(
        restless.hints.contains(&Hint::RenderWrites),
        "a pure render writing `state` never settles: {restless:?}"
    );
    let heavy = row(&report, "heavy");
    assert_eq!(heavy.stats.idle_renders, 12);
    assert!(
        !heavy.hints.contains(&Hint::IdleRenders),
        "an impure pane's idle renders are the not-pure hint's job: {heavy:?}"
    );
}

#[test]
fn nothing_is_recorded_per_mod_while_timing_is_off() {
    let (_dir, host) = interface();
    publish(&host);
    paint(&host, 5);
    host.begin_op();
    let _ = host.dispatch_event(&ping());

    let report = host.mod_report();
    assert_eq!(report.frames, 0);
    assert!(
        report.rows.iter().all(|row| row.stats.renders == 0
            && row.stats.reused == 0
            && row.stats.hooks.event.calls == 0),
        "{report:?}"
    );
    assert_eq!(
        host.end_op().as_deref(),
        Some("listener"),
        "a slow op is attributed whether or not timing is on"
    );
}

#[test]
fn turning_timing_on_starts_a_fresh_mod_window() {
    let (_dir, host) = interface();
    host.set_perf_timing(true);
    publish(&host);
    paint(&host, 3);
    host.set_perf_timing(false);
    host.set_perf_timing(true);
    assert_eq!(host.mod_report().frames, 0);
}

#[test]
fn a_run_is_counted_only_in_the_window_it_started_in() {
    let (_dir, host) = interface();
    let path = host.mods[host.index_of("heavy").expect("heavy")]
        .path
        .clone();
    let started = |at| RunEvent::Started {
        owner: path.clone(),
        at,
    };
    let finished = |started, ms| RunEvent::Finished {
        owner: path.clone(),
        started,
        took: Duration::from_millis(ms),
    };

    let before = Instant::now() - Duration::from_millis(50);
    host.note_run(&started(before));
    host.set_perf_timing(true);
    host.note_run(&finished(before, 30));
    let inside = Instant::now();
    host.note_run(&started(inside));
    let runs = row(&host.mod_report(), "heavy").stats.runs;
    assert_eq!(
        (runs.started, runs.finished),
        (1, 0),
        "a finish must not count in a window it did not start in: {runs:?}"
    );

    std::thread::sleep(Duration::from_millis(2));
    host.reset_mod_perf();
    host.note_run(&finished(inside, 5));
    let runs = row(&host.mod_report(), "heavy").stats.runs;
    assert_eq!(
        (runs.started, runs.finished),
        (0, 0),
        "a new window must not report a finish whose start it never saw: {runs:?}"
    );
}

const ROW_KEYS: [&str; 19] = [
    "closed_renders",
    "failures",
    "file",
    "floats",
    "frame_share",
    "handlers",
    "hints",
    "idle_renders",
    "name",
    "pure",
    "render",
    "renders",
    "reused",
    "runs",
    "store_table_writes",
    "store_writes",
    "store_writes_per_render",
    "total_us",
    "tree",
];

#[test]
fn the_cli_prints_the_mod_table_sorted_with_a_stable_json_shape() {
    let (_dir, host) = interface();
    host.set_perf_timing(true);
    publish(&host);
    paint(&host, 20);
    let heavy = host.mods[host.index_of("heavy").expect("heavy")]
        .path
        .clone();
    for ms in [10, 20] {
        let at = Instant::now();
        host.note_run(&RunEvent::Started {
            owner: heavy.clone(),
            at,
        });
        host.note_run(&RunEvent::Finished {
            owner: heavy.clone(),
            started: at,
            took: Duration::from_millis(ms),
        });
    }
    let report = host.mod_report();

    let json = snapshot_json(
        &Counters::default().read(),
        &Timings::default(),
        &Startup::default(),
        0,
        &report,
    );
    let _profile = publish_perf(&json.to_string());

    let out = thurbox::cli::perf::run(true).expect("render");
    assert!(out.failure.is_none());
    let rows = out.json["plugins"].as_array().expect("plugins array");
    assert_eq!(rows.len(), 4);
    let mut keys: Vec<&str> = rows[0]
        .as_object()
        .expect("row object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ROW_KEYS, "the row shape is a contract");
    for key in ["on_key", "on_action", "on_click", "on_scroll", "on_event"] {
        for field in ["calls", "total_us", "max_us"] {
            assert!(
                rows[0]["handlers"][key][field].is_number(),
                "handlers.{key}.{field} missing"
            );
        }
    }
    for field in ["p50_us", "p95_us", "max_us", "samples", "total_us"] {
        assert!(rows[0]["render"][field].is_number(), "render.{field}");
    }
    for field in ["asked", "started", "finished", "total_us", "max_us"] {
        assert!(rows[0]["runs"][field].is_number(), "runs.{field}");
    }
    for field in ["nodes", "spans", "max_nodes"] {
        assert!(rows[0]["tree"][field].is_number(), "tree.{field}");
    }
    let totals: Vec<u64> = rows
        .iter()
        .map(|row| row["total_us"].as_u64().expect("total"))
        .collect();
    assert!(
        totals.windows(2).all(|pair| pair[0] >= pair[1]),
        "sorted by total time: {totals:?}"
    );
    assert_eq!(rows[0]["name"], "heavy");
    assert_eq!(rows[0]["runs"]["finished"], 2);
    assert!(rows[0]["hints"]
        .as_array()
        .expect("hints")
        .iter()
        .any(|hint| hint["id"] == "not_pure" && hint["text"].is_string()));

    let human = &out.human;
    let heavy_at = human.find("heavy").expect("heavy row");
    let cheap_at = human.find("cheap").expect("cheap row");
    assert!(heavy_at < cheap_at, "{human}");
    assert!(human.contains("not pure"), "{human}");

    let plain = thurbox::cli::perf::run(false).expect("render");
    assert!(plain.human.contains("--mods"), "{}", plain.human);
}

#[path = "../../runtime-mux/tests/support/tmux_server.rs"]
mod tmux_server;

fn publish_perf(json: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("profile");
    thurbox::paths::set_test_dir(dir.path());
    let path = thurbox::paths::perf_snapshot_file().expect("perf path");
    std::fs::write(path, json).expect("publish");
    dir
}
