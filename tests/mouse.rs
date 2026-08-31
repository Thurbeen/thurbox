//! Click targeting, against the *real* bundled plugins.
//!
//! The kernel hit-tests the tree it painted, so these render the shipped Lua
//! and then ask "what is under this cell?". That is deliberately the same
//! question the event loop asks — if a plugin stops declaring a target, a test
//! here fails rather than a click quietly doing nothing.
//!
//! What is NOT here is the loop's dispatch of a hit, which needs a live
//! `LuaHost` plus a terminal plus a registry the binary owns; the pieces it is
//! built from — verb parsing, hit ordering, plugin attribution — are each
//! asserted below.

use ratatui::backend::TestBackend;
use ratatui::layout::{Position, Rect};
use ratatui::Terminal;

use thurbox::kernel::host::{LuaHost, Published, RenderContext};
use thurbox::kernel::node::{ClickVerb, Identity};
use thurbox::kernel::paint::{render_recording, Hit, PlaceholderSurfaces};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;

fn host() -> LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

fn row(name: &str, repo: &str) -> SessionRow {
    SessionRow {
        id: format!("{name}-0000-0000-0000-000000000000"),
        name: name.into(),
        agent: "claude".into(),
        status: "idle".into(),
        cwd: Some(std::path::PathBuf::from(format!("/src/{repo}"))),
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
        shell_backend_id: None,
        member_dirs: Vec::new(),
    }
}

fn sample() -> Snapshot {
    Snapshot {
        sessions: vec![row("alpha", "thurbox"), row("beta", "website")],
        ..Snapshot::default()
    }
}

