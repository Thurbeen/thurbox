//! Resource limits: the guarantees Luau would have donated and Lua 5.4 makes
//! the kernel build.
//!
//! Separate from `kernel_mvp` because these tests mutate process-wide bounds,
//! and running them beside the rest would change the limits under those tests.
//! Within the file each holds [`limits`] for its whole run, so they run in
//! sequence under any runner, restoring the defaults afterwards.

use thurbox::kernel::host::{self, Epoch, LuaHost, MemoryPressure, Published, RenderContext};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{DeletedRow, Snapshot};
use thurbox::kernel::theme::Themes;

fn plugin_dir(source: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    std::fs::write(plugins.join("10_probe.lua"), source).expect("write");
    dir
}

/// Held by every test here: the limits are process-wide, and `cargo test` runs
/// a file's tests on parallel threads.
fn limits() -> std::sync::MutexGuard<'static, ()> {
    static LIMITS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LIMITS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn ctx() -> RenderContext {
    RenderContext {
        width: 20,
        height: 3,
        focused: false,
        elapsed: 0.0,
        frame: 0,
    }
}

#[test]
fn a_float_focus_hook_that_never_returns_is_interrupted_and_reported() {
    let _limits = limits();
    host::set_instruction_budget(200_000);
    let dir = plugin_dir(
        r#"return {
             name = "spinner", floats = true,
             on_focus_cycle = function() while true do end end,
             render = function() return { type = "text", text = "" } end,
           }"#,
    );
    let spinner = LuaHost::new(dir.path());
    assert!(spinner.error.is_none(), "{:?}", spinner.error);
    let result = spinner.on_focus_cycle(0, "next");
    host::set_instruction_budget(0);
    let failure = result.expect_err("cycle must be interrupted");
    assert_eq!(failure.plugin, "spinner");
    assert_eq!(failure.phase, host::Phase::Key);
    assert!(failure.message.contains("instruction budget"), "{failure}");
    spinner.render(0, ctx()).expect("budget removed after hook");
}

#[test]
fn an_event_handler_that_never_returns_is_interrupted_and_reported() {
    let _limits = limits();
    host::set_instruction_budget(200_000);
    let dir = plugin_dir(
        r#"return {
             name = "spinner", slot = "a",
             events = { "session.status" },
             on_event = function() while true do end end,
             render = function() return { text = "" } end,
           }"#,
    );
    let spinner = LuaHost::new(dir.path());
    assert!(spinner.error.is_none(), "{:?}", spinner.error);
    let failures = spinner.dispatch_event(
        &thurbox::kernel::events::Event::new("session.status").with("session", Some("s1")),
    );
    host::set_instruction_budget(0);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].plugin, "spinner");
    assert_eq!(failures[0].phase, host::Phase::Event);
    assert!(
        failures[0].message.contains("instruction budget"),
        "{}",
        failures[0].message
    );
}

