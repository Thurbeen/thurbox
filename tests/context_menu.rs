//! The context menu: the generic float (`ui/plugins/64_menu.lua`) and the
//! sessions pane that opens it.
//!
//! Driven through the host's hooks with the bundled interface, so what is
//! pinned is the Lua contract — `store.menu` in, `command("action")` out. The
//! road from the terminal is `tests/tui_e2e.rs`'s.

use std::path::{Path, PathBuf};

use ratatui::backend::TestBackend;
use ratatui::Terminal;

use thurbox::kernel::command::Command;
use thurbox::kernel::host::{Click, KeyPress, LuaHost, Published, RenderContext};
use thurbox::kernel::node::Identity;
use thurbox::kernel::paint::{render_recording, PlaceholderSurfaces};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;
use thurbox::session::SessionState;

/// Opens a menu of three entries and a rule at its right press.
const OPENER: &str = r#"
return {
  name = "opener",
  slot = "sessions",
  order = 10,
  keys = {
    { key = "x", action = "opener.first", desc = "first", group = "Test" },
  },
  render = function()
    return { type = "text", text = "opener", id = "opener" }
  end,
  on_context = function(hit)
    store.menu = {
      at = { x = hit.screen_x, y = hit.screen_y },
      items = {
        { label = "First", action = "opener.first" },
        { label = "Second", action = "opener.second" },
        "sep",
        { label = "Third", action = "opener.third" },
      },
    }
    return true
  end,
}
"#;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("read_dir") {
        let entry = entry.expect("entry");
        std::fs::copy(entry.path(), to.join(entry.file_name())).expect("copy");
    }
}

/// The bundled `lib/` and the menu float, plus `OPENER` — nothing else, so no
/// bundled pane can answer for the menu.
fn menu_host() -> (tempfile::TempDir, LuaHost) {
    let home = tempfile::tempdir().expect("tempdir");
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    copy_dir(&source.join("lib"), &home.path().join("lib"));
    std::fs::create_dir_all(home.path().join("plugins")).expect("mkdir");
    std::fs::copy(
        source.join("plugins/64_menu.lua"),
        home.path().join("plugins/64_menu.lua"),
    )
    .expect("copy the menu");
    std::fs::write(home.path().join("plugins/10_opener.lua"), OPENER).expect("write opener");
    let host = LuaHost::new(home.path().to_path_buf());
    assert!(host.error.is_none(), "{:?}", host.error);
    publish(&host, &Snapshot::default());
    (home, host)
}

fn publish(host: &LuaHost, snapshot: &Snapshot) {
    publish_with(host, snapshot, &[]);
}

fn publish_with(
    host: &LuaHost,
    snapshot: &Snapshot,
    inflight: &[thurbox::kernel::command::InFlight],
) {
    publish_hovered(host, snapshot, inflight, None);
}

fn publish_hovered(
    host: &LuaHost,
    snapshot: &Snapshot,
    inflight: &[thurbox::kernel::command::InFlight],
    hovered: Option<&Identity>,
) {
    let themes = Themes::load(None);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    registry.declare_commands(host.commands());
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
        snapshot,
        attach_errors: &Default::default(),
        inflight,
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
        hovered,
        printing: &Default::default(),
    })
    .expect("publish");
}

fn index_of(host: &LuaHost, name: &str) -> usize {
    host.plugins
        .iter()
        .position(|p| p.name == name)
        .unwrap_or_else(|| panic!("{name} should have loaded"))
}

fn ctx() -> RenderContext {
    RenderContext {
        width: 80,
        height: 24,
        focused: true,
        elapsed: 0.0,
        frame: 0,
    }
}

fn right_press_at(x: u16, y: u16, id: Option<&str>) -> Click {
    Click {
        id: id.map(str::to_string),
        role: id.map(|_| "row".to_string()),
        w: 20,
        h: 1,
        screen_x: x,
        screen_y: y,
        clicks: 1,
        ..Click::default()
    }
}

fn key(name: &str) -> KeyPress {
    KeyPress {
        name: name.into(),
        ch: (name.chars().count() == 1).then(|| name.chars().next().unwrap()),
        ..KeyPress::default()
    }
}

