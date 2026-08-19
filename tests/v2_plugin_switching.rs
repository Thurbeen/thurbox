//! Turning a plugin off without losing it.
//!
//! The state that was missing is the one people want most: a plugin present,
//! intact, and not loaded. Without it, the only key that read like "not right
//! now" deleted the file — and for a plugin the user wrote there is no embedded
//! copy to restore, so the work was simply gone.
//!
//! What is asserted here is that disabling is *absence*, not a half-loaded
//! in-between. Every one of these is a consequence of the same decision — a
//! disabled file is not read — so they would all break together, which is
//! exactly why they are worth pinning separately.

use std::collections::HashSet;

use thurbox::kernel::bundled;
use thurbox::kernel::host::LuaHost;
use thurbox::kernel::inventory::{self, State, Trust};
use thurbox::kernel::registry::Registry;

/// A pane that declares a key, a setting and a slot, so its absence is testable
/// from several directions at once.
fn pane(name: &str) -> String {
    format!(
        r#"return {{
  name = "{name}",
  slot = "sessions",
  keys = {{ {{ key = "ctrl+alt+z", action = "{name}.go", desc = "go", scope = "global" }} }},
  settings = {{ {{ id = "loud", desc = "loud", default = false }} }},
  render = function() return {{ type = "text", text = "{name}" }} end,
}}"#
    )
}

fn interface(files: &[(&str, String)]) -> (tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = home.path().join("ui");
    bundled::materialize(&ui);
    for (name, source) in files {
        std::fs::write(ui.join("plugins").join(name), source).expect("write");
    }
    (home, ui)
}

fn loaded(ui: &std::path::Path, disabled: &[&str]) -> LuaHost {
    let host = LuaHost::new(ui);
    host.set_disabled(disabled.iter().copied().map(str::to_string).collect());
    let mut host = host;
    host.reload();
    host
}

#[test]
fn a_disabled_plugin_is_not_loaded_and_its_file_is_untouched() {
    let (_home, ui) = interface(&[("90_mine.lua", pane("mine"))]);
    let path = ui.join("plugins").join("90_mine.lua");
    let before = std::fs::read_to_string(&path).expect("read");

    let host = loaded(&ui, &["plugins/90_mine.lua"]);
    assert!(
        !host.plugins.iter().any(|p| p.name == "mine"),
        "a disabled plugin must not be loaded"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        before,
        "the file must be exactly as it was — that is the whole point"
    );
}

#[test]
fn turning_it_back_on_needs_nothing_restored() {
    let (_home, ui) = interface(&[("90_mine.lua", pane("mine"))]);
    assert!(!loaded(&ui, &["plugins/90_mine.lua"])
        .plugins
        .iter()
        .any(|p| p.name == "mine"));
    assert!(
        loaded(&ui, &[]).plugins.iter().any(|p| p.name == "mine"),
        "nothing was lost, so nothing has to be recovered"
    );
}

#[test]
fn a_disabled_plugin_declares_nothing() {
    // Its key is free, its setting is not offered, its slot is released. All one
    // consequence of not being read.
    let (_home, ui) = interface(&[("90_mine.lua", pane("mine"))]);

    let on = loaded(&ui, &[]);
    let (bindings, settings) = on.declarations();
    assert!(bindings.iter().any(|b| b.chord == "ctrl+alt+z"));
    assert!(settings.iter().any(|s| s.plugin == "mine"));
    assert_eq!(on.in_slot("sessions").len(), 2, "it shares the column");

    let off = loaded(&ui, &["plugins/90_mine.lua"]);
    let (bindings, settings) = off.declarations();
    assert!(
        !bindings.iter().any(|b| b.chord == "ctrl+alt+z"),
        "its key must be free while it is off"
    );
    assert!(
        !settings.iter().any(|s| s.plugin == "mine"),
        "its setting must not be offered"
    );
    assert_eq!(off.in_slot("sessions").len(), 1, "its slot is released");
}

