//! The centre pane, after agent and shell became two tabs of ONE plugin.
//!
//! v1's centre is a single pane with a tab strip on its border (`CentralTab`).
//! v2 first modelled the two views as two plugins taking turns in the `switch`
//! slot, which cost the strip on every tab but the agent's, a second stop in
//! the focus ring, and a slot arbitration that refereed nothing else
//! (v2-system-modals D4). These assert the shape that replaced it, against the
//! *real* bundled plugins — so a plugin edit that loses a tab fails here.

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;

use thurbox::kernel::host::{KeyPress, LuaHost, Published, RenderContext};
use thurbox::kernel::node::{ClickVerb, Node};
use thurbox::kernel::paint::{render, render_recording, Hit, PlaceholderSurfaces};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;

/// The plugin under test. It keeps its `agent` name: the name is what
/// `command("focus", …)`, the footer's focus label and the tests below spell,
/// and renaming it is a separate edit from merging the views.
const TERMINAL: &str = "agent";

fn host() -> LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

fn row(name: &str) -> SessionRow {
    SessionRow {
        id: format!("{name}-0000-0000-0000-000000000000"),
        name: name.into(),
        agent: "claude".into(),
        status: "idle".into(),
        cwd: None,
        repo: Some("thurbox".into()),
        repos: vec!["thurbox".into()],
        branch: Some(format!("feat/{name}")),
        backend: "local-tmux".into(),
        backend_id: Some("%1".into()),
        remote_host: None,
        agent_session_id: None,
        parent_id: None,
        display_order: None,
        worktree_count: 1,
        git: None,
        hook_state: None,
        shell_backend_id: None,
        member_dirs: Vec::new(),
    }
}

fn sample() -> Snapshot {
    Snapshot {
        sessions: vec![row("alpha"), row("beta")],
        ..Snapshot::default()
    }
}

fn registry(host: &LuaHost) -> Registry {
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    registry
}

fn publish(host: &LuaHost) {
    let themes = Themes::load(None);
    let registry = registry(host);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        snapshot: &sample(),
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
        hovered: None,
    })
    .expect("publish");
}

fn ctx(width: u16, height: u16) -> RenderContext {
    RenderContext {
        width,
        height,
        focused: true,
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

/// Publish, then let the session list choose a session — the terminal reads its
/// choice out of `store`, exactly as it does in the binary.
fn with_a_selection() -> LuaHost {
    let host = host();
    publish(&host);
    host.render(index_of(&host, "sessions"), ctx(40, 12))
        .expect("render the list");
    host
}

fn tree(host: &LuaHost, width: u16, height: u16) -> Node {
    host.render(index_of(host, TERMINAL), ctx(width, height))
        .expect("render the terminal pane")
        .node
}

fn buffer(node: &Node, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| render(frame, frame.area(), node, &PlaceholderSurfaces))
        .expect("draw");
    terminal.backend().buffer().clone()
}

fn painted(node: &Node, width: u16, height: u16) -> Vec<String> {
    let buffer = buffer(node, width, height);
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

fn hits(node: &Node, width: u16, height: u16) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| render_recording(frame, frame.area(), node, &PlaceholderSurfaces, &mut hits))
        .expect("draw");
    hits
}

/// Which column a label starts in, in cells rather than bytes.
fn column_of(line: &str, needle: &str) -> u16 {
    let at = line
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} is not on the border: {line}"));
    u16::try_from(line[..at].chars().count()).expect("a column")
}

fn session_surface(node: &Node) -> String {
    node.first_session_surface()
        .expect("the pane places a session surface")
        .to_string()
}

// ── one plugin, one focus stop ────────────────────────────────────────────

#[test]
fn the_centre_holds_one_terminal_plugin_rather_than_two() {
    let host = host();
    let names: Vec<&str> = host.plugins.iter().map(|p| p.name.as_str()).collect();
    assert!(
        !names.contains(&"shell"),
        "the shell is a tab of the terminal now, not a plugin: {names:?}"
    );

    // The kernel does not know which plugin "is" the terminal — it knows which
    // asked for raw session input. Exactly one may, or a keystroke has two
    // homes.
    let raw: Vec<&str> = host
        .plugins
        .iter()
        .filter(|plugin| plugin.session_input)
        .map(|plugin| plugin.name.as_str())
        .collect();
    assert_eq!(raw, vec![TERMINAL], "one pane owns the pty");

    let focusable: Vec<&str> = host
        .focusable()
        .iter()
        .map(|index| host.plugins[*index].name.as_str())
        .collect();
    assert_eq!(
        focusable.iter().filter(|name| **name == TERMINAL).count(),
        1,
        "the pane is one stop in the ring: {focusable:?}"
    );
}

