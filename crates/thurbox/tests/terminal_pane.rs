use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;

use thurbox::kernel::host::{Click, KeyPress, LuaHost, Published, RenderContext, Scroll};
use thurbox::kernel::node::{ClickVerb, Node, SurfaceSource};
use thurbox::kernel::paint::{render, render_recording, Hit, PlaceholderSurfaces};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;
use thurbox::session::SessionState;

const TERMINAL: &str = "agent";

fn host() -> LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ui");
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
        backend: "local:tmux".into(),
        backend_id: Some("%1".into()),
        remote_host: None,
        place: None,
        agent_session_id: None,
        parent_id: None,
        display_order: None,
        worktree_count: 1,
        git: None,
        stopped: false,
        hook_state: None,
        key: None,
        reports_as: None,
        detected_agent: None,
        shell_backend_id: None,
        member_dirs: Vec::new(),
        meta: Default::default(),
        deleting: None,
        state_why: None,
        status_source: None,
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
    let repos = thurbox::kernel::repos::RepoStore::new();
    host.publish(&Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
        snapshot: &sample(),
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
    host.mods
        .iter()
        .position(|loaded| loaded.name == name)
        .unwrap_or_else(|| panic!("no plugin named {name}"))
}

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
        .as_ref()
        .clone()
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

fn column_of(line: &str, needle: &str) -> u16 {
    let at = line
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} is not on the border: {line}"));
    u16::try_from(line[..at].chars().count()).expect("a column")
}

fn surface_scroll(node: &Node) -> u16 {
    fn walk(node: &Node) -> Option<u16> {
        match node {
            Node::Surface {
                source: SurfaceSource::Session(_),
                scroll,
                ..
            } => Some(*scroll),
            Node::Box { children, .. } => children.iter().find_map(walk),
            _ => None,
        }
    }
    walk(node).expect("the pane places a session surface")
}

fn wheel(host: &LuaHost, up: bool) -> bool {
    host.on_scroll(index_of(host, TERMINAL), &Scroll { up, x: 30, y: 5 })
        .expect("the wheel")
}

fn session_surface(node: &Node) -> String {
    node.first_session_surface()
        .expect("the pane places a session surface")
        .to_string()
}

#[test]
fn the_centre_holds_one_terminal_mod_rather_than_two() {
    let host = host();
    let names: Vec<&str> = host.mods.iter().map(|p| p.name.as_str()).collect();
    assert!(
        !names.contains(&"shell"),
        "the shell is a tab of the terminal now, not a plugin: {names:?}"
    );

    let raw: Vec<&str> = host
        .mods
        .iter()
        .filter(|loaded| loaded.session_input)
        .map(|loaded| loaded.name.as_str())
        .collect();
    assert_eq!(raw, vec![TERMINAL], "one pane owns the pty");

    let focusable: Vec<&str> = host
        .focusable()
        .iter()
        .map(|index| host.mods[*index].name.as_str())
        .collect();
    assert_eq!(
        focusable.iter().filter(|name| **name == TERMINAL).count(),
        1,
        "the pane is one stop in the ring: {focusable:?}"
    );
}

#[test]
fn the_shell_view_names_itself_to_the_focus_badge() {
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
    let host = host();
    let registry = registry(&host);
    for press in [
        KeyPress {
            name: "t".into(),
            ch: Some('t'),
            ctrl: true,
            ..KeyPress::default()
        },
        KeyPress {
            name: "f8".into(),
            ..KeyPress::default()
        },
    ] {
        let binding = registry
            .resolve(&press, Some(TERMINAL))
            .unwrap_or_else(|| panic!("{} is not bound", press.name));
        assert_eq!(binding.action, "shell.open");
        assert_eq!(binding.owner, TERMINAL, "declared by the pane that acts");
    }
}

#[test]
fn the_tab_strip_renders_on_the_shell_tab_too() {
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
    assert!(shell_tab[0].contains("(shell)"), "{}", shell_tab[0]);
}