#[test]
fn a_freed_key_can_be_claimed_without_a_conflict() {
    // The consequence worth stating: while it is off, the chord is genuinely
    // unowned rather than reserved for a plugin that is not running.
    let (_home, ui) = interface(&[
        ("90_mine.lua", pane("mine")),
        ("91_other.lua", pane("other")),
    ]);

    let both = loaded(&ui, &[]);
    let mut registry = Registry::default();
    let (bindings, settings) = both.declarations();
    registry.declare(bindings, settings);
    assert!(
        !registry.conflicts().is_empty(),
        "two plugins claiming one global chord is a conflict while both run"
    );

    let one = loaded(&ui, &["plugins/90_mine.lua"]);
    let mut registry = Registry::default();
    let (bindings, settings) = one.declarations();
    registry.declare(bindings, settings);
    assert!(
        registry.conflicts().is_empty(),
        "with one of them off, the chord has a single owner: {:?}",
        registry.conflicts()
    );
}

#[test]
fn a_plugin_that_would_fail_to_load_does_not_while_it_is_off() {
    // The recovery case: a bad file you can switch off, rather than one you must
    // delete to get the interface back.
    let (_home, ui) = interface(&[("90_broken.lua", "this is not lua".to_string())]);

    assert!(
        LuaHost::new(&ui).error.is_some(),
        "the broken file fails the load while it is on"
    );
    assert!(
        loaded(&ui, &["plugins/90_broken.lua"]).error.is_none(),
        "turning it off must be enough to get a working interface back"
    );
}

#[test]
fn a_disabled_file_reads_as_disabled_rather_than_failed() {
    // It is not loaded, and the loader must not mistake a deliberate choice for
    // a fault — `failed` is the reading that turns "I turned this off" into an
    // alarm.
    let (_home, ui) = interface(&[("90_mine.lua", pane("mine"))]);
    let host = loaded(&ui, &["plugins/90_mine.lua"]);

    let rows = inventory::rows(
        &host.plugins,
        &bundled::sources(&ui),
        &HashSet::new(),
        &HashSet::new(),
        host.error.as_deref(),
        &|_| Trust::NotAsked,
        &|path| path == "plugins/90_mine.lua",
    );
    let mine = rows
        .iter()
        .find(|row| row.path == "plugins/90_mine.lua")
        .expect("listed");
    assert_eq!(mine.state, State::Disabled, "{:?}", mine.state);
    assert_eq!(State::Disabled.as_str(), "disabled");
}

#[test]
fn the_decision_is_per_file_and_survives_a_restart() {
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("THURBOX_CONFIG_DIR", home.path());

    // Loaded, not default: only a registry that read `ui.json` writes it back,
    // which is what keeps a test from erasing the config directory it inherited
    // (`registry::Origin`). The loop's registry is a loaded one.
    let mut registry = Registry::load();
    registry
        .set_disabled("/ui/plugins/mine.lua", true)
        .expect("disable");
    assert!(registry.is_disabled("/ui/plugins/mine.lua"));
    assert!(
        !registry.is_disabled("/ui/plugins/other.lua"),
        "turning one off says nothing about another"
    );

    assert!(
        Registry::load().is_disabled("/ui/plugins/mine.lua"),
        "a choice that does not survive a restart is not a choice"
    );

    registry
        .set_disabled("/ui/plugins/mine.lua", false)
        .expect("enable");
    assert!(!registry.is_disabled("/ui/plugins/mine.lua"));
    // Asking for a state it is already in is not an error.
    registry
        .set_disabled("/ui/plugins/mine.lua", false)
        .expect("idempotent");

    std::env::remove_var("THURBOX_CONFIG_DIR");
}

#[test]
fn disabling_is_not_recorded_as_a_removal() {
    // A disabled bundled file is still one delivery should keep up to date, so
    // the delivery manifest must not learn about a user preference.
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("THURBOX_CONFIG_DIR", home.path());
    let ui = home.path().join("ui");
    bundled::materialize(&ui);

    let target = ui.join("plugins").join("10_sessions.lua");
    let mut registry = Registry::default();
    registry
        .set_disabled(&target.to_string_lossy(), true)
        .expect("disable");

    let sources = bundled::sources(&ui);
    assert_eq!(
        sources.get("plugins/10_sessions.lua"),
        Some(&bundled::Source::Bundled),
        "delivery must still see it as a file it ships, not as removed"
    );
    assert!(target.exists(), "and the file is still there");

    std::env::remove_var("THURBOX_CONFIG_DIR");
}