#[test]
fn the_shell_view_names_itself_to_the_focus_badge() {
    // The footer's badge must say what you are LOOKING at, as v1's does
    // (`InputFocus::Terminal if is_shell_view => "Shell"`). The seam is the
    // surface name: this pane addresses its shell as `<id>#shell`, and the badge
    // is derived from that — so the two cannot disagree about which view is up.
    let host = with_a_selection();
    let index = index_of(&host, TERMINAL);

    let agent_surface = tree(&host, 100, 10)
        .first_session_surface()
        .expect("the agent tab paints a session surface")
        .to_string();
    assert!(
        !agent_surface.contains('#'),
        "the agent tab names the session plainly: {agent_surface}"
    );
    assert_eq!(
        thurbox::kernel::bands::focus_label(Some(&agent_surface), TERMINAL),
        "Agent"
    );

    host.on_action(index, "terminal.shell").expect("select");
    let shell_surface = tree(&host, 100, 10)
        .first_session_surface()
        .expect("the shell tab paints a session surface")
        .to_string();
    assert!(
        shell_surface.ends_with("#shell"),
        "the shell tab names its own view: {shell_surface}"
    );
    assert_eq!(
        thurbox::kernel::bands::focus_label(Some(&shell_surface), TERMINAL),
        "Shell",
        "switching to the shell must change the badge"
    );
}

#[test]
fn the_shell_chord_survived_the_merge() {
    // The opener moved plugins; the chords it is reachable by must not have.
    let host = host();
    let registry = registry(&host);
    for press in [
        KeyPress {
            name: "t".into(),
            ch: Some('t'),
            ctrl: true,
            ..KeyPress::default()
        },
        // The F-key alternate exists because a focused terminal is exactly
        // where a bare `ctrl+<letter>` is most likely to be wanted by the agent.
        KeyPress {
            name: "f8".into(),
            ..KeyPress::default()
        },
    ] {
        let binding = registry
            .resolve(&press, Some(TERMINAL))
            .unwrap_or_else(|| panic!("{} is not bound", press.name));
        assert_eq!(binding.action, "shell.open");
        assert_eq!(binding.plugin, TERMINAL, "declared by the pane that acts");
    }
}

// ── the strip is the pane's, so it is on every tab ────────────────────────

#[test]
fn the_tab_strip_renders_on_the_shell_tab_too() {
    // The bug the merge exists to fix: the strip used to live in the agent
    // plugin, so switching view took it off the screen with it.
    let host = with_a_selection();
    let index = index_of(&host, TERMINAL);

    let agent_tab = painted(&tree(&host, 100, 10), 100, 10);
    assert!(agent_tab[0].contains("Agent"), "{}", agent_tab[0]);
    assert!(agent_tab[0].contains("Shell"), "{}", agent_tab[0]);

    host.on_action(index, "terminal.shell").expect("select");
    let shell_tab = painted(&tree(&host, 100, 10), 100, 10);
    assert!(
        shell_tab[0].contains("Agent") && shell_tab[0].contains("Shell"),
        "the strip must survive the switch: {}",
        shell_tab[0]
    );
    // v1's shell title: name and view, with neither branch nor status, because
    // neither describes the shell (`ui::terminal_view`).
    assert!(shell_tab[0].contains("(shell)"), "{}", shell_tab[0]);
}

#[test]
fn the_showing_view_is_the_primary_chip() {
    // Which tab is active is communicated by the chip's style — v1's
    // `button_style` primary — so it has to move with the view.
    let host = with_a_selection();
    let index = index_of(&host, TERMINAL);

    let before = buffer(&tree(&host, 100, 10), 100, 10);
    let line: String = (0..100).map(|x| before[(x, 0)].symbol()).collect();
    let (agent_at, shell_at) = (column_of(&line, "Agent"), column_of(&line, "Shell"));
    let active = before[(agent_at, 0)].bg;
    assert_ne!(
        active,
        before[(shell_at, 0)].bg,
        "the active chip must not look like the rest"
    );

    host.on_action(index, "terminal.shell").expect("select");
    let after = buffer(&tree(&host, 100, 10), 100, 10);
    assert_eq!(after[(shell_at, 0)].bg, active, "the fill follows the view");
    assert_ne!(after[(agent_at, 0)].bg, active);
}

// ── the views ─────────────────────────────────────────────────────────────

#[test]
fn the_shell_tab_places_the_shell_surface() {
    // No new node kind: the shell is `<id>#shell` over the same primitive.
    let host = with_a_selection();
    let index = index_of(&host, TERMINAL);
    let agent = session_surface(&tree(&host, 60, 10));
    assert!(
        !agent.contains('#'),
        "the agent view is the bare id: {agent}"
    );

    host.on_action(index, "terminal.shell").expect("select");
    let kinds: Vec<&str> = host
        .drain_commands()
        .iter()
        .map(|command| command.kind())
        .collect();
    // Opening the shell and bringing the pane forward, which is what v1's
    // `select_central_tab` does for both terminal tabs.
    assert!(kinds.contains(&"shell"), "{kinds:?}");
    assert!(kinds.contains(&"focus"), "{kinds:?}");
    assert_eq!(
        session_surface(&tree(&host, 60, 10)),
        format!("{agent}#shell")
    );
}

