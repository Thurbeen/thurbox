//! The layout presets: arrangements thurbox ships whole, of which the user picks
//! one.
//!
//! Each is asserted against the real delivery path and the real bundled panes —
//! a preset is only worth shipping if it loads, passes `plugin check` and draws
//! what it promises, and a switch is only safe if it never loses a layout the
//! user wrote themselves.

use std::path::Path;
use std::process::Command;

use ratatui::layout::Rect;

use thurbox::kernel::bundled;
use thurbox::kernel::host::{LuaHost, Published, RenderContext};
use thurbox::kernel::layout::resolve;
use thurbox::kernel::node::Node;
use thurbox::kernel::presets;
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;
use thurbox::session::SessionState;

/// An interface directory delivered the way a first run delivers one, with
/// `preset` chosen.
fn delivered(preset: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let preset = presets::find(preset).expect("a shipped preset");
    let report = bundled::materialize_with(dir.path(), preset);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    dir
}

fn host_at(dir: &Path) -> LuaHost {
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

fn row(name: &str) -> SessionRow {
    SessionRow {
        id: format!("{name}-0000-0000-0000-000000000000"),
        name: name.into(),
        agent: "claude".into(),
        status: SessionState::Idle,
        cwd: None,
        repo: Some("thurbox".into()),
        repos: vec!["thurbox".into()],
        branch: Some(format!("feat/{name}")),
        base_branch: None,
        backend: "local-tmux".into(),
        backend_id: Some("%1".into()),
        remote_host: None,
        agent_session_id: None,
        parent_id: None,
        display_order: None,
        worktree_count: 1,
        git: None,
        stopped: false,
        hook_state: None,
        reports_as: None,
        detected_agent: None,
        shell_backend_id: None,
        member_dirs: Vec::new(),
    }
}

fn publish(host: &LuaHost) {
    let themes = Themes::load(None);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    let snapshot = Snapshot {
        sessions: vec![row("alpha"), row("beta")],
        ..Snapshot::default()
    };
    host.publish(&Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
        snapshot: &snapshot,
        attach_errors: &Default::default(),
        inflight: &[],
        themes: &themes,
        registry: &registry,
        diffs: &diffs,
        links: &Default::default(),
        content: &Default::default(),
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

fn ctx(width: u16, height: u16) -> RenderContext {
    RenderContext {
        width,
        height,
        focused: false,
        elapsed: 0.0,
        frame: 0,
    }
}

fn index_of(host: &LuaHost, name: &str) -> usize {
    host.plugins
        .iter()
        .position(|plugin| plugin.name == name)
        .unwrap_or_else(|| panic!("no plugin named {name}"))
}

/// Where each slot lands at `width`×`height`, as the loop resolves it.
fn slots(host: &LuaHost, width: u16, height: u16) -> Vec<(String, Rect)> {
    let region = host.arrangement(width, height).expect("the arrangement");
    resolve(&region, Rect::new(0, 0, width, height))
        .into_iter()
        .map(|placed| (placed.slot, placed.rect))
        .collect()
}

fn rect_of(slots: &[(String, Rect)], slot: &str) -> Option<Rect> {
    slots
        .iter()
        .find(|(name, _)| name == slot)
        .map(|(_, rect)| *rect)
}

/// The text of every styled run a node carries on its frame and in its body,
/// flattened — enough to ask whether a chip is on the border.
fn words(node: &Node) -> String {
    format!("{node:?}")
}

/// A host with the list's selection published, and the slots the loop saw on
/// screen recorded, exactly as the binary does between arranging and painting.
fn split_shell_with_a_selection(dir: &Path, width: u16, height: u16) -> LuaHost {
    let host = host_at(dir);
    publish(&host);
    host.render(index_of(&host, "sessions"), ctx(40, 12))
        .expect("render the list");
    let on_screen: std::collections::HashSet<String> = slots(&host, width, height)
        .into_iter()
        .map(|(slot, _)| slot)
        .collect();
    host.note_placed(&on_screen);
    host
}

/// Run `thurbox-cli` in a private profile whose interface lives at
/// `<config>/ui`, the directory a real install delivers into.
fn cli(profile: &Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    command
        .args(args)
        .arg("--json")
        .env("HOME", profile.join("home"))
        .env("THURBOX_CONFIG_DIR", profile.join("config"))
        .env("THURBOX_DATA_DIR", profile.join("data"))
        .env_remove("THURBOX_UI_DIR")
        .env_remove("THURBOX_LAYOUT");
    command.output().expect("run thurbox-cli")
}

fn cli_json(profile: &Path, args: &[&str]) -> serde_json::Value {
    let output = cli(profile, args);
    assert!(
        output.status.success(),
        "thurbox-cli {args:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("json")
}

fn profile() -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("tempdir");
    for sub in ["home", "config", "data"] {
        std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
    }
    root
}

// ── what ships ─────────────────────────────────────────────────────────────

#[test]
fn classic_is_the_default_and_is_the_layout_that_shipped_before_presets() {
    // `classic` users must see no change, so the preset IS the old file, byte for
    // byte, and nothing chooses another one for them.
    assert_eq!(presets::DEFAULT, "classic");
    assert_eq!(
        presets::find("classic").expect("classic").layout,
        include_str!("../ui/layout.lua")
    );
    assert_eq!(
        thurbox::session::settings::Settings::default().layout,
        "classic"
    );
    let dir = delivered("classic");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("layout.lua")).expect("layout.lua"),
        include_str!("../ui/layout.lua")
    );
}

#[test]
fn every_preset_passes_plugin_check() {
    for preset in presets::PRESETS {
        let dir = delivered(preset.name);
        let output = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"))
            .args(["plugin", "check", "--json"])
            .env("THURBOX_UI_DIR", dir.path())
            .env("THURBOX_CONFIG_DIR", dir.path().join("config"))
            .env("THURBOX_DATA_DIR", dir.path().join("data"))
            .output()
            .expect("run thurbox-cli");
        assert!(
            output.status.success(),
            "{}: plugin check failed:\n{}",
            preset.name,
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn a_hand_edited_layout_without_a_shell_slot_still_passes_plugin_check() {
    // The shell pane ships to everyone, so an arrangement that leaves it out —
    // `classic`, or any layout a user wrote before presets existed — must not
    // start failing `check` over a slot it never asked for.
    let dir = delivered("classic");
    std::fs::write(
        dir.path().join("layout.lua"),
        "return function(ctx)\n  return { children = { { slot = \"sessions\", len = 30 }, { slot = \"center\" } } }\nend\n",
    )
    .expect("edit layout.lua");
    let host = host_at(dir.path());
    let unplaced = host
        .unplaced_slots(thurbox::kernel::layout::REFERENCE)
        .expect("resolves");
    assert!(!unplaced.contains(&"shell".to_string()), "{unplaced:?}");
}

// ── split-shell ───────────────────────────────────────────────────────────

#[test]
fn split_shell_puts_the_shell_below_the_agent_and_falls_back_to_the_agent_alone() {
    let dir = delivered("split-shell");
    let host = host_at(dir.path());
    publish(&host);

    let wide = slots(&host, 160, 48);
    let center = rect_of(&wide, "center").expect("the agent pane");
    let shell = rect_of(&wide, "shell").expect("the shell pane");
    assert_eq!(shell.x, center.x, "the shell sits under the agent");
    assert_eq!(shell.width, center.width);
    assert_eq!(shell.y, center.y + center.height, "directly below it");
    assert!(
        center.height > shell.height,
        "the agent keeps the larger half"
    );
    assert!(
        rect_of(&wide, "sessions").is_some(),
        "the list stays beside both"
    );

    // The narrow-width rule every preset keeps: below `two_panel_min_cols` there
    // is room for the agent and nothing else.
    let narrow = slots(&host, 60, 48);
    assert!(rect_of(&narrow, "center").is_some());
    assert!(rect_of(&narrow, "shell").is_none(), "{narrow:?}");
    assert!(rect_of(&narrow, "sessions").is_none(), "{narrow:?}");
}

#[test]
fn classic_never_places_the_shell_pane() {
    let dir = delivered("classic");
    let host = host_at(dir.path());
    publish(&host);
    assert!(rect_of(&slots(&host, 160, 48), "shell").is_none());
}

#[test]
fn split_shell_shows_the_agent_and_its_shell_at_once_for_the_selected_session() {
    let dir = delivered("split-shell");
    let host = split_shell_with_a_selection(dir.path(), 160, 48);

    let agent = host
        .render(index_of(&host, "agent"), ctx(120, 30))
        .expect("render the agent pane");
    let shell = host
        .render(index_of(&host, "shell"), ctx(120, 16))
        .expect("render the shell pane");
    let agent_surface = agent.node.first_session_surface().expect("agent surface");
    let shell_surface = shell.node.first_session_surface().expect("shell surface");
    assert!(!agent_surface.contains('#'), "{agent_surface}");
    assert_eq!(shell_surface, format!("{agent_surface}#shell"));

    // With the shell on screen below, a Shell tab on the agent pane would put the
    // same terminal in two rects at once — so it goes.
    assert!(
        !words(&agent.node).contains("Shell"),
        "the agent pane still offers a Shell tab: {:?}",
        agent.node
    );
    // And the shell pane opened the companion shell it is showing.
    let kinds: Vec<&str> = host
        .drain_commands()
        .iter()
        .map(|command| command.kind())
        .collect();
    assert!(kinds.contains(&"shell"), "{kinds:?}");
}

#[test]
fn the_agent_keeps_its_shell_tab_wherever_no_shell_pane_is_on_screen() {
    // Narrow, split-shell drops to the agent alone — and the shell has to stay
    // reachable, so the tab comes back.
    let dir = delivered("split-shell");
    let host = split_shell_with_a_selection(dir.path(), 60, 48);
    let agent = host
        .render(index_of(&host, "agent"), ctx(60, 40))
        .expect("render the agent pane");
    assert!(words(&agent.node).contains("Shell"), "{:?}", agent.node);
}

#[test]
fn the_shell_chord_moves_focus_to_the_shell_pane_when_there_is_one() {
    let dir = delivered("split-shell");
    let host = split_shell_with_a_selection(dir.path(), 160, 48);
    host.drain_commands();
    host.on_action(index_of(&host, "agent"), "shell.open")
        .expect("the chord");
    let focus: Vec<String> = host
        .drain_commands()
        .iter()
        .filter(|command| command.kind() == "focus")
        .map(|command| format!("{command:?}"))
        .collect();
    assert!(
        focus.iter().any(|command| command.contains("\"shell\"")),
        "the chord should focus the shell pane: {focus:?}"
    );
}

// ── choosing and switching ────────────────────────────────────────────────

#[test]
fn delivery_keeps_the_chosen_preset_rather_than_reverting_it() {
    let dir = delivered("split-shell");
    let preset = presets::find("split-shell").expect("split-shell");
    let again = bundled::materialize_with(dir.path(), preset);
    assert!(again.preserved.is_empty(), "{again:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("layout.lua")).expect("layout.lua"),
        preset.layout
    );
}

#[test]
fn layout_set_switches_an_untouched_layout_without_a_backup() {
    let root = profile();
    let ui = root.path().join("config/ui");

    let report = cli_json(root.path(), &["layout", "set", "split-shell"]);
    assert_eq!(report["layout"], "split-shell");
    assert!(report["backup"].is_null(), "{report}");
    assert_eq!(
        std::fs::read_to_string(ui.join("layout.lua")).expect("layout.lua"),
        presets::find("split-shell").expect("split-shell").layout
    );
    let settings =
        std::fs::read_to_string(root.path().join("config/settings.toml")).expect("settings");
    assert!(
        settings.contains("layout = \"split-shell\""),
        "the choice is recorded where delivery reads it: {settings}"
    );

    let listed = cli_json(root.path(), &["layout", "list"]);
    assert_eq!(listed["current"], "split-shell");
    assert_eq!(listed["edited"], false);
}

#[test]
fn layout_set_backs_up_a_hand_edited_layout_and_says_so() {
    let root = profile();
    let ui = root.path().join("config/ui");
    cli_json(root.path(), &["layout", "set", "classic"]);
    let mine = "-- my own arrangement\nreturn { children = { { slot = \"center\" } } }\n";
    std::fs::write(ui.join("layout.lua"), mine).expect("edit layout.lua");
    assert_eq!(cli_json(root.path(), &["layout", "list"])["edited"], true);

    let output = cli(root.path(), &["layout", "set", "split-shell"]);
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    let backup = report["backup"].as_str().expect("a backup is named");
    assert_eq!(
        std::fs::read_to_string(backup).expect("the backup exists"),
        mine,
        "the edits survive, word for word"
    );
    assert_eq!(
        std::fs::read_to_string(ui.join("layout.lua")).expect("layout.lua"),
        presets::find("split-shell").expect("split-shell").layout
    );

    // A second switch backs up nothing — the file is ours again — and never
    // overwrites the first backup.
    let second = cli_json(root.path(), &["layout", "set", "classic"]);
    assert!(second["backup"].is_null(), "{second}");
    assert_eq!(std::fs::read_to_string(backup).expect("backup"), mine);
}

#[test]
fn layout_set_refuses_a_preset_that_does_not_exist() {
    let root = profile();
    let output = cli(root.path(), &["layout", "set", "nope"]);
    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("split-shell"), "names the real ones: {text}");
}

#[test]
fn turning_the_shell_pane_off_gives_the_agent_its_shell_tab_back() {
    // `placed.shell` must not outlive the pane it describes: with the shell pane
    // turned off (or deleted) the arrangement stops placing it, and a stale
    // `true` would hide the Shell tab and leave the shell unreachable.
    let dir = delivered("split-shell");
    let mut host = split_shell_with_a_selection(dir.path(), 160, 48);
    assert_eq!(host.shared_bool("placed.shell"), Some(true));

    std::fs::remove_file(dir.path().join("plugins/25_shell.lua")).expect("remove the pane");
    host.reload_from(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);
    let on_screen: std::collections::HashSet<String> = slots(&host, 160, 48)
        .into_iter()
        .map(|(slot, _)| slot)
        .collect();
    assert!(!on_screen.contains("shell"));
    host.note_placed(&on_screen);
    assert_eq!(host.shared_bool("placed.shell"), Some(false));
}
