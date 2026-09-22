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
    // The pane asks for nothing from its render — painting its surface is what
    // opens the shell (`Terminals::take_wanted_shells`) — so it can be pure.
    assert!(host.plugins[index_of(&host, "shell")].pure);
    let kinds: Vec<&str> = host
        .drain_commands()
        .iter()
        .map(|command| command.kind())
        .collect();
    assert!(!kinds.contains(&"shell"), "{kinds:?}");
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

// ── four presets ──────────────────────────────────────────────────────────

#[test]
fn four_presets_ship_with_classic_first() {
    let names: Vec<&str> = presets::PRESETS.iter().map(|preset| preset.name).collect();
    assert_eq!(names, ["classic", "split-shell", "focus", "ide"]);
}

/// A third-party column the way an installed plugin brings one — a slot no
/// bundled pane fills, with its own panel toggle — so the presets can be asked
/// where they put a pane they have never heard of.
fn with_a_third_party_column(dir: &Path, slot: &str) {
    std::fs::write(
        dir.join(format!("plugins/90_{slot}.lua")),
        format!(
            "local panels = require(\"lib.panels\")\n\
             return {{\n  name = \"{slot}\",\n  slot = \"{slot}\",\n  \
             render = function() return {{ type = \"text\", text = \"{slot}\" }} end,\n  \
             on_action = function(action)\n    \
             if action == \"{slot}.toggle\" then panels.toggle(\"{slot}\") return true end\n    \
             return false\n  end,\n}}\n"
        ),
    )
    .expect("write the third-party pane");
}

fn toggle_sessions(host: &LuaHost) {
    host.on_action(index_of(host, "sessions"), "sessions.toggle_panel")
        .expect("F9");
}

// ── focus ─────────────────────────────────────────────────────────────────

#[test]
fn focus_gives_the_agent_the_whole_width_and_f9_brings_the_list_back() {
    let dir = delivered("focus");
    let host = host_at(dir.path());
    publish(&host);

    let wide = slots(&host, 160, 48);
    let center = rect_of(&wide, "center").expect("the agent pane");
    assert_eq!(center.width, 160, "{wide:?}");
    assert!(rect_of(&wide, "sessions").is_none(), "{wide:?}");
    assert!(rect_of(&wide, "shell").is_none(), "{wide:?}");

    // One press, not two: the list starts hidden here, and F9 is the same toggle
    // every other preset uses.
    toggle_sessions(&host);
    let shown = slots(&host, 160, 48);
    let sessions = rect_of(&shown, "sessions").expect("F9 brings the list back");
    assert!(sessions.x < rect_of(&shown, "center").expect("center").x);
    toggle_sessions(&host);
    assert!(rect_of(&slots(&host, 160, 48), "sessions").is_none());

    let narrow = slots(&host, 60, 48);
    assert_eq!(rect_of(&narrow, "center").expect("center").width, 60);
}

#[test]
fn focus_keeps_third_party_columns_closed_until_their_toggle_opens_them() {
    let dir = delivered("focus");
    with_a_third_party_column(dir.path(), "files");
    let host = host_at(dir.path());
    publish(&host);

    assert!(rect_of(&slots(&host, 200, 50), "files").is_none());
    host.on_action(index_of(&host, "files"), "files.toggle")
        .expect("the pane's toggle");
    let shown = slots(&host, 200, 50);
    let files = rect_of(&shown, "files").expect("its toggle brings it back");
    assert!(files.x > rect_of(&shown, "center").expect("center").x);
}

#[test]
fn focus_leaves_the_shell_a_tab_of_the_agent() {
    let dir = delivered("focus");
    let host = split_shell_with_a_selection(dir.path(), 160, 48);
    let agent = host
        .render(index_of(&host, "agent"), ctx(160, 46))
        .expect("render the agent pane");
    assert!(words(&agent.node).contains("Shell"), "{:?}", agent.node);
}

// ── ide ───────────────────────────────────────────────────────────────────

#[test]
fn ide_puts_sessions_left_and_the_shell_along_the_bottom_with_no_empty_right_column() {
    let dir = delivered("ide");
    let host = host_at(dir.path());
    publish(&host);

    let wide = slots(&host, 200, 50);
    let sessions = rect_of(&wide, "sessions").expect("the list on the left");
    let center = rect_of(&wide, "center").expect("the agent pane");
    let shell = rect_of(&wide, "shell").expect("the shell panel");
    assert_eq!(sessions.x, 0);
    assert!(center.x >= sessions.x + sessions.width);
    assert_eq!(shell.x, center.x, "the panel sits under the agent");
    assert_eq!(shell.width, center.width);
    assert_eq!(shell.y, center.y + center.height);
    // Nothing fills a right-hand slot, so nothing is reserved for one: the agent
    // runs to the right edge.
    assert_eq!(center.x + center.width, 200, "{wide:?}");
}