#[test]
fn the_chord_toggles_where_a_chip_selects() {
    // v1 `toggle_shell_view` vs `select_central_tab`: the key flips between the
    // two views, a tab click names the one you want. Selecting twice must not
    // bounce you back off the tab you asked for.
    let host = with_a_selection();
    let index = index_of(&host, TERMINAL);
    let agent = session_surface(&tree(&host, 60, 10));

    host.on_action(index, "shell.open").expect("toggle");
    assert_eq!(
        session_surface(&tree(&host, 60, 10)),
        format!("{agent}#shell")
    );
    host.on_action(index, "shell.open").expect("toggle back");
    assert_eq!(session_surface(&tree(&host, 60, 10)), agent);

    host.on_action(index, "terminal.shell").expect("select");
    host.on_action(index, "terminal.shell")
        .expect("select again");
    assert_eq!(
        session_surface(&tree(&host, 60, 10)),
        format!("{agent}#shell")
    );
    host.on_action(index, "terminal.agent").expect("select");
    assert_eq!(session_surface(&tree(&host, 60, 10)), agent);
}

#[test]
fn each_session_keeps_its_own_tab() {
    // v1 keys the view per session (`App::session_terminal_views`), so a shell
    // opened on one session does not follow you to the next.
    let host = with_a_selection();
    let index = index_of(&host, TERMINAL);
    let sessions = index_of(&host, "sessions");
    let first = session_surface(&tree(&host, 60, 10));

    host.on_action(index, "terminal.shell").expect("select");
    host.on_action(sessions, "sessions.next").expect("next");
    let second = session_surface(&tree(&host, 60, 10));
    assert!(
        !second.contains('#') && second != first,
        "a different session, on its own agent view: {second}"
    );

    host.on_action(sessions, "sessions.previous").expect("back");
    assert_eq!(
        session_surface(&tree(&host, 60, 10)),
        format!("{first}#shell")
    );
}

#[test]
fn paging_is_the_agent_views_and_the_ptys_everywhere_else() {
    // Scrollback is read off the agent's parser only, so on the shell tab a
    // page key is declined and reaches whatever is running in it instead.
    let host = with_a_selection();
    let index = index_of(&host, TERMINAL);
    let page = KeyPress {
        name: "pageup".into(),
        ..KeyPress::default()
    };
    assert!(host.on_key(index, &page).expect("key"), "the agent scrolls");

    host.on_action(index, "terminal.shell").expect("select");
    assert!(
        !host.on_key(index, &page).expect("key"),
        "the shell leaves it to the pty"
    );
    // And the title says nothing about a scroll the shell surface cannot honour.
    let line = painted(&tree(&host, 100, 10), 100, 10)[0].clone();
    assert!(!line.contains('↑'), "{line}");
}

// ── the chips are click targets ───────────────────────────────────────────

#[test]
fn a_chip_selects_a_tab_rather_than_focusing_another_plugin() {
    let host = with_a_selection();
    let verbs: Vec<ClickVerb> = hits(&tree(&host, 100, 10), 100, 10)
        .iter()
        .filter_map(|hit| hit.identity.click_verb())
        .collect();

    assert!(
        verbs.contains(&ClickVerb::Action("terminal.agent".into())),
        "{verbs:?}"
    );
    assert!(
        verbs.contains(&ClickVerb::Action("terminal.shell".into())),
        "{verbs:?}"
    );
    assert!(
        !verbs.contains(&ClickVerb::Focus("shell".into())),
        "there is no shell plugin to focus: {verbs:?}"
    );
    // No chip may name a plugin that is not loaded: a `focus:` verb pointing at
    // a removed pane would light up and then do nothing.
    assert!(
        !verbs
            .iter()
            .any(|verb| matches!(verb, ClickVerb::Focus(name) if name == "review")),
        "the review chip went with its plugin: {verbs:?}"
    );
    // The chevron shares the border with them and must not have been shifted
    // out of the strip by the tabs beside it.
    assert!(
        verbs.contains(&ClickVerb::Action("sessions.toggle_panel".into())),
        "{verbs:?}"
    );
}

#[test]
fn a_chip_is_only_as_wide_as_its_own_label() {
    // A chip that swallowed the border would make every click on it mean the
    // same thing — and would eat the drag-selection over the terminal below.
    let host = with_a_selection();
    let node = tree(&host, 100, 10);
    for hit in hits(&node, 100, 10) {
        if hit.identity.click_verb().is_none() {
            continue;
        }
        assert_eq!(hit.rect.y, 0, "the strip is the top border");
        assert!(hit.rect.height == 1 && hit.rect.width < 20, "{hit:?}");
    }
}