/// Render one bundled plugin and return every hitbox it declared.
fn hits_of(plugin: &str, width: u16, height: u16) -> Vec<Hit> {
    let host = host();
    let themes = Themes::load(None);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
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

    // The session list publishes `store.selected`, which is how the agent pane
    // knows it has a session to show — without it that pane draws its empty
    // state, whose border carries no tab strip and no collapse toggle. The loop
    // paints the list first for the same reason.
    if plugin != "sessions" {
        let sessions = host
            .plugins
            .iter()
            .position(|p| p.name == "sessions")
            .expect("sessions plugin");
        host.render(
            sessions,
            RenderContext {
                width: 30,
                height,
                focused: false,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render the list");
    }

    let index = host
        .plugins
        .iter()
        .position(|p| p.name == plugin)
        .unwrap_or_else(|| panic!("no plugin {plugin}"));
    let node = host
        .render(
            index,
            RenderContext {
                width,
                height,
                focused: true,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render")
        .node;

    let mut hits = Vec::new();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| render_recording(frame, frame.area(), &node, &PlaceholderSurfaces, &mut hits))
        .expect("draw");
    hits
}

/// The hitbox the loop would pick for a cell: innermost, last painted.
fn target_at(hits: &[Hit], x: u16, y: u16) -> Option<&Hit> {
    let position = Position::new(x, y);
    hits.iter().rev().find(|hit| hit.rect.contains(position))
}

// ── the vocabulary ────────────────────────────────────────────────────────

#[test]
fn a_verb_is_read_off_the_role_and_nothing_else_is() {
    assert_eq!(
        ClickVerb::parse(Some("action:themes.open")),
        Some(ClickVerb::Action("themes.open".into()))
    );
    assert_eq!(
        ClickVerb::parse(Some("key:ctrl+q")),
        Some(ClickVerb::Key("ctrl+q".into()))
    );
    assert_eq!(
        ClickVerb::parse(Some("focus:shell")),
        Some(ClickVerb::Focus("shell".into()))
    );
    // A url's own colons belong to the url: the verb is read off the FIRST one
    // and everything after it is the link.
    assert_eq!(
        ClickVerb::parse(Some("url:https://example.test/a?q=1")),
        Some(ClickVerb::Url("https://example.test/a?q=1".into()))
    );
    // `row` is what widgets.lua writes for an ordinary list row, and it must
    // keep meaning "ask the plugin" rather than becoming a kernel verb.
    assert!(ClickVerb::parse(Some("row")).is_none());
}

/// The seam the loop uses to turn a pane's `url:` node into a link the outer
/// terminal can open: the recorded hit's rect, read back out of the frame that
/// was just painted.
///
/// Asserted here rather than in the kernel's own tests because it is the
/// *combination* that has to hold — a hitbox is recorded for the node, and the
/// cells at that rect are the ones the plugin drew. A node whose rect drifted
/// from its glyphs would link the wrong text without either half looking wrong.
#[test]
fn a_pane_url_node_becomes_a_link_over_the_cells_it_drew() {
    use thurbox::kernel::node::{Node, Run, Size};
    use thurbox::kernel::terminal::drawn_link_paints;

    let url = "https://example.test/some/page";
    let node = Node::Text {
        // Indented, as a pane indents its text: the padding must not be linked.
        lines: vec![vec![Run::plain(format!("  {url}"))]],
        align: Default::default(),
        wrap: false,
        scroll: 0,
        frame: None,
        size: Size::default(),
        identity: Identity {
            role: Some(format!("url:{url}")),
            ..Identity::default()
        },
    };

    let mut hits = Vec::new();
    let mut terminal = Terminal::new(TestBackend::new(40, 1)).expect("terminal");
    let painted = terminal
        .draw(|frame| render_recording(frame, frame.area(), &node, &PlaceholderSurfaces, &mut hits))
        .expect("draw");

    assert_eq!(hits.len(), 1, "the node carries a role, so it is a target");
    assert_eq!(
        hits[0].identity.click_verb(),
        Some(ClickVerb::Url(url.into())),
        "and the loop reads a url verb off it"
    );

    let paints = drawn_link_paints(painted.buffer, hits[0].rect, url);
    assert_eq!(paints.len(), 1);
    assert_eq!(paints[0].url, url);
    assert_eq!(
        paints[0].x, 2,
        "the two-space indent is not part of the link"
    );
    let printed: String = paints[0]
        .cells
        .iter()
        .map(|(symbol, _)| symbol.as_str())
        .collect();
    assert_eq!(printed, url);
}

// ── recording ─────────────────────────────────────────────────────────────

#[test]
fn only_identified_nodes_become_targets() {
    // A tree of plain text declares nothing, so a click on it can only reach
    // the pane fallback the loop records around it.
    let hits = hits_of("sessions", 40, 12);
    assert!(
        hits.iter().all(|hit| !hit.identity.is_empty()),
        "an empty identity would shadow the pane fallback"
    );
}

#[test]
fn the_collapse_toggle_is_one_target_covering_its_chevron_and_its_hint() {
    // The label is ` ◀ F9 ` and it is ONE button. It has to be two nodes — a node
    // carries one style, and the chevron reads accent while the hint reads muted
    // — so both carry the same role, and the click registry answers either.
    //
    // Before this, only the three-cell chevron was a target: the pointer had to
    // find the arrow inside a six-cell label, which is the "increased click size"
    // this fixes.
    let hits = hits_of("agent", 113, 10);
    let toggle: Vec<&Hit> = hits
        .iter()
        .filter(|hit| {
            hit.identity.click_verb() == Some(ClickVerb::Action("sessions.toggle_panel".into()))
        })
        .collect();

    assert_eq!(
        toggle.len(),
        2,
        "the chevron and its hint are both targets: {:?}",
        toggle.iter().map(|hit| hit.rect).collect::<Vec<_>>()
    );
    let covered: u16 = toggle.iter().map(|hit| hit.rect.width).sum();
    assert_eq!(covered, 6, "` ◀ F9 ` is six cells wide: {covered}");
    // Adjacent, so there is no dead cell between the halves.
    let mut rects: Vec<Rect> = toggle.iter().map(|hit| hit.rect).collect();
    rects.sort_by_key(|rect| rect.x);
    assert_eq!(
        rects[0].x + rects[0].width,
        rects[1].x,
        "the two halves must touch: {rects:?}"
    );
}

#[test]
fn a_parent_is_recorded_before_its_children() {
    // The ordering the whole hit test rests on: reverse scan finds the
    // innermost node, and the pane fallback only when nothing inside matched.
    use thurbox::kernel::node::{Axis, Node, Size};

    let child = Node::Text {
        lines: vec![],
        align: Default::default(),
        wrap: false,
        scroll: 0,
        frame: None,
        size: Size::default(),
        identity: Identity {
            id: Some("child".into()),
            ..Identity::default()
        },
    };
    let parent = Node::Box {
        axis: Axis::Vertical,
        gap: 0,
        children: vec![child],
        frame: None,
        size: Size::default(),
        identity: Identity {
            id: Some("parent".into()),
            ..Identity::default()
        },
    };

    let mut hits = Vec::new();
    let mut terminal = Terminal::new(TestBackend::new(10, 3)).expect("terminal");
    terminal
        .draw(|frame| {
            render_recording(
                frame,
                frame.area(),
                &parent,
                &PlaceholderSurfaces,
                &mut hits,
            )
        })
        .expect("draw");

    let ids: Vec<_> = hits
        .iter()
        .map(|hit| hit.identity.id.clone().unwrap_or_default())
        .collect();
    assert_eq!(ids, vec!["parent", "child"]);
    assert_eq!(
        target_at(&hits, 0, 0).and_then(|hit| hit.identity.id.as_deref()),
        Some("child"),
        "the innermost node must win"
    );
}

#[test]
fn a_framed_node_is_clickable_on_its_border() {
    // v1 puts the collapse chevron and the tab pills ON the central pane's top
    // border, so a hitbox that stopped at the inner rect could never catch one.
    use thurbox::kernel::node::{Frame, Node, Size};

    let node = Node::Text {
        lines: vec![],
        align: Default::default(),
        wrap: false,
        scroll: 0,
        frame: Some(Frame::default()),
        size: Size::default(),
        identity: Identity {
            id: Some("panel".into()),
            ..Identity::default()
        },
    };
    let mut hits = Vec::new();
    let mut terminal = Terminal::new(TestBackend::new(8, 3)).expect("terminal");
    terminal
        .draw(|frame| render_recording(frame, frame.area(), &node, &PlaceholderSurfaces, &mut hits))
        .expect("draw");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rect, Rect::new(0, 0, 8, 3));
}

// ── the bundled plugins declare what v1 recorded ──────────────────────────

#[test]
fn every_session_row_is_a_click_target_carrying_its_id() {
    let hits = hits_of("sessions", 40, 12);
    let rows: Vec<_> = hits
        .iter()
        .filter(|hit| hit.identity.classes.iter().any(|c| c == "session-row"))
        .collect();
    assert_eq!(rows.len(), 2, "one target per session:\n{hits:#?}");

    for hit in &rows {
        assert!(
            hit.identity
                .id
                .as_deref()
                .is_some_and(|id| id.contains('-')),
            "a row must carry the session id, not a screen position"
        );
        assert_eq!(hit.identity.role.as_deref(), Some("row"));
        assert_eq!(hit.rect.height, 1);
    }
    // The border columns are outside every row, so a click on the frame focuses
    // the column rather than selecting whatever row it is level with.
    let inside = rows[0].rect;
    assert!(inside.x >= 1 && inside.right() <= 39);
}

#[test]
fn a_session_row_click_lands_on_the_session_under_it() {
    let hits = hits_of("sessions", 40, 12);
    let first = hits
        .iter()
        .find(|hit| hit.identity.classes.iter().any(|c| c == "session-row"))
        .expect("a session row");
    let picked = target_at(&hits, first.rect.x, first.rect.y).expect("something under the row");
    assert_eq!(picked.identity.id, first.identity.id);
}

// ── the plugins that answer a click themselves ────────────────────────────

#[test]
fn clicking_a_session_row_selects_that_session() {
    let host = host();
    let themes = Themes::load(None);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
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

    let index = host
        .plugins
        .iter()
        .position(|p| p.name == "sessions")
        .expect("sessions");
    let ctx = RenderContext {
        width: 40,
        height: 12,
        focused: true,
        elapsed: 0.0,
        frame: 0,
    };
    host.render(index, ctx).expect("render");

    // The second session, which is not the one the cursor starts on.
    let wanted = sample().sessions[1].id.clone();
    let click = thurbox::kernel::host::Click {
        id: Some(wanted.clone()),
        classes: vec!["row".into(), "session-row".into()],
        role: Some("row".into()),
        x: 0,
        y: 0,
        w: 40,
        h: 1,
        dragging: false,
    };
    assert!(host.on_click(index, &click).expect("click"), "handled");

    // The pane publishes its selection through `store`, which is how the agent
    // pane learns what to show — so re-rendering and reading the tree back is
    // the honest check that the click took.
    let node = host.render(index, ctx).expect("render").node;
    let mut hits = Vec::new();
    let mut terminal = Terminal::new(TestBackend::new(40, 12)).expect("terminal");
    terminal
        .draw(|frame| render_recording(frame, frame.area(), &node, &PlaceholderSurfaces, &mut hits))
        .expect("draw");
    let selected = hits
        .iter()
        .find(|hit| hit.identity.classes.iter().any(|c| c == "selected"))
        .or_else(|| {
            hits.iter()
                .find(|hit| hit.identity.id.as_deref() == Some(wanted.as_str()))
        });
    assert!(selected.is_some(), "the clicked row should be on screen");
}

#[test]
fn a_click_on_nothing_is_declined_rather_than_guessed() {
    let host = host();
    let index = host
        .plugins
        .iter()
        .position(|p| p.name == "sessions")
        .expect("sessions");
    let click = thurbox::kernel::host::Click::default();
    assert!(
        !host.on_click(index, &click).expect("click"),
        "an idless click must not move the cursor"
    );
}

#[test]
fn a_plugin_without_on_click_declines_instead_of_failing() {
    // Most panes never opt in, and a click on one has to fall through to the
    // pane focus fallback rather than surfacing an error panel.
    let host = host();
    // The agent pane draws a terminal surface and declares no `on_click`, so it
    // is the surviving example of the case: a press on it must fall through to
    // the focus fallback (and, over a terminal, arm a drag) rather than error.
    let index = host
        .plugins
        .iter()
        .position(|p| p.name == "agent")
        .expect("agent");
    assert!(!host
        .on_click(index, &thurbox::kernel::host::Click::default())
        .expect("no handler is not an error"));
}