/// A probe holding `chunks` of [`CHUNK`] bytes from load. `grow` holds one
/// more; every render leaves garbage behind.
fn resident_probe(chunks: usize) -> String {
    format!(
        r#"local held = {{}}
           local function hold() held[#held + 1] = string.rep("x", {CHUNK}) end
           for _ = 1, {chunks} do hold() end
           return {{
             name = "probe", slot = "a",
             on_action = function(action)
               if action ~= "grow" then return false end
               hold()
               return true
             end,
             render = function()
               local junk = {{}}
               for i = 1, 16 do junk[i] = string.rep("g", 4096) end
               return {{ type = "text", text = "ok" }}
             end,
           }}"#
    )
}

const CHUNK: usize = 64 * 1024;

/// Grow the probe one chunk at a time until the VM reports pressure.
fn grow_until_pressure(host: &LuaHost) -> Option<MemoryPressure> {
    let probe = host.index_of("probe").expect("the probe loaded");
    for _ in 0..4096 {
        if let Some(pressure) = host.memory_pressure() {
            return Some(pressure);
        }
        let _ = host.on_action(probe, "grow");
    }
    None
}

#[test]
fn memory_pressure_tells_a_full_vm_from_a_large_one() {
    let _limits = limits();
    let limit = 16 * 1024 * 1024;
    host::set_memory_limit(limit);
    let base = LuaHost::new(plugin_dir(&resident_probe(0)).path()).used_memory();
    let loading = |share: usize| {
        let host =
            LuaHost::new(plugin_dir(&resident_probe((limit / 100 * share - base) / CHUNK)).path());
        assert!(host.error.is_none(), "{:?}", host.error);
        host
    };

    // Large and stable: above the rebuild threshold from load, and not full.
    let large = loading(93);
    assert_eq!(
        large.memory_pressure(),
        None,
        "a VM that loads large is not full"
    );
    // Checked once a frame, as the loop does: the check is also what collects
    // the garbage, which the collector's own pace would leave to pile up to the
    // ceiling, where a buffer allocation fails without an emergency collection.
    for _ in 0..400 {
        large.render(0, ctx()).expect("the large VM renders");
        assert_eq!(
            large.memory_pressure(),
            None,
            "garbage piling up under a large resident heap is not pressure"
        );
    }

    // A leak from a small build is caught by its growth, before the ceiling.
    let leaking = loading(10);
    assert_eq!(grow_until_pressure(&leaking), Some(MemoryPressure::Grown));

    // Accepted as resident, it is pressure again only once it grows further.
    leaking.accept_resident_memory();
    let accepted = leaking.used_memory();
    assert_eq!(
        leaking.memory_pressure(),
        None,
        "the heap was accepted as resident"
    );
    assert_eq!(grow_until_pressure(&leaking), Some(MemoryPressure::Grown));
    assert!(
        leaking.used_memory() >= accepted + limit / 20,
        "only growth past the accepted heap counts"
    );

    // Accepted this close to the ceiling, no growth allowance fits under it:
    // only the ceiling itself can say the VM filled, and it must.
    leaking.accept_resident_memory();
    assert!(
        leaking.used_memory() + limit / 20 > limit - limit / 32,
        "growth alone could not report this VM before it is full"
    );
    assert_eq!(
        grow_until_pressure(&leaking),
        Some(MemoryPressure::Exhausted)
    );
    let probe = leaking.index_of("probe").expect("the probe loaded");
    while leaking.on_action(probe, "grow").is_ok() {}
    assert_eq!(
        leaking.memory_pressure(),
        Some(MemoryPressure::Exhausted),
        "a VM whose allocations fail is full"
    );

    // A live heap parked just under a threshold must not buy a full collection
    // on every frame: the next one waits for a share of new garbage.
    let parked = loading(10);
    let probe = parked.index_of("probe").expect("the probe loaded");
    while parked.used_memory() < limit / 1000 * 895 {
        parked.on_action(probe, "grow").expect("room to grow");
    }
    let before = parked.pressure_collections();
    for _ in 0..200 {
        parked.render(0, ctx()).expect("the parked VM renders");
        assert_eq!(parked.memory_pressure(), None, "parked, not full");
    }
    // 200 frames of 64 KiB of garbage is 12.5 MiB; one collection per 1/64
    // of the limit is at most 50 of them.
    let collections = parked.pressure_collections() - before;
    assert!(
        collections <= 51,
        "{collections} full collections in 200 frames"
    );
    host::set_memory_limit(0);
}

#[test]
fn the_limits_are_settable_and_enforced() {
    let _limits = limits();
    // --- memory ------------------------------------------------------------
    // A small ceiling, so exhausting it is a test rather than an ordeal.
    host::set_memory_limit(4 * 1024 * 1024);
    let dir = plugin_dir(
        r#"local t = {}
           return {
             name = "hog", slot = "a",
             render = function()
               for i = 1, 1e9 do t[i] = string.rep("x", 1024) end
               return { text = "never" }
             end,
           }"#,
    );
    let hog = LuaHost::new(dir.path());
    assert!(
        !hog.near_memory_limit(),
        "a fresh VM is nowhere near the ceiling"
    );
    let error = hog
        .render(0, ctx())
        .expect_err("exhausting memory must fail the plugin");
    assert_eq!(error.plugin, "hog");
    assert!(
        error.message.contains("memory"),
        "the message should name the limit: {}",
        error.message
    );
    // The process is still here, which is the actual guarantee.
    // And the VM is reported as full, which is what gets it rebuilt rather
    // than called again on every frame. The hog keeps what it allocated: a
    // spike a collection returns leaves a VM that is not full at all.
    assert!(
        hog.near_memory_limit(),
        "a VM that ran out is near its ceiling"
    );
    host::set_memory_limit(0);

    // --- instructions ------------------------------------------------------
    // A tiny budget, so the guard fires immediately rather than after 20M.
    host::set_instruction_budget(200_000);
    assert_eq!(host::instruction_budget(), 200_000);

    let dir = plugin_dir(
        r#"return {
             name = "spin", slot = "a",
             render = function() while true do end end,
           }"#,
    );
    let spin = LuaHost::new(dir.path());
    let started = std::time::Instant::now();
    let error = spin
        .render(0, ctx())
        .expect_err("an unterminated render must be interrupted");
    let elapsed = started.elapsed();

    assert_eq!(error.plugin, "spin");
    assert!(
        error.message.contains("instruction budget"),
        "{}",
        error.message
    );
    // A smaller budget must actually mean a shorter wait, which is the whole
    // point of the setting.
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "a 200k budget took {elapsed:?}"
    );

    host::set_instruction_budget(0);
    assert_eq!(host::instruction_budget(), host::INSTRUCTION_BUDGET);
}

