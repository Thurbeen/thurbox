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

use thurbox::kernel::host::{Click, KeyPress, LuaHost, Published, RenderContext, Scroll};
use thurbox::kernel::node::{ClickVerb, Node, SurfaceSource};
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
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
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

/// Which column a label starts in, in cells rather than bytes.
fn column_of(line: &str, needle: &str) -> u16 {
    let at = line
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} is not on the border: {line}"));
    u16::try_from(line[..at].chars().count()).expect("a column")
}

/// How far back the session surface this pane placed is scrolled.
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

/// One wheel report over the middle of the pane.
fn wheel(host: &LuaHost, up: bool) -> bool {
    host.on_scroll(index_of(host, TERMINAL), &Scroll { up, x: 30, y: 5 })
        .expect("the wheel")
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
    // Scrollback is read off the agent's parser only, so on the shell tab the
    // action declines and the key reaches whatever is running in it instead.
    //
    // Through the action rather than `on_key`: the page keys are *declared*
    // now, so they are listed in help and can be rebound — which is the whole
    // difference between a key a pane owns and a key it merely intercepts.
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

// ── the wheel ─────────────────────────────────────────────────────────────

#[test]
fn a_wheel_tick_scrolls_the_terminal_under_it() {
    // The wheel reaches a pane as its own tick, not as a synthesized `up`. This
    // pane is the reason the hook exists: it hands every unclaimed key to the
    // agent, so it is the one pane that cannot declare the arrow keys the
    // keystroke fallback needs — and the wheel therefore did nothing at all
    // over the only surface with a scrollback worth moving.
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
    // Scrollback is the pane's policy, and it used to hold one for the agent
    // view alone: the shell surface is a live terminal with a scrollback of its
    // own, so refusing to hold an offset for it left the wheel dead there.
    //
    // The page keys still decline on this tab (see the test above): a pager or
    // an editor running in the shell has its own idea of what a page is, and
    // none of what a wheel means.
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
    // Two live terminals taking turns in one rect, so one offset between them
    // would put the shell where the agent was — and the kernel writes the
    // offset into whichever parser it is drawing.
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
    // v1's rule: a key forwarded to the pty returns you to the live end of the
    // stream. Without it a wheel tick leaves you typing into a screen you
    // cannot see, which is a worse trap now that the wheel actually moves.
    //
    // The key is not consumed — the handler declines it — so it still reaches
    // the agent.
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
    assert_eq!(surface_scroll(&tree(&host, 60, 10)), 0);
}

// ── the scrollbar is a control ────────────────────────────────────────────

/// The pane height every scrollbar case below is rendered at, and the scrollback
/// they scroll. A 12-row pane leaves 10 inner rows: a cap, eight rows of track,
/// a cap.
const BAR_HEIGHT: u16 = 12;
const BAR_TRACK_TOP: u16 = 1;
const BAR_TRACK_BOTTOM: u16 = 8;
const BAR_DEPTH: u16 = 20;

/// The role the pane gives its scrollbar — the kernel's own spelling for "a
/// press here takes hold of the pointer".
const DRAG: &str = "drag";

/// A press, or a move under one, on row `row` of the scrollbar column.
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
            dragging,
        },
    )
    .expect("press")
}

/// A session scrolled back far enough to have a bar to grab.
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
    // The bar is painted into the right border column, which is one node for
    // the whole column — so it is a target or it is not, and there is no row of
    // it that can be pressed while the rest cannot.
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
    // Both ends have to be reachable, and the bottom one is the one that was
    // off by a line: the position a scrollbar can express is `0..=depth`, and
    // counting `depth` of them left the end of the track one line above the
    // live bottom — so the bar could be dragged all the way down and still
    // leave you off the stream.
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
    // A shallow scrollback makes a tall thumb, and pressing its lower half must
    // not jerk it up by half its length. The offset within the thumb is taken at
    // the press and held for the whole gesture, so the row under the pointer
    // stays the row under the pointer.
    let host = with_a_scrollback();
    // Live bottom: the thumb sits at the end of the track, and its last row is
    // the last row of the track.
    assert!(bar_press(&host, BAR_TRACK_BOTTOM, false));
    assert_eq!(surface_scroll(&tree(&host, 60, BAR_HEIGHT)), 0);

    // Pressing that last row again is a grab, not a jump: nothing moves.
    assert!(bar_press(&host, BAR_TRACK_BOTTOM, false));
    assert_eq!(
        surface_scroll(&tree(&host, 60, BAR_HEIGHT)),
        0,
        "grabbing the thumb where it already is moves nothing"
    );

    // And dragging it one row up moves by one row of the track, not by the
    // whole distance from the track's start.
    assert!(bar_press(&host, BAR_TRACK_BOTTOM - 1, true));
    let one_row = surface_scroll(&tree(&host, 60, BAR_HEIGHT));
    assert!(
        one_row > 0 && one_row < BAR_DEPTH,
        "one row of travel, not a jump to the top: {one_row}"
    );
}

#[test]
fn a_press_that_is_not_the_scrollbar_is_declined() {
    // The pane's other affordances are chips the kernel resolves itself through
    // a click verb; this handler must not swallow anything else that reaches it.
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
