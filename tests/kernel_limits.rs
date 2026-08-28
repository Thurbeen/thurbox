//! Resource limits: the guarantees Luau would have donated and Lua 5.4 makes
//! the kernel build.
//!
//! Separate from `kernel_mvp` because both tests mutate a process-wide bound,
//! and running them beside the rest would change the limits under those tests.
//! They run in one file, in sequence, restoring the defaults afterwards.

use thurbox::kernel::host::{self, LuaHost, RenderContext};

fn plugin_dir(source: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    std::fs::write(plugins.join("10_probe.lua"), source).expect("write");
    dir
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
fn an_event_handler_that_never_returns_is_interrupted_and_reported() {
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

#[test]
fn the_limits_are_settable_and_enforced() {
    // --- memory ------------------------------------------------------------
    // A small ceiling, so exhausting it is a test rather than an ordeal.
    host::set_memory_limit(4 * 1024 * 1024);
    let dir = plugin_dir(
        r#"return {
             name = "hog", slot = "a",
             render = function()
               local t = {}
               for i = 1, 1e9 do t[i] = string.rep("x", 1024) end
               return { text = "never" }
             end,
           }"#,
    );
    let hog = LuaHost::new(dir.path());
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