/// The menu's text, painted into its own rect, one line per row.
fn menu_text(host: &LuaHost) -> Option<String> {
    let rendered = host.render(index_of(host, "menu"), ctx()).expect("render");
    let float = rendered.float?;
    let (cols, rows) = (float.cols.unwrap_or(40), float.rows.unwrap_or(10));
    let mut hits = Vec::new();
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).expect("terminal");
    terminal
        .draw(|frame| {
            render_recording(
                frame,
                frame.area(),
                &rendered.node,
                &PlaceholderSurfaces,
                &mut hits,
            )
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    Some(
        (0..rows)
            .map(|y| {
                (0..cols)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn actions(host: &LuaHost) -> Vec<String> {
    host.drain_commands()
        .into_iter()
        .filter_map(|command| match command {
            Command::Action { action, .. } => Some(action),
            _ => None,
        })
        .collect()
}

fn open(host: &LuaHost) {
    assert!(host
        .on_context(
            index_of(host, "opener"),
            &right_press_at(12, 5, Some("opener"))
        )
        .expect("context"));
}

// ── the menu float ──────────────────────────────────────────────────────────

#[test]
fn nothing_floats_until_a_menu_is_asked_for() {
    let (_home, host) = menu_host();
    assert!(menu_text(&host).is_none());
}

#[test]
fn the_menu_opens_at_the_press_sized_to_its_entries() {
    let (_home, host) = menu_host();
    open(&host);
    let float = host
        .render(index_of(&host, "menu"), ctx())
        .expect("render")
        .float
        .expect("the menu floats");
    assert_eq!(float.at, Some((12, 5)));
    assert_eq!(float.rows, Some(6), "four entries and two borders");
    let text = menu_text(&host).expect("drawn");
    for label in ["First", "Second", "Third", "─"] {
        assert!(text.contains(label), "{label} missing:\n{text}");
    }
}

/// The hint is read from the registry, so it follows a rebind; an action with
/// no chord shows none.
#[test]
fn an_entry_shows_the_chord_its_action_is_bound_to() {
    let (_home, host) = menu_host();
    open(&host);
    let text = menu_text(&host).expect("drawn");
    let first = text
        .lines()
        .find(|l| l.contains("First"))
        .expect("First row");
    assert!(first.trim_end_matches(['│', ' ']).ends_with('x'), "{first}");
}

/// Closed before the action runs, so an action that opens a float of its own is
/// never drawn under a menu that is still up.
#[test]
fn enter_closes_the_menu_and_runs_the_highlighted_entry() {
    let (_home, host) = menu_host();
    open(&host);
    let menu = index_of(&host, "menu");
    assert!(host.on_key(menu, &key("enter")).expect("key"));
    assert!(menu_text(&host).is_none(), "the menu must be gone");
    assert_eq!(actions(&host), ["opener.first"]);
}

#[test]
fn moving_down_skips_the_rule() {
    let (_home, host) = menu_host();
    open(&host);
    let menu = index_of(&host, "menu");
    host.on_key(menu, &key("down")).expect("key");
    host.on_key(menu, &key("j")).expect("key");
    host.on_key(menu, &key("enter")).expect("key");
    assert_eq!(actions(&host), ["opener.third"]);
}

#[test]
fn moving_past_either_end_stays_put() {
    let (_home, host) = menu_host();
    open(&host);
    let menu = index_of(&host, "menu");
    host.on_key(menu, &key("up")).expect("key");
    host.on_key(menu, &key("k")).expect("key");
    host.on_key(menu, &key("enter")).expect("key");
    assert_eq!(actions(&host), ["opener.first"]);
}

#[test]
fn escape_closes_the_menu_and_runs_nothing() {
    let (_home, host) = menu_host();
    open(&host);
    assert!(host
        .on_key(index_of(&host, "menu"), &key("esc"))
        .expect("key"));
    assert!(menu_text(&host).is_none());
    assert!(actions(&host).is_empty());
}

/// A modal takes every key while it is up; one it does not know is swallowed.
#[test]
fn an_unknown_key_is_swallowed_while_the_menu_is_up() {
    let (_home, host) = menu_host();
    open(&host);
    assert!(host
        .on_key(index_of(&host, "menu"), &key("q"))
        .expect("key"));
    assert!(menu_text(&host).is_some(), "still open");
}

#[test]
fn a_press_elsewhere_closes_the_menu() {
    let (_home, host) = menu_host();
    open(&host);
    assert!(host
        .on_outside(index_of(&host, "menu"), &right_press_at(70, 20, None))
        .expect("outside"));
    assert!(menu_text(&host).is_none());
    assert!(actions(&host).is_empty());
}

#[test]
fn clicking_an_entry_runs_it() {
    let (_home, host) = menu_host();
    open(&host);
    let menu = index_of(&host, "menu");
    let click = Click {
        id: Some("menu-2".into()),
        role: Some("row".into()),
        clicks: 1,
        ..Click::default()
    };
    assert!(host.on_click(menu, &click).expect("click"));
    assert_eq!(actions(&host), ["opener.second"]);
    assert!(menu_text(&host).is_none());
}

/// The background of each menu row's first label cell, with the pointer over
/// `hovered`.
fn row_backgrounds(host: &LuaHost, hovered: Option<&Identity>) -> Vec<ratatui::style::Color> {
    publish_hovered(host, &Snapshot::default(), &[], hovered);
    let rendered = host.render(index_of(host, "menu"), ctx()).expect("render");
    let float = rendered.float.expect("the menu floats");
    let (cols, rows) = (float.cols.unwrap_or(40), float.rows.unwrap_or(10));
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).expect("terminal");
    terminal
        .draw(|frame| {
            render_recording(
                frame,
                frame.area(),
                &rendered.node,
                &PlaceholderSurfaces,
                &mut Vec::new(),
            )
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    (1..rows - 1).map(|y| buffer[(2, y)].bg).collect()
}

/// The pointer bands the entry it is over, the one a click would run, and
/// leaves the highlighted entry and the rest as they were.
#[test]
fn a_hovered_entry_is_banded_and_the_others_are_not() {
    let (_home, host) = menu_host();
    open(&host);
    let resting = row_backgrounds(&host, None);
    let second = Identity {
        id: Some("menu-2".into()),
        role: Some("row".into()),
        ..Identity::default()
    };
    let lit = row_backgrounds(&host, Some(&second));
    assert_ne!(resting[1], lit[1], "the hovered entry is banded");
    assert_eq!(resting[0], lit[0], "the highlighted entry keeps its bar");
    assert_eq!(resting[3], lit[3], "an entry not pointed at is untouched");
}

/// The rule, and the frame around the entries, are part of the menu: a press
/// on them is the menu's and does nothing.
#[test]
fn clicking_the_rule_does_nothing() {
    let (_home, host) = menu_host();
    open(&host);
    assert!(host
        .on_click(index_of(&host, "menu"), &Click::default())
        .expect("click"));
    assert!(actions(&host).is_empty());
    assert!(menu_text(&host).is_some(), "still open");
}

// ── the sessions pane opens it ──────────────────────────────────────────────

fn row(name: &str, repo: &str) -> SessionRow {
    SessionRow {
        id: format!("{name}-0000-0000-0000-000000000000"),
        name: name.into(),
        agent: "claude".into(),
        status: SessionState::Idle,
        cwd: Some(PathBuf::from(format!("/src/{repo}"))),
        repo: Some(repo.into()),
        repos: Vec::new(),
        branch: Some("main".into()),
        base_branch: None,
        backend: "local-tmux".into(),
        backend_id: Some("%1".into()),
        remote_host: None,
        agent_session_id: None,
        parent_id: None,
        display_order: None,
        worktree_count: 0,
        git: None,
        stopped: false,
        hook_state: None,
        reports_as: None,
        detected_agent: None,
        shell_backend_id: None,
        member_dirs: Vec::new(),
    }
}

fn two_sessions() -> Snapshot {
    Snapshot {
        sessions: vec![row("alpha", "thurbox"), row("beta", "website")],
        ..Snapshot::default()
    }
}

/// The bundled interface, published two sessions and rendered once.
fn sessions_host() -> LuaHost {
    let host = LuaHost::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui"));
    assert!(host.error.is_none(), "{:?}", host.error);
    publish(&host, &two_sessions());
    host.render(index_of(&host, "sessions"), ctx())
        .expect("render");
    host
}

#[test]
fn a_right_press_on_a_session_opens_its_menu_at_the_pointer() {
    let host = sessions_host();
    let beta = two_sessions().sessions[1].id.clone();
    assert!(host
        .on_context(
            index_of(&host, "sessions"),
            &right_press_at(7, 4, Some(&beta))
        )
        .expect("context"));
    let float = host
        .render(index_of(&host, "menu"), ctx())
        .expect("render")
        .float
        .expect("the menu floats");
    assert_eq!(float.at, Some((7, 4)));
    let text = menu_text(&host).expect("drawn");
    for label in [
        "Open",
        "Rename",
        "Fork",
        "Open in editor",
        "Restart",
        "Sync",
        "Move up",
        "Move down",
        "Delete",
        "Delete + worktree",
    ] {
        assert!(text.contains(label), "{label} missing:\n{text}");
    }
    assert!(!text.contains("Sort"), "sort targets no session:\n{text}");
}

/// The entries run the pane's own actions on the SELECTED session, so the
/// press has to select the row it landed on.
#[test]
fn the_right_press_selects_the_session_it_landed_on() {
    let host = sessions_host();
    let beta = two_sessions().sessions[1].id.clone();
    host.on_context(
        index_of(&host, "sessions"),
        &right_press_at(7, 4, Some(&beta)),
    )
    .expect("context");
    // Down once, from Open, is Rename; its action opens the rename float for
    // the selected session once the coordinator runs it.
    let menu = index_of(&host, "menu");
    host.on_key(menu, &key("down")).expect("key");
    host.on_key(menu, &key("enter")).expect("key");
    assert_eq!(actions(&host), ["sessions.rename"]);
    assert!(host
        .on_action(index_of(&host, "sessions"), "sessions.rename")
        .expect("action"));
    let rename = host
        .render(index_of(&host, "rename"), ctx())
        .expect("render");
    assert!(rename.float.is_some(), "the rename float opens");
    assert!(
        format!("{:?}", rename.node).contains("beta"),
        "…for beta, the row pressed"
    );
}

/// Empty space — and a repo header, which carries no id either — is about no
/// session, so it opens the pane's own menu of general actions.
#[test]
fn a_right_press_off_the_rows_opens_the_panes_menu() {
    let host = sessions_host();
    assert!(host
        .on_context(index_of(&host, "sessions"), &right_press_at(9, 20, None))
        .expect("context"));
    let float = host
        .render(index_of(&host, "menu"), ctx())
        .expect("render")
        .float
        .expect("the menu floats");
    assert_eq!(float.at, Some((9, 20)));
    let text = menu_text(&host).expect("drawn");
    for label in [
        "New session",
        "Restore deleted",
        "Sort by name",
        "Hide panel",
    ] {
        assert!(text.contains(label), "{label} missing:\n{text}");
    }
    for row_only in ["Rename", "Delete"] {
        assert!(
            !text.contains(row_only),
            "{row_only} is a row's entry:\n{text}"
        );
    }
    assert!(
        !text.contains("Undo delete"),
        "nothing to undo yet:\n{text}"
    );
}

#[test]
fn new_session_from_the_panes_menu_opens_the_creation_flow() {
    let host = sessions_host();
    host.on_context(index_of(&host, "sessions"), &right_press_at(9, 20, None))
        .expect("context");
    host.on_key(index_of(&host, "menu"), &key("enter"))
        .expect("key");
    assert_eq!(actions(&host), ["new_session.open"]);
}

/// Offered only when there is something to undo, like every entry here.
#[test]
fn undo_delete_is_offered_once_a_session_was_deleted() {
    let host = sessions_host();
    let sessions = index_of(&host, "sessions");
    host.on_action(sessions, "sessions.delete").expect("delete");
    host.drain_commands();
    host.on_context(sessions, &right_press_at(9, 20, None))
        .expect("context");
    let text = menu_text(&host).expect("drawn");
    assert!(text.contains("Undo delete"), "{text}");
}

#[test]
fn an_empty_list_offers_no_sort() {
    let host = LuaHost::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui"));
    publish(&host, &Snapshot::default());
    host.render(index_of(&host, "sessions"), ctx())
        .expect("render");
    assert!(host
        .on_context(index_of(&host, "sessions"), &right_press_at(9, 20, None))
        .expect("context"));
    let text = menu_text(&host).expect("drawn");
    assert!(text.contains("New session"), "{text}");
    assert!(!text.contains("Sort by name"), "nothing to sort:\n{text}");
}

#[test]
fn a_right_press_on_an_empty_list_opens_nothing() {
    let host = LuaHost::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui"));
    publish(&host, &Snapshot::default());
    host.render(index_of(&host, "sessions"), ctx())
        .expect("render");
    assert!(!host
        .on_context(
            index_of(&host, "sessions"),
            &right_press_at(7, 4, Some("gone"))
        )
        .expect("context"));
    assert!(menu_text(&host).is_none());
}

// ── review follow-ups ───────────────────────────────────────────────────────

/// The node carrying `id`, anywhere in the tree.
fn node_with_id<'a>(
    node: &'a thurbox::kernel::node::Node,
    id: &str,
) -> Option<&'a thurbox::kernel::node::Node> {
    use thurbox::kernel::node::Node;
    match node {
        Node::Text { identity, .. } if identity.id.as_deref() == Some(id) => Some(node),
        Node::Box { children, .. } => children.iter().find_map(|child| node_with_id(child, id)),
        _ => None,
    }
}

/// `ui/AGENTS.md`: a highlight is a node style, so the bar covers the row and a
/// run that names its own colour (the chord hint) keeps it.
#[test]
fn the_highlight_is_the_row_nodes_style_not_its_runs() {
    use thurbox::kernel::node::Node;
    let (_home, host) = menu_host();
    open(&host);
    let rendered = host.render(index_of(&host, "menu"), ctx()).expect("render");
    let Some(Node::Text { style, lines, .. }) = node_with_id(&rendered.node, "menu-1") else {
        panic!("the highlighted row is a text node");
    };
    assert!(style.bg.is_some(), "the bar is the node's background");
    assert!(
        lines.iter().flatten().all(|run| run.style.bg.is_none()),
        "no run paints the bar itself: {lines:?}"
    );
}

/// On a screen shorter than the menu the entries scroll, so the one `enter`
/// would run is always on screen.
#[test]
fn a_menu_taller_than_the_screen_keeps_the_highlight_in_view() {
    let (_home, host) = menu_host();
    open(&host);
    let menu = index_of(&host, "menu");
    host.on_key(menu, &key("down")).expect("key");
    host.on_key(menu, &key("down")).expect("key");
    let short = RenderContext { height: 5, ..ctx() };
    let rendered = host.render(menu, short).expect("render");
    let float = rendered.float.expect("floats");
    assert!(float.rows.unwrap_or(0) <= 5, "fits the screen: {float:?}");
    let tree = format!("{:?}", rendered.node);
    assert!(
        tree.contains("Third"),
        "the highlighted entry is drawn: {tree}"
    );
    assert!(
        !tree.contains("First"),
        "the window scrolled past the top: {tree}"
    );
}

/// The entries act on the session that was pressed, not on whatever row the
/// cursor fell back to: if that session is gone, nothing runs.
#[test]
fn a_menu_entry_does_not_act_on_a_session_other_than_the_one_pressed() {
    let host = sessions_host();
    let sessions = index_of(&host, "sessions");
    let beta = two_sessions().sessions[1].id.clone();
    host.on_context(sessions, &right_press_at(7, 4, Some(&beta)))
        .expect("context");

    // beta disappears (another instance deleted it) while its menu is up.
    publish(
        &host,
        &Snapshot {
            sessions: vec![row("alpha", "thurbox")],
            ..Snapshot::default()
        },
    );
    host.render(sessions, ctx()).expect("render");

    // Delete + worktree is the last entry.
    let menu = index_of(&host, "menu");
    for _ in 0..12 {
        host.on_key(menu, &key("down")).expect("key");
    }
    host.on_key(menu, &key("enter")).expect("key");
    assert_eq!(actions(&host), ["sessions.force_delete"]);
    // What the coordinator does with that command.
    host.on_action(sessions, "sessions.force_delete")
        .expect("action");
    let issued = host.drain_commands();
    assert!(
        !issued.iter().any(|c| c.kind() == "delete"),
        "alpha must not be deleted in beta's place: {issued:?}"
    );
    assert!(
        host.render(index_of(&host, "confirm"), ctx())
            .expect("render")
            .float
            .is_none(),
        "nor asked about"
    );
}

/// And while the pressed session is still there, its entry acts on it even if
/// the cursor has moved since.
#[test]
fn a_menu_entry_acts_on_the_pressed_session_after_the_cursor_moved() {
    let host = sessions_host();
    let sessions = index_of(&host, "sessions");
    let beta = two_sessions().sessions[1].id.clone();
    host.on_context(sessions, &right_press_at(7, 4, Some(&beta)))
        .expect("context");
    host.on_key(index_of(&host, "menu"), &key("down"))
        .expect("key");
    host.on_key(index_of(&host, "menu"), &key("enter"))
        .expect("key");
    assert_eq!(actions(&host), ["sessions.rename"]);
    // The cursor moves before the action lands.
    host.on_action(sessions, "sessions.first").expect("action");
    host.on_action(sessions, "sessions.rename").expect("action");
    let rename = host
        .render(index_of(&host, "rename"), ctx())
        .expect("render");
    assert!(
        format!("{:?}", rename.node).contains("beta"),
        "renames beta, the row pressed"
    );
}

/// A creation in flight draws a placeholder row, but a placeholder is not a
/// session: there is still nothing to sort.
#[test]
fn a_creation_in_flight_is_not_something_to_sort() {
    use thurbox::kernel::command::{InFlight, Phase};
    let host = LuaHost::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui"));
    publish_with(
        &host,
        &Snapshot::default(),
        &[InFlight {
            id: 1,
            kind: "create",
            session: String::new(),
            subject: Some("thurbox".to_string()),
            host: None,
            phase: Phase::Running,
            error: None,
        }],
    );
    host.render(index_of(&host, "sessions"), ctx())
        .expect("render");
    host.on_context(index_of(&host, "sessions"), &right_press_at(9, 20, None))
        .expect("context");
    let text = menu_text(&host).expect("drawn");
    assert!(
        !text.contains("Sort by name"),
        "a placeholder is not a session:\n{text}"
    );
}

// ── entries another plugin contributes ──────────────────────────────────────

/// A plugin that owns a per-session action and offers it in the sessions
/// menu. `extra.toggle` is a palette command with no chord, so it is declared
/// only in `commands`; `extra.ghost` is declared nowhere.
const CONTRIBUTOR: &str = r#"
-- Offered at load, which runs again on every reload: rewriting the same list
-- under the same name is idempotent.
local extra = store["sessions.menu_extra"] or {}
extra.extra = {
  { label = "Auto-continue", action = "extra.toggle" },
  "sep",
  { label = "Ghost", action = "extra.ghost" },
}
store["sessions.menu_extra"] = extra

return {
  name = "extra",
  slot = "float",
  floats = true,
  focusable = false,
  commands = {
    { action = "extra.toggle", desc = "Toggle for the pressed session" },
    { action = "extra.mislabel", desc = "Contribute an entry whose label is not text" },
  },
  render = function()
    return { type = "text", text = "" }
  end,
  on_action = function(action)
    if action == "extra.mislabel" then
      local extra = store["sessions.menu_extra"] or {}
      extra.mislabelled = { { label = { "not text" }, action = "extra.toggle" } }
      store["sessions.menu_extra"] = extra
      return true
    elseif action ~= "extra.toggle" then
      return false
    end
    local chosen = store["menu.chosen"]
    store["menu.chosen"] = nil
    command("message", { text = "toggled " .. tostring(chosen and chosen.target) })
    return true
  end,
}
"#;

/// The bundled interface plus `CONTRIBUTOR`, published two sessions.
fn contributed_host() -> (tempfile::TempDir, LuaHost) {
    let home = tempfile::tempdir().expect("tempdir");
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    copy_dir(&source.join("lib"), &home.path().join("lib"));
    copy_dir(&source.join("plugins"), &home.path().join("plugins"));
    std::fs::copy(source.join("layout.lua"), home.path().join("layout.lua")).expect("layout");
    std::fs::write(home.path().join("plugins/90_extra.lua"), CONTRIBUTOR).expect("write");
    let host = LuaHost::new(home.path().to_path_buf());
    assert!(host.error.is_none(), "{:?}", host.error);
    publish(&host, &two_sessions());
    host.render(index_of(&host, "sessions"), ctx())
        .expect("render");
    (home, host)
}

fn messages(host: &LuaHost) -> Vec<String> {
    host.drain_commands()
        .into_iter()
        .filter_map(|command| match command {
            Command::Message { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

#[test]
fn a_contributed_entry_is_offered_on_a_session_and_an_undeclared_one_is_not() {
    let (_home, host) = contributed_host();
    let beta = two_sessions().sessions[1].id.clone();
    host.on_context(
        index_of(&host, "sessions"),
        &right_press_at(7, 4, Some(&beta)),
    )
    .expect("context");
    let text = menu_text(&host).expect("drawn");
    assert!(text.contains("Auto-continue"), "{text}");
    assert!(
        text.contains("Delete + worktree"),
        "the pane's own stay:\n{text}"
    );
    assert!(
        !text.contains("Ghost"),
        "extra.ghost is declared nowhere:\n{text}"
    );
    let last = text.lines().rev().nth(1).unwrap_or_default();
    assert!(
        last.contains("Auto-continue"),
        "contributions come after the pane's own entries, and the rule the \
         dropped entry would have needed goes with it:\n{text}"
    );
}

#[test]
fn a_contributed_entry_runs_on_the_session_pressed_not_the_selection() {
    let (_home, host) = contributed_host();
    let sessions = index_of(&host, "sessions");
    let beta = two_sessions().sessions[1].id.clone();
    host.on_context(sessions, &right_press_at(7, 4, Some(&beta)))
        .expect("context");
    let menu = index_of(&host, "menu");
    for _ in 0..20 {
        host.on_key(menu, &key("down")).expect("key");
    }
    host.on_key(menu, &key("enter")).expect("key");
    assert_eq!(actions(&host), ["extra.toggle"]);
    // The cursor moves back to alpha before the action lands.
    host.on_action(sessions, "sessions.first").expect("action");
    host.drain_commands();
    assert!(host
        .on_action(index_of(&host, "extra"), "extra.toggle")
        .expect("action"));
    assert_eq!(messages(&host), [format!("toggled {beta}")]);
}

#[test]
fn contributions_are_not_offered_off_the_rows() {
    let (_home, host) = contributed_host();
    host.on_context(index_of(&host, "sessions"), &right_press_at(9, 20, None))
        .expect("context");
    let text = menu_text(&host).expect("drawn");
    assert!(!text.contains("Auto-continue"), "{text}");
}

/// One contributor's malformed label must not cost the row menu: the entry
/// falls back to its action's name, as `64_menu` draws an unlabelled one.
#[test]
fn a_contributed_label_that_is_not_text_falls_back_to_its_action() {
    let (_home, host) = contributed_host();
    assert!(host
        .on_action(index_of(&host, "extra"), "extra.mislabel")
        .expect("mislabel"));
    let beta = two_sessions().sessions[1].id.clone();
    host.on_context(
        index_of(&host, "sessions"),
        &right_press_at(7, 4, Some(&beta)),
    )
    .expect("context");
    let text = menu_text(&host).expect("the row menu still draws");
    assert!(text.contains("Open"), "{text}");
    assert!(text.contains("extra.toggle"), "{text}");
}