#[test]
fn ide_gives_filled_right_hand_panes_a_column_and_drops_it_before_the_rest() {
    let dir = delivered("ide");
    with_a_third_party_column(dir.path(), "files");
    with_a_third_party_column(dir.path(), "fleetqueue");
    let host = host_at(dir.path());
    publish(&host);

    let wide = slots(&host, 200, 50);
    let center = rect_of(&wide, "center").expect("center");
    let shell = rect_of(&wide, "shell").expect("shell");
    let files = rect_of(&wide, "files").expect("a filled right-hand slot is placed");
    let queue = rect_of(&wide, "fleetqueue").expect("so is every other one");
    assert_eq!(files.x, center.x + center.width, "right of the agent");
    assert_eq!(files.x, queue.x, "stacked in the one right column");
    assert_eq!(files.width, queue.width);
    assert_eq!(
        shell.x + shell.width,
        files.x,
        "the panel stops at the column"
    );

    // One press hides it: the column starts open in this preset, and the pane's
    // own toggle agrees with that.
    host.on_action(index_of(&host, "files"), "files.toggle")
        .expect("the pane's toggle");
    assert!(rect_of(&slots(&host, 200, 50), "files").is_none());

    // Below `three_panel_min_cols` the right column goes first; below
    // `two_panel_min_cols` the agent is alone.
    let middle = slots(&host, 100, 50);
    assert!(rect_of(&middle, "fleetqueue").is_none(), "{middle:?}");
    assert!(rect_of(&middle, "sessions").is_some(), "{middle:?}");
    assert!(rect_of(&middle, "shell").is_some(), "{middle:?}");
    let narrow = slots(&host, 60, 50);
    assert!(rect_of(&narrow, "center").is_some());
    for gone in ["sessions", "shell", "fleetqueue", "files"] {
        assert!(rect_of(&narrow, gone).is_none(), "{gone}: {narrow:?}");
    }
}

#[test]
fn ide_drops_the_shell_panel_on_a_short_screen_and_the_tab_comes_back() {
    let dir = delivered("ide");
    let host = split_shell_with_a_selection(dir.path(), 160, 16);
    assert!(rect_of(&slots(&host, 160, 16), "shell").is_none());
    let agent = host
        .render(index_of(&host, "agent"), ctx(120, 14))
        .expect("render the agent pane");
    assert!(words(&agent.node).contains("Shell"), "{:?}", agent.node);
}

/// A host arranged at `width`×`height` with the placement recorded, but with
/// nothing rendered yet — so no pane has published a selection.
fn arranged(dir: &Path, width: u16, height: u16) -> LuaHost {
    let host = host_at(dir);
    publish(&host);
    let on_screen: std::collections::HashSet<String> = slots(&host, width, height)
        .into_iter()
        .map(|(slot, _)| slot)
        .collect();
    host.note_placed(&on_screen);
    host
}

#[test]
fn with_the_list_hidden_the_agent_pane_still_shows_and_selects_a_session() {
    // The list owns the selection and writes it from its render, so a layout
    // that starts it hidden would otherwise show "no session" until F9.
    let dir = delivered("focus");
    let host = arranged(dir.path(), 160, 48);
    let agent = host
        .render(index_of(&host, "agent"), ctx(160, 46))
        .expect("render the agent pane");
    let surface = agent
        .node
        .first_session_surface()
        .expect("the agent shows a session");
    assert_eq!(host.shared_string("selected").as_deref(), Some(surface));
}

#[test]
fn with_the_list_hidden_a_focus_request_still_lands() {
    // A clicked notification or `thurbox-cli session focus` leaves a one-shot
    // request the list consumes; with the list off screen the agent pane must.
    let dir = delivered("focus");
    let host = arranged(dir.path(), 160, 48);
    let beta = row("beta").id;
    host.set_shared_string("focus_session", &beta);
    let agent = host
        .render(index_of(&host, "agent"), ctx(160, 46))
        .expect("render the agent pane");
    assert_eq!(agent.node.first_session_surface(), Some(beta.as_str()));
    assert_eq!(
        host.shared_string("selected").as_deref(),
        Some(beta.as_str())
    );
    assert_eq!(
        host.shared_string("focus_session"),
        None,
        "spent, not replayed"
    );
}

// ── defects found running the first split-shell ───────────────────────────

/// A third-party column in the one spot every preset but `classic` can place:
/// installed, never mentioned by the layout.
#[test]
fn split_shell_gives_third_party_columns_a_right_hand_column() {
    let dir = delivered("split-shell");
    with_a_third_party_column(dir.path(), "files");
    with_a_third_party_column(dir.path(), "fleetqueue");
    let host = host_at(dir.path());
    publish(&host);

    let wide = slots(&host, 200, 50);
    let center = rect_of(&wide, "center").expect("center");
    let files = rect_of(&wide, "files").expect("placed, not dropped");
    let queue = rect_of(&wide, "fleetqueue").expect("placed, not dropped");
    assert_eq!(files.x, queue.x, "one right-hand column: {wide:?}");
    assert!(files.x >= center.x + center.width, "{wide:?}");
    let unplaced = host
        .unplaced_slots(thurbox::kernel::layout::REFERENCE)
        .expect("resolves");
    assert!(unplaced.is_empty(), "{unplaced:?}");
}