#[test]
fn a_healthy_plugin_is_unaffected_by_the_hook() {
    let _limits = limits();
    // The overhead question from task 2.12, as a bound rather than a number:
    // a plugin doing ordinary work must not be near the budget.
    let dir = plugin_dir(
        r#"return {
             name = "ordinary", slot = "a",
             render = function()
               local rows = {}
               for i = 1, 500 do rows[i] = { type = "text", len = 1, text = "row " .. i } end
               return { type = "box", children = rows }
             end,
           }"#,
    );
    let host = LuaHost::new(dir.path());

    let started = std::time::Instant::now();
    for _ in 0..100 {
        host.render(0, ctx()).expect("500 rows should render");
    }
    let elapsed = started.elapsed();

    // 100 renders of 500 rows each. Generous, because this runs on shared CI
    // hardware; it is a regression guard, not a benchmark.
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "100 renders took {elapsed:?} — the instruction hook may be too frequent"
    );
}

// ── A long deleted history ──────────────────────────────────────────────────
//
// Every plugin shares one VM under the memory limit, and the `deleted` group is
// rebuilt whenever the database moves — every mirror pass of a host with
// history. Each rebuild must be garbage the collector can take back, or ~1600
// mirrored tombstones filled the ceiling within a few hundred publishes and
// took every pane down with it.

fn bundled() -> LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

fn snapshot(deleted: usize) -> Snapshot {
    Snapshot {
        deleted: (0..deleted)
            .map(|i| DeletedRow {
                id: format!("00000000-0000-4000-8000-{i:012}"),
                name: format!("worker-session-{i}"),
                agent: "claude".into(),
                deleted_at: 3_600_000,
                worktrees: 1,
                partial: false,
                restore_refusal: None,
            })
            .collect(),
        taken_at_ms: 7_200_000,
        ..Snapshot::default()
    }
}

fn publish(host: &LuaHost, snapshot: &Snapshot, registry: &Registry) {
    let themes = Themes::load(None);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        epoch: Epoch::always_fresh(),
        snapshot,
        attach_errors: &Default::default(),
        inflight: &[],
        themes: &themes,
        registry,
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

fn frames(host: &LuaHost, snapshot: &Snapshot, count: usize) -> usize {
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let mut peak = 0;
    for frame in 0..count {
        publish(host, snapshot, &registry);
        // The panes that read `thurbox.deleted`.
        for name in ["sessions", "restore", "confirm"] {
            let Some(index) = host.index_of(name) else {
                continue;
            };
            host.render(
                index,
                RenderContext {
                    width: 40,
                    height: 40,
                    focused: false,
                    elapsed: frame as f64,
                    frame: frame as u64,
                },
            )
            .unwrap_or_else(|e| panic!("{name} at frame {frame}: {e}"));
        }
        peak = peak.max(host.used_memory());
    }
    peak
}

#[test]
fn rebuilding_a_long_deleted_history_stays_far_below_the_memory_limit() {
    let _limits = limits();
    let host = bundled();
    let peak = frames(&host, &snapshot(1600), 300);
    // Measured at ~6 MiB; the ceiling leaves room for the collector's pace
    // while staying an order of magnitude under the limit it used to reach.
    let ceiling = 32 * 1024 * 1024;
    assert!(
        peak < ceiling,
        "peak {peak} bytes over 300 publishes (limit {})",
        host::memory_limit()
    );
}