#[test]
fn the_showing_view_is_the_primary_chip() {
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

#[test]
fn the_shell_tab_places_the_shell_surface() {
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
    assert!(kinds.contains(&"shell"), "{kinds:?}");
    assert!(kinds.contains(&"focus"), "{kinds:?}");
    assert_eq!(
        session_surface(&tree(&host, 60, 10)),
        format!("{agent}#shell")
    );
}

#[test]
fn the_chord_toggles_where_a_chip_selects() {
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
    let host = with_a_selection();
    let index = index_of(&host, TERMINAL);
    assert!(
        host.on_action(index, "terminal.scroll_up").expect("action"),
        "the agent scrolls"
    );

    host.on_action(index, "terminal.shell").expect("select");
    assert!(
        !host.on_action(index, "terminal.scroll_up").expect("action"),
        "the shell leaves it to the pty"
    );
    let line = painted(&tree(&host, 100, 10), 100, 10)[0].clone();
    assert!(!line.contains('↑'), "{line}");
}

#[test]
fn a_chip_selects_a_tab_rather_than_focusing_another_mod() {
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
    assert!(
        !verbs
            .iter()
            .any(|verb| matches!(verb, ClickVerb::Focus(name) if name == "review")),
        "the review chip went with its plugin: {verbs:?}"
    );
    assert!(
        verbs.contains(&ClickVerb::Action("sessions.toggle_panel".into())),
        "{verbs:?}"
    );
}

#[test]
fn a_chip_is_only_as_wide_as_its_own_label() {
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

#[test]
fn a_wheel_tick_scrolls_the_terminal_under_it() {
    let host = with_a_selection();
    assert_eq!(
        surface_scroll(&tree(&host, 60, 10)),
        0,
        "live, at the bottom"
    );

    assert!(wheel(&host, true), "the pane takes the tick");
    assert_eq!(
        surface_scroll(&tree(&host, 60, 10)),
        1,
        "one report is one line, as it is when the tick is forwarded to a pty"
    );

    assert!(wheel(&host, false), "and back down");
    assert_eq!(surface_scroll(&tree(&host, 60, 10)), 0);
    assert!(
        !wheel(&host, false),
        "at the bottom there is nothing to give, so the tick is declined"
    );
}

#[test]
fn the_wheel_scrolls_the_shell_tab_as_well() {
    let host = with_a_selection();
    let index = index_of(&host, TERMINAL);
    host.on_action(index, "terminal.shell").expect("select");

    assert!(wheel(&host, true), "the shell takes the tick");
    let node = tree(&host, 100, 10);
    assert!(
        session_surface(&node).ends_with("#shell"),
        "still the shell's own surface"
    );
    assert_eq!(surface_scroll(&node), 1);
    assert!(
        painted(&node, 100, 10)[0].contains("1↑"),
        "and says so on its title: {:?}",
        painted(&node, 100, 10)[0]
    );
}

#[test]
fn each_tab_keeps_its_own_place_in_its_own_scrollback() {
    let host = with_a_selection();
    let index = index_of(&host, TERMINAL);
    assert!(wheel(&host, true));
    assert!(wheel(&host, true));
    assert_eq!(surface_scroll(&tree(&host, 60, 10)), 2);

    host.on_action(index, "terminal.shell").expect("select");
    assert_eq!(
        surface_scroll(&tree(&host, 60, 10)),
        0,
        "the shell has its own, and has not been scrolled"
    );

    host.on_action(index, "terminal.agent").expect("back");
    assert_eq!(
        surface_scroll(&tree(&host, 60, 10)),
        2,
        "the agent kept its"
    );
}

#[test]
fn typing_snaps_the_view_back_to_the_bottom() {
    let host = with_a_selection();
    let index = index_of(&host, TERMINAL);
    assert!(wheel(&host, true));
    assert_eq!(surface_scroll(&tree(&host, 60, 10)), 1);

    let key = KeyPress {
        name: "a".into(),
        ch: Some('a'),
        ..KeyPress::default()
    };
    assert!(
        !host.on_key(index, &key).expect("key"),
        "the keystroke belongs to the agent"
    );
    assert_eq!(
        surface_scroll(&tree(&host, 60, 10)),
        0,
        "a key forwarded to the pty returns the view to the live end"
    );
}

const BAR_HEIGHT: u16 = 12;
const BAR_TRACK_TOP: u16 = 1;
const BAR_TRACK_BOTTOM: u16 = 8;
const BAR_DEPTH: u16 = 20;

const DRAG: &str = "drag";

fn bar_press(host: &LuaHost, row: u16, dragging: bool) -> bool {
    host.on_click(
        index_of(host, TERMINAL),
        &Click {
            id: None,
            classes: Vec::new(),
            role: Some(DRAG.into()),
            x: 0,
            y: row,
            w: 1,
            h: BAR_HEIGHT - 2,
            screen_x: 0,
            screen_y: 0,
            dragging,
            clicks: 1,
        },
    )
    .expect("press")
}

fn with_a_scrollback() -> LuaHost {
    let host = with_a_selection();
    for _ in 0..BAR_DEPTH {
        assert!(wheel(&host, true), "scroll back");
    }
    assert_eq!(surface_scroll(&tree(&host, 60, BAR_HEIGHT)), BAR_DEPTH);
    host
}

#[test]
fn the_scrollbar_is_a_click_target_only_once_there_is_one() {
    let host = with_a_selection();
    let roles = |host: &LuaHost| -> Vec<String> {
        hits(&tree(host, 60, BAR_HEIGHT), 60, BAR_HEIGHT)
            .iter()
            .filter_map(|hit| hit.identity.role.clone())
            .collect()
    };
    assert!(
        !roles(&host).contains(&DRAG.to_string()),
        "an unscrolled pane draws no bar, so there is nothing to grab"
    );

    let host = with_a_scrollback();
    assert!(
        roles(&host).contains(&DRAG.to_string()),
        "the bar is grabbable as soon as it is drawn"
    );
}

#[test]
fn the_ends_of_the_track_are_the_ends_of_the_scrollback() {
    let host = with_a_scrollback();
    assert!(bar_press(&host, BAR_TRACK_BOTTOM, false));
    assert_eq!(
        surface_scroll(&tree(&host, 60, BAR_HEIGHT)),
        0,
        "the end of the track is the live bottom"
    );

    assert!(bar_press(&host, BAR_TRACK_TOP, false));
    assert_eq!(
        surface_scroll(&tree(&host, 60, BAR_HEIGHT)),
        BAR_DEPTH,
        "and the start of it is as far back as we have been"
    );
}

#[test]
fn the_thumb_is_picked_up_where_it_was_grabbed() {
    let host = with_a_scrollback();
    assert!(bar_press(&host, BAR_TRACK_BOTTOM, false));
    assert_eq!(
        surface_scroll(&tree(&host, 60, BAR_HEIGHT)),
        0,
        "at the live bottom the thumb's last row is the track's last row"
    );

    assert!(bar_press(&host, BAR_TRACK_BOTTOM, false));
    assert_eq!(
        surface_scroll(&tree(&host, 60, BAR_HEIGHT)),
        0,
        "grabbing the thumb where it already is moves nothing"
    );

    assert!(bar_press(&host, BAR_TRACK_BOTTOM - 1, true));
    let one_row = surface_scroll(&tree(&host, 60, BAR_HEIGHT));
    assert!(
        one_row > 0 && one_row < BAR_DEPTH,
        "one row of travel, not a jump to the top: {one_row}"
    );
}

#[test]
fn a_press_that_is_not_the_scrollbar_is_declined() {
    let host = with_a_scrollback();
    assert!(
        !host
            .on_click(
                index_of(&host, TERMINAL),
                &Click {
                    role: Some("row".into()),
                    h: BAR_HEIGHT - 2,
                    ..Click::default()
                },
            )
            .expect("press"),
        "declined, so the press falls through as it always did"
    );
}