#[test]
fn every_preset_but_classic_places_third_party_columns() {
    // `classic` is the file as it shipped; a column there is placed by hand.
    for preset in ["split-shell", "focus", "ide"] {
        let dir = delivered(preset);
        with_a_third_party_column(dir.path(), "files");
        let host = host_at(dir.path());
        publish(&host);
        let unplaced = host
            .unplaced_slots(thurbox::kernel::layout::REFERENCE)
            .expect("resolves");
        assert!(unplaced.is_empty(), "{preset}: {unplaced:?}");
    }
}

#[test]
fn with_the_list_hidden_by_f9_a_focus_request_still_lands_under_classic() {
    // Not only `focus`: F9 hides the list anywhere, and a clicked notification
    // or `thurbox-cli session focus` was held until the list came back.
    let dir = delivered("classic");
    let host = host_at(dir.path());
    publish(&host);
    host.render(index_of(&host, "sessions"), ctx(40, 12))
        .expect("render the list");
    toggle_sessions(&host);
    let on_screen: std::collections::HashSet<String> = slots(&host, 160, 48)
        .into_iter()
        .map(|(slot, _)| slot)
        .collect();
    assert!(!on_screen.contains("sessions"));
    host.note_placed(&on_screen);

    let beta = row("beta").id;
    host.set_shared_string("focus_session", &beta);
    let agent = host
        .render(index_of(&host, "agent"), ctx(160, 46))
        .expect("render the agent pane");
    assert_eq!(agent.node.first_session_surface(), Some(beta.as_str()));
}

fn focused(width: u16, height: u16) -> RenderContext {
    RenderContext {
        focused: true,
        ..ctx(width, height)
    }
}

fn note_arranged(host: &LuaHost, width: u16, height: u16) {
    let on_screen: std::collections::HashSet<String> = slots(host, width, height)
        .into_iter()
        .map(|(slot, _)| slot)
        .collect();
    host.note_placed(&on_screen);
}

#[test]
fn a_focused_shell_pane_that_leaves_the_screen_hands_its_shell_to_the_agent_pane() {
    // Narrowing took the shell pane away and focus fell to the agent pane —
    // showing the agent, so the next line meant for the shell reached the agent.
    for preset in ["split-shell", "ide"] {
        let dir = delivered(preset);
        let host = split_shell_with_a_selection(dir.path(), 160, 48);
        host.render(index_of(&host, "shell"), focused(120, 16))
            .expect("render the focused shell pane");

        note_arranged(&host, 60, 48);
        let agent = host
            .render(index_of(&host, "agent"), focused(60, 46))
            .expect("render the agent pane");
        let surface = agent.node.first_session_surface().expect("a surface");
        assert!(
            surface.ends_with("#shell"),
            "{preset}: the agent pane shows the shell: {surface}"
        );
    }
}

#[test]
fn the_shell_tab_hands_the_keyboard_to_a_shell_pane_that_appears() {
    // The other direction: typing on the agent's Shell tab, then widening, gave
    // the agent pane its own view back and left the keyboard with the agent.
    let dir = delivered("split-shell");
    let host = split_shell_with_a_selection(dir.path(), 60, 48);
    host.on_action(index_of(&host, "agent"), "shell.open")
        .expect("the chord opens the Shell tab");
    host.drain_commands();

    note_arranged(&host, 160, 48);
    let agent = host
        .render(index_of(&host, "agent"), focused(120, 30))
        .expect("render the agent pane");
    let surface = agent.node.first_session_surface().expect("a surface");
    assert!(!surface.ends_with("#shell"), "{surface}");
    let focus: Vec<String> = host
        .drain_commands()
        .iter()
        .filter(|command| command.kind() == "focus")
        .map(|command| format!("{command:?}"))
        .collect();
    assert!(
        focus.iter().any(|command| command.contains("\"shell\"")),
        "focus follows the shell into its pane: {focus:?}"
    );
}

#[test]
fn a_chosen_preset_an_edited_layout_keeps_out_of_force_is_said_at_start() {
    // settings.toml named split-shell while an edited layout.lua stayed on
    // screen, and nothing said the choice was not in force.
    let dir = delivered("classic");
    assert_eq!(presets::not_in_force(dir.path(), "split-shell"), None);
    std::fs::write(
        dir.path().join("layout.lua"),
        "-- mine\nreturn function() return { children = { { slot = \"center\" } } } end\n",
    )
    .expect("edit layout.lua");
    let note = presets::not_in_force(dir.path(), "split-shell").expect("a note");
    assert!(note.contains("thurbox-cli layout set split-shell"), "{note}");
    assert!(!note.contains("  "), "one line, no run of spaces: {note}");
    assert_eq!(presets::not_in_force(dir.path(), "classic"), None);
    assert_eq!(presets::not_in_force(dir.path(), "nope"), None);
}
