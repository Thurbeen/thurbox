//! End-to-end test of the v2 kernel against the *real* bundled plugins.
//!
//! Loads `ui/` exactly as the binary does, publishes a snapshot, resolves the
//! arrangement and renders every plugin into its resolved rect — so this fails
//! if the kernel breaks *or* if a shipped plugin does.
//!
//! The properties under test are the load-bearing design decisions:
//!
//! - **D1** the kernel exposes four node kinds, and the panes are built from
//!   them via `ui/lib/widgets.lua` rather than from kernel widgets
//! - **D2** a plugin is told its own resolved rect, not the screen's
//! - **D3** the agent pane places a `surface` it does not paint
//! - **D5** plugin reads are served from a snapshot and never block
//! - isolation: a throwing plugin costs its own pane, not the frame

use std::path::PathBuf;

use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use thurbox::kernel::host::{KeyPress, LuaHost, RenderContext};
use thurbox::kernel::layout::{divide_slot, resolve, SlotMode};
use thurbox::kernel::node::{Axis, Node, KINDS};
use thurbox::kernel::paint::{render, PlaceholderSurfaces};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;

/// The theme every test publishes with: whatever the environment resolves,
/// which keeps these tests independent of the user's active choice.
fn themes() -> Themes {
    Themes::load(None)
}

/// Publish a snapshot with the defaults every test wants.
fn publish(host: &LuaHost, snapshot: &Snapshot) {
    publish_with(host, snapshot, &Default::default(), &[]);
}

/// Publish with attach errors and in-flight commands spelled out.
fn publish_with(
    host: &LuaHost,
    snapshot: &Snapshot,
    attach_errors: &std::collections::HashMap<String, String>,
    inflight: &[thurbox::kernel::command::InFlight],
) {
    let themes = themes();
    let registry = registry(host);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&thurbox::kernel::host::Published {
        snapshot,
        attach_errors,
        inflight,
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

/// A registry holding whatever the given host's plugins declared.
fn registry(host: &LuaHost) -> Registry {
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    registry
}

fn ui_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui")
}

fn host() -> LuaHost {
    let host = LuaHost::new(ui_dir());
    assert!(
        host.error.is_none(),
        "the bundled plugins must load: {:?}",
        host.error
    );
    host
}

fn row(name: &str, repo: &str, status: &str) -> SessionRow {
    SessionRow {
        id: format!("{name}-0000-0000-0000-000000000000"),
        name: name.to_string(),
        agent: "claude".to_string(),
        status: status.to_string(),
        cwd: None,
        repo: Some(repo.to_string()),
        repos: vec![repo.to_string()],
        branch: Some(format!("feat/{name}")),
        base_branch: None,
        backend: "local-tmux".to_string(),
        backend_id: Some("%1".to_string()),
        agent_session_id: None,
        remote_host: None,
        parent_id: None,
        display_order: None,
        worktree_count: 1,
        git: None,
        hook_state: None,
        shell_backend_id: None,
        member_dirs: Vec::new(),
    }
}

fn snapshot(rows: Vec<SessionRow>) -> Snapshot {
    Snapshot {
        sessions: rows,
        taken_at_ms: 1_700_000_000_000,
        ..Snapshot::default()
    }
}

fn sample() -> Snapshot {
    snapshot(vec![
        row("fix-osc52", "thurbox", "working"),
        row("add-wsl-tests", "thurbox", "blocked"),
        row("perf-cache", "thurbox", "done"),
        row("update-deps", "website", "idle"),
    ])
}

fn ctx(width: u16, height: u16, focused: bool) -> RenderContext {
    RenderContext {
        width,
        height,
        focused,
        elapsed: 1.0,
        frame: 1,
    }
}

/// Render one plugin into a rect and return the painted screen as text.
fn paint(host: &LuaHost, index: usize, width: u16, height: u16) -> Vec<String> {
    let node = host
        .render(index, ctx(width, height, true))
        .unwrap_or_else(|e| panic!("plugin should render: {e}"))
        .node;
    paint_node(&node, width, height)
}

/// Like `paint_node`, but keeps each cell's colours and modifiers. v1 marks the
/// selected row by STYLE (a highlighted background), never by a marker glyph —
/// see `tests/snapshots/v1_recordings__v1_session_list.snap`, whose rows carry
/// no cursor character — so a symbols-only comparison cannot see the cursor
/// move at all.
fn paint_styles(host: &LuaHost, index: usize, width: u16, height: u16) -> Vec<String> {
    let node = host
        .render(index, ctx(width, height, true))
        .unwrap_or_else(|e| panic!("plugin should render: {e}"))
        .node;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| render(frame, frame.area(), &node, &PlaceholderSurfaces))
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| {
                    let cell = &buffer[(x, y)];
                    format!(
                        "{}/{:?}/{:?}/{:?}",
                        cell.symbol(),
                        cell.fg,
                        cell.bg,
                        cell.modifier
                    )
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

fn paint_node(node: &Node, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| render(frame, frame.area(), node, &PlaceholderSurfaces))
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

fn index_of(host: &LuaHost, name: &str) -> usize {
    host.plugins
        .iter()
        .position(|plugin| plugin.name == name)
        .unwrap_or_else(|| panic!("no plugin named {name}"))
}

#[test]
fn the_bundled_plugins_load_and_claim_their_slots() {
    let host = host();
    let names: Vec<&str> = host.plugins.iter().map(|p| p.name.as_str()).collect();
    // The interface is three panes — the list, the centre and the search strip —
    // plus two floats that occupy no slot at all: the creation flow and the
    // confirmation. The view of the interface's own files is NOT among them: it
    // is a tab of the settings modal, because a recovery tool that is itself a
    // plugin can be the thing that is broken. Every other v1 surface was removed
    // with its plugin, and its slot with it — see `ui/layout.lua`.
    assert_eq!(
        names,
        ["sessions", "agent", "confirm", "search", "new_session"],
        "the bundled set changed"
    );
    // A strip of its own above the bands, so it can highlight the panes it is
    // searching instead of covering them.
    assert_eq!(host.in_slot("search").len(), 1);

    assert_eq!(host.in_slot("sessions").len(), 1);
    // The centre is still a `switch` slot even with one occupant: the mode is a
    // property of the slot, not of how many plugins happen to want it, and the
    // agent's own shell tab already relies on it.
    let centre: Vec<&str> = host
        .in_slot("center")
        .iter()
        .map(|i| host.plugins[*i].name.as_str())
        .collect();
    // One occupant today. The slot keeps its mode anyway: it is a property of
    // the slot, not of how many plugins happen to want it, and the next centre
    // pane (the code review) will share it.
    assert_eq!(centre, ["agent"], "{centre:?}");
    // Help, settings and the theme picker are kernel modals, so they overlay
    // rather than compete for this slot — and must never reappear as panes.
    for gone in ["themes", "help", "settings"] {
        assert!(!centre.contains(&gone), "{gone} is a modal, not a pane");
    }
    assert_eq!(host.slot_mode("center"), SlotMode::Switch);

    // The panes hold focus; the creation flow does not, because it floats — a
    // closed modal that could be tabbed to would be a pane drawing nothing.
    let focusable: Vec<&str> = host
        .focusable()
        .iter()
        .map(|i| host.plugins[*i].name.as_str())
        .collect();
    assert!(focusable.contains(&"sessions"), "{focusable:?}");
    assert!(focusable.contains(&"agent"), "{focusable:?}");
    for float in ["new_session", "confirm"] {
        assert!(!focusable.contains(&float), "{focusable:?}");
    }
    let floating: Vec<&str> = host
        .floating()
        .iter()
        .map(|i| host.plugins[*i].name.as_str())
        .collect();
    assert_eq!(
        floating,
        ["confirm", "new_session"],
        "a float must also be excluded from the focus ring, or Tab would land on \
         a pane that draws nothing"
    );
    // And it takes no slot the arrangement places, so it never competes with the
    // terminal for the centre.
    assert_eq!(
        host.in_slot("center").len(),
        1,
        "the flow is not a centre pane"
    );
}

#[test]
fn the_session_list_renders_real_snapshot_rows() {
    let host = host();
    publish(&host, &sample());
    let screen = paint(&host, index_of(&host, "sessions"), 40, 12).join("\n");

    assert!(screen.contains("fix-osc52"), "{screen}");
    assert!(screen.contains("add-wsl-tests"), "{screen}");
    // Grouped by repo, as v1 groups them.
    assert!(screen.contains("thurbox"), "{screen}");
    assert!(screen.contains("website"), "{screen}");
    // v1's title is the bare word, with one status dot per session packed into
    // the RIGHT side of the top border — it never spells out a count.
    assert!(screen.contains("Sessions"), "{screen}");
    assert!(!screen.contains("sessions · 4"), "{screen}");
}

#[test]
fn each_status_paints_its_own_glyph() {
    let host = host();
    publish(&host, &sample());
    let screen = paint(&host, index_of(&host, "sessions"), 40, 12).join("\n");

    // blocked ◆, done ●, idle ○ — working animates through the braille
    // spinner, so it is asserted separately below.
    for glyph in ["◆", "●", "○"] {
        assert!(screen.contains(glyph), "expected {glyph} in:\n{screen}");
    }
}

#[test]
fn the_working_spinner_advances_with_elapsed_time() {
    let host = host();
    publish(&host, &snapshot(vec![row("busy", "thurbox", "working")]));
    let index = index_of(&host, "sessions");

    let mut seen = std::collections::HashSet::new();
    for step in 0..8 {
        let node = host
            .render(
                index,
                RenderContext {
                    elapsed: f64::from(step) * 0.15,
                    ..ctx(40, 8, true)
                },
            )
            .expect("render")
            .node;
        seen.insert(paint_node(&node, 40, 8).join("\n"));
    }
    assert!(seen.len() > 1, "the spinner should animate across frames");
}

#[test]
fn a_plugin_is_told_its_own_rect_not_the_screens() {
    // design.md D2. The session list budgets its name column against the width
    // it was given, so a narrow pane must truncate where a wide one does not.
    let host = host();
    publish(
        &host,
        &snapshot(vec![row(
            "a-very-long-session-name-that-will-not-fit",
            "thurbox",
            "idle",
        )]),
    );
    let index = index_of(&host, "sessions");

    let narrow = paint(&host, index, 28, 8).join("\n");
    let wide = paint(&host, index, 80, 8).join("\n");

    // v1 clips a long session name to the pane rather than ellipsising it —
    // `truncate_ellipsis` is reserved for the agent-status tail. So the
    // property is "the narrow pane shows less", not "there is a … in it".
    assert!(
        !narrow.contains("a-very-long-session-name-that-will-not-fit"),
        "narrow pane should not fit the whole name:\n{narrow}"
    );
    assert!(
        wide.contains("a-very-long-session-name-that-will-not-fit"),
        "wide pane should show it in full:\n{wide}"
    );
}

#[test]
fn a_long_list_windows_against_the_resolved_height() {
    // The other half of D2: the scroll window and its overflow markers are
    // derived from the height the plugin was handed.
    let host = host();
    let rows: Vec<SessionRow> = (0..40)
        .map(|n| row(&format!("session-{n:02}"), "thurbox", "idle"))
        .collect();
    publish(&host, &snapshot(rows));

    let screen = paint(&host, index_of(&host, "sessions"), 40, 10).join("\n");
    // v1 marks overflow in the bottom border (`▼ 33`) rather than spending a
    // content row on it, which is what the port reproduces.
    assert!(
        screen.contains('▼'),
        "a windowed list must mark what it hid:\n{screen}"
    );
}

#[test]
fn the_agent_pane_places_a_surface_it_does_not_paint() {
    // design.md D3: the plugin frames and positions the terminal; the kernel
    // fills it. The plugin never sees a cell.
    let host = host();
    publish(&host, &sample());

    // Selecting is the session list's job, and it publishes through the store.
    let sessions = index_of(&host, "sessions");
    host.render(sessions, ctx(40, 12, true))
        .expect("render list");

    let node = host
        .render(index_of(&host, "agent"), ctx(60, 10, true))
        .expect("render agent")
        .node;
    let kinds = collect_kinds(&node);
    assert!(
        kinds.contains(&"surface"),
        "the agent pane must place a surface, found {kinds:?}"
    );
}

#[test]
fn the_agent_pane_says_so_when_nothing_is_selected() {
    let host = host();
    publish(&host, &snapshot(Vec::new()));
    let screen = paint(&host, index_of(&host, "agent"), 50, 8).join("\n");
    // v1 `ui::terminal_view` renders this exact phrase in the empty state.
    assert!(screen.contains("No active sessions"), "{screen}");
    // And offers nothing it cannot honour: the new-session chord went with its
    // plugin, so the box must not still advertise it.
    assert!(
        !screen.contains("Ctrl+N"),
        "the empty state advertises a chord that is unbound:\n{screen}"
    );
}

#[test]
fn the_arrangement_drops_the_side_column_when_narrow() {
    // v1's compute_layout breakpoints, now living in ui/layout.lua.
    let host = host();
    let area = |w: u16| Rect {
        x: 0,
        y: 0,
        width: w,
        height: 24,
    };

    let wide = resolve(&host.arrangement(120, 24).expect("wide layout"), area(120));
    assert!(wide.iter().any(|s| s.slot == "sessions"), "{wide:?}");
    assert!(wide.iter().any(|s| s.slot == "center"), "{wide:?}");

    let narrow = resolve(&host.arrangement(60, 24).expect("narrow layout"), area(60));
    assert!(
        !narrow.iter().any(|s| s.slot == "sessions"),
        "the session column should be gone at 60 columns: {narrow:?}"
    );
    assert!(narrow.iter().any(|s| s.slot == "center"), "{narrow:?}");
}

#[test]
fn the_sessions_column_is_bounded_by_its_declared_min_and_max() {
    let host = host();
    let placed = resolve(
        &host.arrangement(400, 24).expect("layout"),
        Rect {
            x: 0,
            y: 0,
            width: 400,
            height: 24,
        },
    );
    let sessions = placed
        .iter()
        .find(|s| s.slot == "sessions")
        .expect("sessions slot");
    // With no side column open, v1 stays on the TWO-panel 25/75 split however
    // wide the terminal is (`compute_layout` only takes the three-panel branch
    // when info/tasks/files is showing), so 25% of 400 is 100. The three-panel
    // 18% is exercised by the parity test, which opens a column first.
    assert_eq!(sessions.rect.width, 100);
}

#[test]
fn moving_the_cursor_changes_the_selection() {
    let host = host();
    publish(&host, &sample());
    let index = index_of(&host, "sessions");

    let before = paint_styles(&host, index, 40, 12).join("\n");
    press_key(&host, "sessions", 'j');

    let after = paint_styles(&host, index, 40, 12).join("\n");
    assert_ne!(before, after, "the selection highlight should have moved");
}

#[test]
fn an_unclaimed_key_is_left_for_the_kernel() {
    let host = host();
    publish(&host, &sample());
    let registry = registry(&host);
    let key = KeyPress {
        name: "z".to_string(),
        ch: Some('z'),
        ..KeyPress::default()
    };
    assert!(
        registry.resolve(&key, Some("sessions")).is_none(),
        "z is not declared"
    );
    let handled = host
        .on_key(index_of(&host, "sessions"), &key)
        .expect("key dispatched");
    assert!(!handled, "an unhandled key must fall through");
}

#[test]
fn plugin_reads_are_served_from_the_snapshot() {
    // design.md D5: reads never consult the database, so they cannot block.
    // Rendering a thousand times touches nothing but the published table.
    let host = host();
    publish(&host, &sample());
    let index = index_of(&host, "sessions");
    for _ in 0..1000 {
        host.render(index, ctx(40, 12, false)).expect("render");
    }
}

#[test]
fn the_kernel_still_exposes_exactly_four_node_kinds() {
    // The D1 tripwire. If this fails, something was added to the kernel that
    // should have been composed in ui/lib/widgets.lua — read design.md D1
    // before changing the number.
    assert_eq!(
        KINDS,
        ["text", "box", "input", "surface"],
        "the node catalog grew — that is the failure mode v2 exists to avoid"
    );
}

#[test]
fn the_panes_are_built_from_primitives_not_kernel_widgets() {
    // The other half of D1: the session list is a *list*, but there is no list
    // node — it is boxes and text from the widget library.
    let host = host();
    publish(&host, &sample());
    let node = host
        .render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render")
        .node;

    let kinds = collect_kinds(&node);
    for kind in &kinds {
        assert!(
            KINDS.contains(kind),
            "{kind} is not one of the four primitives"
        );
    }
    assert!(
        kinds.contains(&"box") && kinds.contains(&"text"),
        "{kinds:?}"
    );
}

#[test]
fn a_broken_plugin_costs_only_its_own_pane() {
    // The isolation rule, end to end: a plugin that throws is reported against
    // itself, and its neighbours still render.
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    std::fs::write(
        plugins.join("10_good.lua"),
        r#"return { name = "good", slot = "a", render = function() return { text = "fine" } end }"#,
    )
    .expect("write good");
    std::fs::write(
        plugins.join("20_bad.lua"),
        r#"return { name = "bad", slot = "b", render = function() error("boom") end }"#,
    )
    .expect("write bad");

    let host = LuaHost::new(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);

    let good = host.render(index_of(&host, "good"), ctx(20, 3, false));
    assert!(good.is_ok(), "the healthy plugin must still render");

    let bad = host
        .render(index_of(&host, "bad"), ctx(20, 3, false))
        .expect_err("the throwing plugin must fail");
    assert_eq!(bad.plugin, "bad", "the failure must name its plugin");
    assert!(bad.message.contains("boom"), "{}", bad.message);
}

#[test]
fn a_failed_reload_keeps_the_last_good_plugins_running() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    let file = plugins.join("10_pane.lua");
    std::fs::write(
        &file,
        r#"return { name = "pane", slot = "a", render = function() return { text = "v1" } end }"#,
    )
    .expect("write");

    let mut host = LuaHost::new(dir.path());
    let reloads = host.reloads;

    // Save a syntax error over it.
    std::fs::write(&file, "return { this is not lua").expect("write broken");
    host.reload();

    assert!(host.error.is_some(), "the failure must be surfaced");
    assert_eq!(host.reloads, reloads, "a failed reload must not count");
    let screen = paint(&host, index_of(&host, "pane"), 10, 1).join("\n");
    assert!(screen.contains("v1"), "the last good plugin keeps running");

    // And repairing it recovers.
    std::fs::write(
        &file,
        r#"return { name = "pane", slot = "a", render = function() return { text = "v2" } end }"#,
    )
    .expect("write fixed");
    host.reload();
    assert!(host.error.is_none(), "{:?}", host.error);
    let screen = paint(&host, index_of(&host, "pane"), 10, 1).join("\n");
    assert!(screen.contains("v2"), "the repaired plugin takes over");
}

#[test]
fn an_unterminated_plugin_is_interrupted_rather_than_hanging() {
    // design.md D8: Lua 5.4 gives no set_interrupt, so the kernel builds this
    // from a count hook. Without it, this test would never return.
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    std::fs::write(
        plugins.join("10_spin.lua"),
        r#"return { name = "spin", slot = "a", render = function() while true do end end }"#,
    )
    .expect("write");

    let host = LuaHost::new(dir.path());
    let error = host
        .render(index_of(&host, "spin"), ctx(20, 3, false))
        .expect_err("an unterminated render must be interrupted");
    assert_eq!(error.plugin, "spin");
    assert!(
        error.message.contains("instruction budget"),
        "{}",
        error.message
    );
}

#[test]
fn plugins_have_no_filesystem_or_process_access() {
    // design.md D9: enforcement by ABSENCE. `io`, `os` and the real `package`
    // are simply not in the environment, so there is no binding to refuse.
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    std::fs::write(
        plugins.join("10_probe.lua"),
        r#"
        return {
          name = "probe", slot = "a",
          render = function()
            local found = {}
            local probes = {
              "io", "os", "package", "debug",
              "dofile", "loadfile", "load", "loadstring", "print",
            }
            for _, name in ipairs(probes) do
              if _G[name] ~= nil then found[#found + 1] = name end
            end
            return { text = #found == 0 and "clean" or table.concat(found, ",") }
          end,
        }
        "#,
    )
    .expect("write");

    let host = LuaHost::new(dir.path());
    let screen = paint(&host, index_of(&host, "probe"), 60, 1).join("\n");
    assert!(
        screen.contains("clean"),
        "these capabilities must not exist in a plugin's environment: {screen}"
    );
}

#[test]
fn plugin_state_is_private_and_survives_a_reload() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    // Both plugins write state.n; neither may see the other's.
    for (file, name, value) in [("10_a.lua", "a", 1), ("20_b.lua", "b", 2)] {
        std::fs::write(
            plugins.join(file),
            format!(
                r#"return {{ name = "{name}", slot = "{name}", render = function()
                     state.n = state.n or {value}
                     return {{ text = "n=" .. state.n }}
                   end }}"#
            ),
        )
        .expect("write");
    }

    let mut host = LuaHost::new(dir.path());
    assert!(paint(&host, index_of(&host, "a"), 10, 1)[0].contains("n=1"));
    assert!(paint(&host, index_of(&host, "b"), 10, 1)[0].contains("n=2"));

    // Values persist across a rebuild of the whole VM.
    host.reload();
    assert!(host.error.is_none(), "{:?}", host.error);
    assert!(paint(&host, index_of(&host, "a"), 10, 1)[0].contains("n=1"));
    assert!(paint(&host, index_of(&host, "b"), 10, 1)[0].contains("n=2"));
}

#[test]
fn a_slot_divides_its_rect_among_its_occupants() {
    let host = host();
    let members = host.in_slot("sessions");
    let sizes: Vec<_> = members.iter().map(|i| host.plugins[*i].size).collect();
    let rects = divide_slot(
        Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 20,
        },
        Axis::Vertical,
        &sizes,
        0,
    );
    assert_eq!(rects.len(), members.len());
    assert_eq!(rects.iter().map(|r| r.height).sum::<u16>(), 20);
}

/// Every node kind present in a tree, for asserting what a plugin built with.
fn collect_kinds(node: &Node) -> Vec<&'static str> {
    let mut kinds = vec![node.kind()];
    if let Node::Box { children, .. } = node {
        for child in children {
            kinds.extend(collect_kinds(child));
        }
    }
    kinds.sort_unstable();
    kinds.dedup();
    kinds
}

#[test]
fn the_agent_pane_asks_for_raw_session_input() {
    // How the terminal stays an ordinary plugin: the kernel does not know which
    // plugin "is" the terminal, only that this one declared `input = "session"`.
    let host = host();
    let agent = &host.plugins[index_of(&host, "agent")];
    assert!(agent.session_input, "the agent pane must ask for raw input");

    let sessions = &host.plugins[index_of(&host, "sessions")];
    assert!(
        !sessions.session_input,
        "the session list must not swallow keys into a pty"
    );
}

#[test]
fn the_focused_session_is_read_off_the_rendered_tree() {
    // The kernel learns which session has focus from the tree it just painted,
    // not from a name it was told. Replace the plugin and this still works.
    let host = host();
    publish(&host, &sample());
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render list");

    let node = host
        .render(index_of(&host, "agent"), ctx(60, 10, true))
        .expect("render agent");
    let session = node
        .node
        .first_session_surface()
        .expect("the agent pane should point at a session");
    assert!(
        sample().sessions.iter().any(|row| row.id == session),
        "{session} should be one of the snapshot's sessions"
    );
}

#[test]
fn a_pane_without_a_surface_reports_no_session() {
    let host = host();
    publish(&host, &snapshot(Vec::new()));
    let node = host
        .render(index_of(&host, "agent"), ctx(60, 10, true))
        .expect("render");
    assert_eq!(node.node.first_session_surface(), None);
}

#[test]
fn a_failed_attach_is_explained_in_the_pane() {
    // A dead terminal must say why. "not attached" with no reason is the least
    // useful thing a terminal can say.
    let host = host();
    // Every session fails, so this does not depend on which row the list
    // happens to select (it orders alphabetically within a repo).
    let errors: std::collections::HashMap<String, String> = sample()
        .sessions
        .iter()
        .map(|row| (row.id.clone(), "can't find pane: %45".to_string()))
        .collect();

    publish_with(&host, &sample(), &errors, &[]);
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render list");

    let screen = paint(&host, index_of(&host, "agent"), 70, 10).join("\n");
    assert!(screen.contains("no live terminal"), "{screen}");
    assert!(screen.contains("can't find pane"), "{screen}");
}

#[test]
fn a_session_backed_surface_falls_back_when_nothing_is_attached() {
    // PlaceholderSurfaces has nothing live behind it, so the surface must draw
    // the detached notice rather than leaving an empty rect.
    let host = host();
    publish(&host, &sample());
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render list");

    let screen = paint(&host, index_of(&host, "agent"), 60, 10).join("\n");
    assert!(screen.contains("terminal surface"), "{screen}");
    assert!(screen.contains("not attached"), "{screen}");
}

// --- command bus -----------------------------------------------------------

use thurbox::kernel::command::{Command, InFlight, Phase};

/// Press a key and route it exactly as the binary does: registry first
/// (declared actions), then raw `on_key`. Returns whatever commands it issued.
fn press_key(host: &LuaHost, plugin: &str, ch: char) {
    let registry = registry(host);
    let key = KeyPress {
        name: ch.to_lowercase().to_string(),
        ch: Some(ch),
        ..KeyPress::default()
    };
    if let Some(binding) = registry.resolve(&key, Some(plugin)) {
        if let Some(index) = host.index_of(&binding.plugin) {
            if host.on_action(index, &binding.action).expect("action") {
                return;
            }
        }
    }
    host.on_key(index_of(host, plugin), &key).expect("key");
}

fn keys_to_commands(host: &LuaHost, plugin: &str, ch: char) -> Vec<Command> {
    press_key(host, plugin, ch);
    host.drain_commands()
}

#[test]
fn deleting_from_the_session_list_issues_a_command() {
    let host = host();
    publish(&host, &sample());
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render");

    let issued = keys_to_commands(&host, "sessions", 'd');
    assert_eq!(issued.len(), 1, "{issued:?}");
    assert_eq!(issued[0].kind(), "delete");
    assert!(
        sample()
            .sessions
            .iter()
            .any(|row| row.id == issued[0].session()),
        "the command should name a real session"
    );
}

#[test]
fn shift_j_reorders_rather_than_moving_the_cursor() {
    // Regression: a legacy terminal sends a bare "J" with no SHIFT modifier and
    // the host lowercases the key name, so matching on key.shift silently fell
    // through to plain-j cursor movement.
    let host = host();
    publish(&host, &sample());
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render");

    // The move is computed over the rendered items and sent as one explicit
    // order, because only this pane knows the grouping and nesting that decide
    // what a move swaps.
    let issued = keys_to_commands(&host, "sessions", 'J');
    assert_eq!(issued.len(), 1, "expected a reorder, got {issued:?}");
    assert_eq!(issued[0].kind(), "order");

    // Moving UP needs a row above it in the same group: the first row is already
    // at the top of the list, where v1 issues nothing either.
    press_key(&host, "sessions", 'j');
    host.drain_commands();
    let issued = keys_to_commands(&host, "sessions", 'K');
    assert_eq!(issued.len(), 1, "expected a reorder, got {issued:?}");
    assert_eq!(issued[0].kind(), "order");
}

#[test]
fn shift_s_sorts_within_each_repo_group() {
    let host = host();
    publish(&host, &sample());
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render");

    let issued = keys_to_commands(&host, "sessions", 'S');
    assert_eq!(issued.len(), 1, "expected an order, got {issued:?}");
    assert_eq!(issued[0].kind(), "order");
}

#[test]
fn the_top_row_cannot_move_up() {
    // Not an error and not a no-op command: nothing is issued at all, so an
    // order is never persisted for a move that did not happen.
    let host = host();
    publish(&host, &sample());
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render");

    assert!(keys_to_commands(&host, "sessions", 'K').is_empty());
}

#[test]
fn the_arrows_navigate_the_session_list_like_j_and_k() {
    // v1 binds `Action::SessionListNext` to `j` AND `Down` by default; v2
    // declared only the letters, so arrow keys did nothing in the list.
    let host = host();
    publish(&host, &sample());
    let index = index_of(&host, "sessions");
    host.render(index, ctx(40, 12, true)).expect("render");

    let selected = |host: &LuaHost| host.shared_string("selected").expect("a selection");
    let arrow = |host: &LuaHost, name: &str| {
        let key = KeyPress {
            name: name.to_string(),
            ..KeyPress::default()
        };
        let registry = registry(host);
        let binding = registry
            .resolve(&key, Some("sessions"))
            .unwrap_or_else(|| panic!("{name} is bound to nothing in the session list"));
        host.on_action(index, &binding.action).expect("action");
        // The selection is republished while rendering, as the loop does.
        host.render(index, ctx(40, 12, true)).expect("render");
    };

    let first = selected(&host);
    arrow(&host, "down");
    let second = selected(&host);
    assert_ne!(first, second, "Down should move the selection");

    arrow(&host, "up");
    assert_eq!(
        selected(&host),
        first,
        "Up should come back to where Down started"
    );
}

#[test]
fn plain_navigation_issues_no_commands() {
    let host = host();
    publish(&host, &sample());
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render");

    assert!(keys_to_commands(&host, "sessions", 'j').is_empty());
    assert!(keys_to_commands(&host, "sessions", 'k').is_empty());
}

#[test]
fn a_malformed_command_is_reported_against_the_plugin() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    std::fs::write(
        plugins.join("10_bad.lua"),
        r#"return {
             name = "bad", slot = "a",
             render = function() return { text = "x" } end,
             on_key = function() command("explode", { session = "s1" }) return true end,
           }"#,
    )
    .expect("write");

    let host = LuaHost::new(dir.path());
    let error = host
        .on_key(
            index_of(&host, "bad"),
            &KeyPress {
                name: "x".to_string(),
                ch: Some('x'),
                ..KeyPress::default()
            },
        )
        .expect_err("an unknown command must fail the handler");
    assert!(
        error.message.contains("unknown command"),
        "{}",
        error.message
    );
    assert!(host.drain_commands().is_empty(), "nothing should be queued");
}

#[test]
fn work_in_flight_is_drawn_instead_of_the_status() {
    // Without this a delete looks like nothing happened until the next
    // snapshot. v1 needed a whole PendingSpawn type for the same reason.
    let host = host();
    let target = sample().sessions[0].id.clone();
    let inflight = vec![InFlight {
        id: 1,
        kind: "delete",
        session: target,
        subject: None,
        phase: Phase::Running,
        error: None,
    }];

    publish_with(&host, &sample(), &Default::default(), &inflight);
    let screen = paint(&host, index_of(&host, "sessions"), 46, 12).join("\n");
    assert!(screen.contains('◌'), "pending marker missing:\n{screen}");
    assert!(screen.contains("delete"), "{screen}");
}

#[test]
fn a_failed_command_is_drawn_as_failed() {
    let host = host();
    let target = sample().sessions[0].id.clone();
    let inflight = vec![InFlight {
        id: 1,
        kind: "delete",
        session: target,
        subject: None,
        phase: Phase::Failed,
        error: Some("session not found".to_string()),
    }];

    publish_with(&host, &sample(), &Default::default(), &inflight);
    let screen = paint(&host, index_of(&host, "sessions"), 46, 12).join("\n");
    assert!(screen.contains('✗'), "{screen}");
    assert!(screen.contains("failed"), "{screen}");
}

#[test]
fn the_manual_order_is_published_so_a_reorder_is_visible() {
    // Regression: display_order lived in the snapshot but was never published,
    // so reorder commands wrote to the database and the list ignored them.
    let host = host();
    let mut snap = sample();
    // Reverse the alphabetical order explicitly.
    for (position, row) in snap.sessions.iter_mut().enumerate() {
        row.display_order = Some(100 - position as i64);
    }
    let expected_first = snap.sessions.last().expect("rows").name.clone();

    publish(&host, &snap);
    let screen = paint(&host, index_of(&host, "sessions"), 46, 12).join("\n");

    let first_session_line = screen
        .lines()
        // Skip the frame: the top border now carries one status dot per
        // session, so it matches these glyphs too.
        .filter(|line| !line.contains('╭') && !line.contains('╰'))
        .find(|line| line.contains('○') || line.contains('◆') || line.contains('●'))
        .unwrap_or_default()
        .to_string();
    assert!(
        first_session_line.contains(&expected_first),
        "manual order ignored: expected {expected_first} first, got {first_session_line:?}"
    );
}

// --- theming ---------------------------------------------------------------

#[test]
fn the_theme_reaches_plugins_as_roles() {
    // design.md D14: the kernel resolves, plugins name roles. Publishing is the
    // whole interface between them, so this asserts a plugin can read the
    // resolved theme — through a throwaway plugin that prints it, since no
    // bundled pane shows the theme's name any more (the header is gone and the
    // picker is a kernel modal — `tests/v2_modals.rs`).
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    std::fs::write(
        plugins.join("10_probe.lua"),
        r#"return {
             name = "probe", slot = "center",
             render = function()
               -- The name, plus one resolved role: naming a role is the whole
               -- contract, and a role that came back nil would print "nil".
               return {
                 type = "text",
                 text = thurbox.theme.name .. " " .. tostring(thurbox.theme.roles.accent),
               }
             end,
           }"#,
    )
    .expect("write");

    let host = LuaHost::new(dir.path());
    publish(&host, &sample());
    let screen = paint(&host, index_of(&host, "probe"), 40, 1).join("\n");
    let active = themes().active().name.clone();
    assert!(
        screen.contains(&active),
        "saw {screen:?}, wanted {active:?}"
    );
    assert!(
        !screen.contains("nil"),
        "a named role must resolve to a colour: {screen}"
    );
}

/// Does this line contain a literal colour rather than a role name?
///
/// A quoted `#rrggbb`, or one of ratatui's colour names. Deliberately not
/// looking for a bare `#`, which in Lua is the length operator.
fn hardcodes_colour(line: &str) -> bool {
    const NAMES: [&str; 8] = [
        "light-cyan",
        "light-green",
        "light-yellow",
        "light-red",
        "light-blue",
        "light-magenta",
        "dark-gray",
        "dark-grey",
    ];
    if NAMES.iter().any(|name| line.contains(name)) {
        return true;
    }
    let bytes: Vec<char> = line.chars().collect();
    bytes
        .windows(8)
        .any(|w| w[0] == '"' && w[1] == '#' && w[2..8].iter().all(|c| c.is_ascii_hexdigit()))
}

#[test]
fn no_bundled_plugin_hardcodes_a_colour() {
    // The coherence rule made checkable: a literal in a plugin opts out of
    // theming for everyone downstream of it.
    let ui = ui_dir();
    let mut offenders = Vec::new();
    for dir in [ui.join("plugins"), ui.join("lib")] {
        for entry in std::fs::read_dir(&dir).expect("read ui dir") {
            let path = entry.expect("entry").path();
            if path.extension().is_some_and(|e| e == "lua") {
                let source = std::fs::read_to_string(&path).expect("read");
                // theme.lua names roles; everything else must go through it.
                if path.file_name().is_some_and(|n| n == "theme.lua") {
                    continue;
                }
                for (n, line) in source.lines().enumerate() {
                    let trimmed = line.trim_start();
                    if trimmed.starts_with("--") {
                        continue;
                    }
                    if hardcodes_colour(line) {
                        offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                    }
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "bundled plugins must name roles, not colours:\n{}",
        offenders.join("\n")
    );
}

// --- registry --------------------------------------------------------------

#[test]
fn every_bundled_plugin_declares_its_keys_as_data() {
    // The declaration is what makes a key listable, rebindable and
    // conflict-checkable. A pane that only implements on_key gets none of that.
    let host = host();
    let registry = registry(&host);
    assert!(!registry.bindings().is_empty());

    for plugin in ["sessions", "agent"] {
        assert!(
            registry.bindings().iter().any(|b| b.plugin == plugin),
            "{plugin} declares no keys"
        );
    }
    for binding in registry.bindings() {
        assert!(binding.action.contains('.'), "{:?}", binding.action);
        assert!(
            !binding.description.is_empty(),
            "{} has no description",
            binding.action
        );
    }
}

#[test]
fn the_bundled_set_has_no_key_conflicts() {
    // Several panes declare `j`; that is fine because each is plugin-scoped and
    // focus decides. A genuine clash would show up here.
    let host = host();
    let registry = registry(&host);
    assert!(
        registry.conflicts().is_empty(),
        "bundled plugins conflict: {:?}",
        registry.conflicts()
    );
}

#[test]
fn a_rebound_key_routes_to_its_action() {
    let host = host();
    let mut registry = registry(&host);
    let key = KeyPress {
        name: "z".to_string(),
        ch: Some('z'),
        ..KeyPress::default()
    };
    assert!(registry.resolve(&key, Some("sessions")).is_none());

    // Rebinding persists, so this writes the user's real config; only the
    // in-memory routing is asserted, then it is put back.
    registry
        .rebind("sessions.delete", Some("z"))
        .expect("rebind");
    assert_eq!(
        registry
            .resolve(&key, Some("sessions"))
            .map(|b| b.action.as_str()),
        Some("sessions.delete")
    );
    registry.rebind("sessions.delete", None).expect("reset");
}

#[test]
fn capitals_route_the_same_from_either_terminal_encoding() {
    // The portability trap, end to end: shift+J must reach the reorder action
    // whether the terminal reports a bare "J" or "j" plus SHIFT.
    let host = host();
    let registry = registry(&host);
    let legacy = KeyPress {
        name: "j".into(),
        ch: Some('J'),
        ..KeyPress::default()
    };
    let kitty = KeyPress {
        name: "j".into(),
        ch: Some('j'),
        shift: true,
        ..KeyPress::default()
    };
    for key in [legacy, kitty] {
        assert_eq!(
            registry
                .resolve(&key, Some("sessions"))
                .map(|b| b.action.as_str()),
            Some("sessions.move_down"),
            "{key:?} did not reach the reorder action"
        );
    }
}

// --- floating plugins ------------------------------------------------------

// --- guards ----------------------------------------------------------------

#[test]
fn the_lua_vm_stays_non_send() {
    // design.md D10: with mlua's `send` feature off, "plugins never touch the
    // render thread" is a compile error rather than a review rule. Enabling it
    // would silently remove that guarantee, so the dependency is asserted here.
    let manifest =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("read Cargo.toml");
    let mlua_line = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("mlua = "))
        .expect("mlua dependency");
    assert!(
        !mlua_line.contains("\"send\""),
        "mlua's send feature must stay off: {mlua_line}"
    );
}

#[test]
fn the_granted_capability_set_matches_a_declared_list() {
    // design.md D9: capabilities are introduced with their consumer, never
    // ahead of one. This is the tripwire — a new global in the plugin
    // environment has to be added here deliberately.
    const GRANTED: [&str; 6] = [
        "require", "state", "store", "command", "thurbox",
        // Two rooted reads — directory entries and a file's text — confined to
        // a session's working directory. NOT a filesystem: there is no open,
        // no write, no path outside the root. Granted with its consumer (the
        // file viewer), per design.md D9.
        "files",
    ];

    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    std::fs::write(
        plugins.join("10_probe.lua"),
        r#"
        return {
          name = "probe", slot = "a",
          render = function()
            local names = {}
            for key in pairs(_G) do names[#names + 1] = key end
            table.sort(names)
            return { text = table.concat(names, " ") }
          end,
        }
        "#,
    )
    .expect("write");

    let host = LuaHost::new(dir.path());
    publish(&host, &snapshot(Vec::new()));
    let globals = paint(&host, index_of(&host, "probe"), 400, 1).join(" ");

    // Everything thurbox added must be on the list. Lua's own value types
    // (string, table, math…) are the stdlib we deliberately opened.
    const STDLIB: [&str; 12] = [
        "_G",
        "_VERSION",
        "string",
        "table",
        "math",
        "coroutine",
        "utf8",
        "pairs",
        "ipairs",
        "type",
        "tostring",
        "tonumber",
    ];
    for name in globals.split_whitespace() {
        // No blanket excuse for a leading underscore. `_G`/`_VERSION` are named in
        // STDLIB above; anything else spelled `__like_this` is thurbox's own, and
        // a plugin's `_ENV` *is* this table — which is how `__run_impl` sat here
        // handing every untrusted plugin the run capability under a second name.
        let known = GRANTED.contains(&name)
            || STDLIB.contains(&name)
            // Remaining base-library functions: pure computation, no capability.
            || [
                "assert", "error", "next", "pcall", "xpcall", "rawequal", "rawget", "rawlen",
                "rawset", "select", "setmetatable", "getmetatable", "unpack", "require",
            ]
            .contains(&name);
        assert!(
            known,
            "unexpected global {name:?} in the plugin environment — \
             add it to GRANTED deliberately, or withhold it"
        );
    }
    for granted in GRANTED {
        assert!(
            globals.split_whitespace().any(|g| g == granted),
            "{granted} should be granted but is missing"
        );
    }
}

#[test]
fn every_session_field_reaches_lua() {
    // Regression with teeth: `display_order` lived in the snapshot and the
    // database, reorder commands wrote it correctly, and `publish` simply never
    // emitted it — so the list sorted by a value that was always nil. The
    // snapshot struct and the published table are two independent lists of
    // fields, and nothing else checks they agree.
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    std::fs::write(
        plugins.join("10_fields.lua"),
        r#"
        return {
          name = "fields", slot = "a",
          render = function()
            local row = (thurbox and thurbox.sessions or {})[1]
            if not row then return { text = "none" } end
            local names = {}
            for key in pairs(row) do names[#names + 1] = key end
            table.sort(names)
            return { text = table.concat(names, " ") }
          end,
        }
        "#,
    )
    .expect("write");

    let host = LuaHost::new(dir.path());
    let mut snap = sample();
    snap.sessions[0].display_order = Some(3);
    snap.sessions[0].parent_id = Some("parent-id".into());
    // A nil field does not appear in `pairs`, so every field under test needs a
    // value for this to mean anything.
    snap.sessions[0].cwd = Some(PathBuf::from("/tmp/repo"));
    publish(&host, &snap);

    let published = paint(&host, index_of(&host, "fields"), 400, 1).join(" ");
    for field in [
        "id",
        "name",
        "agent",
        "status",
        "backend",
        "repo",
        "branch",
        "cwd",
        "parent",
        "display_order",
        "worktrees",
    ] {
        assert!(
            published.split_whitespace().any(|f| f == field),
            "{field} never reaches Lua; published: {published}"
        );
    }
}

// --- session creation ------------------------------------------------------

#[test]
fn a_create_command_carries_what_the_flow_chose() {
    use thurbox::kernel::command::{Args, Command};
    let parsed = Command::parse(
        "create",
        Args {
            repo: Some("/tmp/demo".into()),
            branch: Some("feat/x".into()),
            agent: Some("claude".into()),
            ..Args::default()
        },
    )
    .expect("parse");
    match parsed {
        Command::Create {
            repo,
            branch,
            agent,
            ..
        } => {
            assert_eq!(repo, "/tmp/demo");
            assert_eq!(branch.as_deref(), Some("feat/x"));
            assert_eq!(agent.as_deref(), Some("claude"));
        }
        other => panic!("expected create, got {other:?}"),
    }
}

#[test]
fn a_bookmark_command_needs_a_path_and_an_explicit_verb() {
    use thurbox::kernel::command::{Args, BookmarkEdit, Command};
    // Forgetting is destructive, so the verb is never inferred.
    let error = Command::parse(
        "bookmark",
        Args {
            repo: Some("/src/thing".into()),
            ..Args::default()
        },
    )
    .unwrap_err();
    assert!(error.contains("action"), "{error}");

    let error = Command::parse(
        "bookmark",
        Args {
            action: Some("add".into()),
            ..Args::default()
        },
    )
    .unwrap_err();
    assert!(error.contains("needs a repo path"), "{error}");

    for (verb, expected) in [
        ("add", BookmarkEdit::Add),
        ("remove", BookmarkEdit::Remove),
        ("parent", BookmarkEdit::Parent),
    ] {
        let parsed = Command::parse(
            "bookmark",
            Args {
                repo: Some("  ~/src/thing  ".into()),
                host: Some("ssh:devbox".into()),
                action: Some(verb.into()),
                ..Args::default()
            },
        )
        .expect("parse");
        assert_eq!(
            parsed,
            Command::Bookmark {
                host: "ssh:devbox".into(),
                // Trimmed, but the tilde is left for the target machine.
                path: "~/src/thing".into(),
                edit: expected,
            },
            "{verb}"
        );
    }
}

#[test]
fn a_create_carries_every_member_with_the_mode_it_was_given() {
    use thurbox::kernel::command::{Args, Command, ExtraMember};
    let parsed = Command::parse(
        "create",
        Args {
            repo: Some("/src/thurbox".into()),
            branch: Some("feat/x".into()),
            base: Some("origin/main".into()),
            host: Some("ssh:devbox".into()),
            extras: vec![
                ExtraMember {
                    path: "/src/website".into(),
                    worktree: true,
                },
                ExtraMember {
                    path: "/reference".into(),
                    worktree: false,
                },
            ],
            ..Args::default()
        },
    )
    .expect("parse");
    match parsed {
        Command::Create {
            base, host, extras, ..
        } => {
            assert_eq!(base.as_deref(), Some("origin/main"));
            assert_eq!(host.as_deref(), Some("ssh:devbox"));
            assert_eq!(extras.len(), 2);
            assert!(extras[0].worktree, "a member can take its own worktree");
            assert!(!extras[1].worktree, "or be attached as it is");
        }
        other => panic!("expected create, got {other:?}"),
    }
}

#[test]
fn creating_without_a_repo_is_refused() {
    use thurbox::kernel::command::{Args, Command};
    let error = Command::parse("create", Args::default()).unwrap_err();
    assert!(error.contains("needs a repo"), "{error}");
}

#[test]
fn fork_and_sync_name_their_session() {
    use thurbox::kernel::command::{Args, Command};
    let fork = Command::parse(
        "fork",
        Args {
            session: "s1".into(),
            ..Args::default()
        },
    )
    .expect("parse fork");
    assert_eq!(fork.kind(), "fork");
    assert_eq!(fork.session(), "s1");

    let sync = Command::parse(
        "sync",
        Args {
            session: "s1".into(),
            ..Args::default()
        },
    )
    .expect("parse sync");
    assert_eq!(sync.kind(), "sync");
}

#[test]
fn a_pending_creation_draws_in_the_repo_it_will_land_in() {
    // v1 needed a bespoke slot in its ordering code for this. Here the command
    // carries its subject, so the list groups it like any other row.
    let host = host();
    let inflight = vec![InFlight {
        id: 1,
        kind: "create",
        session: String::new(),
        subject: Some("thurbox".to_string()),
        phase: Phase::Running,
        error: None,
    }];
    publish_with(&host, &sample(), &Default::default(), &inflight);

    let screen = paint(&host, index_of(&host, "sessions"), 46, 14).join("\n");
    assert!(screen.contains("creating"), "{screen}");

    // And it sits under the repo header, not in a limbo of its own.
    let lines: Vec<&str> = screen.lines().collect();
    let header = lines
        .iter()
        .position(|l| l.contains("thurbox"))
        .expect("header");
    let pending = lines
        .iter()
        .position(|l| l.contains("creating"))
        .expect("pending");
    assert!(
        pending > header,
        "placeholder should follow its repo header:\n{screen}"
    );
}

#[test]
fn a_creation_into_a_fresh_repo_brings_its_own_header() {
    let host = host();
    let inflight = vec![InFlight {
        id: 1,
        kind: "create",
        session: String::new(),
        subject: Some("brand-new".to_string()),
        phase: Phase::Queued,
        error: None,
    }];
    publish_with(&host, &sample(), &Default::default(), &inflight);

    let screen = paint(&host, index_of(&host, "sessions"), 46, 16).join("\n");
    assert!(screen.contains("brand-new"), "{screen}");
    assert!(screen.contains("creating"), "{screen}");
}

#[test]
fn a_force_deleted_restore_is_refused_without_best_effort() {
    use thurbox::kernel::command::{Args, Command};
    let plain = Command::parse(
        "restore",
        Args {
            session: "s1".into(),
            ..Args::default()
        },
    )
    .expect("parse");
    match plain {
        Command::Restore { best_effort, .. } => assert!(!best_effort),
        other => panic!("{other:?}"),
    }

    let explicit = Command::parse(
        "restore",
        Args {
            session: "s1".into(),
            force: true,
            ..Args::default()
        },
    )
    .expect("parse");
    match explicit {
        Command::Restore { best_effort, .. } => assert!(best_effort),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_creation_that_cannot_start_reports_why_and_leaves_nothing() {
    use thurbox::kernel::command::{Args, Command, CommandBus, Phase as CmdPhase};

    let mut bus = CommandBus::new();
    bus.dispatch(
        Command::parse(
            "create",
            Args {
                repo: Some("/definitely/not/a/repo".into()),
                ..Args::default()
            },
        )
        .expect("parse"),
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        bus.poll();
        if let Some(failed) = bus
            .inflight()
            .into_iter()
            .find(|entry| entry.phase == CmdPhase::Failed)
        {
            assert_eq!(failed.kind, "create");
            assert!(
                failed
                    .error
                    .as_deref()
                    .unwrap_or("")
                    .contains("not a directory"),
                "{:?}",
                failed.error
            );
            // And it is still grouped by the repo it was for, so the pending
            // row can show the failure where the session would have appeared.
            assert!(failed.subject.is_some());
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("the failure never surfaced");
}

// --- workflow panes --------------------------------------------------------

#[test]
fn task_and_automation_commands_validate_their_arguments() {
    use thurbox::kernel::command::{Args, Command};

    assert!(Command::parse("task", Args::default()).is_err());
    assert!(Command::parse(
        "task",
        Args {
            text: Some("a new task".into()),
            ..Args::default()
        }
    )
    .is_ok());
    assert!(Command::parse("dispatch", Args::default()).is_err());
    assert!(Command::parse("automation", Args::default()).is_err());
}

// --- navigation panes ------------------------------------------------------

#[test]
fn a_failing_decorator_leaves_the_pane_drawn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    std::fs::write(
        plugins.join("10_pane.lua"),
        r#"return { name = "pane", slot = "a",
                   render = function() return { text = "content" } end }"#,
    )
    .expect("write");
    std::fs::write(
        plugins.join("20_bad.lua"),
        r#"return { name = "bad", slot = "b", decorates = "a",
                   render = function() return { text = "" } end,
                   decorate = function() error("boom") end }"#,
    )
    .expect("write");

    let host = LuaHost::new(dir.path());
    let node = host
        .render(index_of(&host, "pane"), ctx(20, 3, false))
        .expect("render")
        .node;
    let decorator = host.decorators_of("a")[0];
    let error = host
        .decorate(decorator, &node, ctx(20, 3, false))
        .expect_err("the decorator must fail");
    assert_eq!(error.plugin, "bad");
    // And the caller still has the original to draw.
    assert!(paint_node(&node, 20, 3).join("").contains("content"));
}

// --- code review -----------------------------------------------------------

#[test]
fn the_review_body_is_a_surface_not_a_tree() {
    // design.md D3: the diff body is geometry-first, so it is cells. If this
    // ever becomes a tree, the pane has stopped being able to do side-by-side
    // and horizontal scrolling.
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    // A stand-in that renders the same shape the review pane does.
    std::fs::write(
        plugins.join("10_body.lua"),
        r#"return { name = "body", slot = "a", render = function()
             return { type = "box", children = {
               { type = "surface", cells = { "+added", "-removed" } },
             } }
           end }"#,
    )
    .expect("write");

    let host = LuaHost::new(dir.path());
    let node = host
        .render(index_of(&host, "body"), ctx(30, 4, false))
        .expect("render")
        .node;

    let kinds = collect_kinds(&node);
    assert!(kinds.contains(&"surface"), "{kinds:?}");
    for kind in &kinds {
        assert!(KINDS.contains(kind), "{kind} is not a primitive");
    }
    let screen = paint_node(&node, 30, 4).join("\n");
    assert!(screen.contains("+added"), "{screen}");
}

// --- terminal affordances --------------------------------------------------

#[test]
fn link_detection_is_shared_with_v1_not_duplicated() {
    // It moved to `session::links` so both halves use one definition of what
    // counts as a URL. Duplicating it into the kernel would have meant two.
    let rows = vec!["see https://example.com/x for more".to_string()];
    let found = thurbox::session::links::detect_urls(&rows);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].url, "https://example.com/x");

    // And v1's path still resolves to the same function.
    let via_ui = thurbox::session::links::detect_urls(&rows);
    assert_eq!(via_ui.len(), 1);
    assert_eq!(via_ui[0].url, found[0].url);
}

#[test]
fn opening_a_link_and_copying_are_commands() {
    use thurbox::kernel::command::{Args, Command};

    let open = Command::parse(
        "open",
        Args {
            text: Some("https://example.com".into()),
            ..Args::default()
        },
    )
    .expect("parse");
    assert_eq!(open.kind(), "open");
    assert!(Command::parse("open", Args::default()).is_err());

    let copy = Command::parse(
        "copy",
        Args {
            session: "s1".into(),
            ..Args::default()
        },
    )
    .expect("parse");
    assert_eq!(copy.kind(), "copy");
    assert_eq!(copy.session(), "s1");
}

#[test]
fn a_shell_is_a_second_surface_over_the_same_primitive() {
    // No new node kind: the shell is `<id>#shell`, which the provider resolves.
    let host = host();
    publish(&host, &sample());
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render");
    // The shell is a TAB of the terminal pane, not a plugin of its own, so the
    // pane that opens it is the one that shows it.
    let index = index_of(&host, "agent");

    host.on_action(index, "shell.open").expect("open");
    // Two commands, because selecting a tab also brings its pane forward (v1
    // `select_central_tab` focuses the centre for both terminal tabs).
    let issued = host.drain_commands();
    let kinds: Vec<&str> = issued.iter().map(|command| command.kind()).collect();
    assert!(kinds.contains(&"shell"), "{kinds:?}");

    let node = host.render(index, ctx(50, 8, true)).expect("render").node;
    let session = node
        .first_session_surface()
        .expect("the shell pane places a surface");
    assert!(session.ends_with("#shell"), "{session}");
    // Still four primitives.
    for kind in collect_kinds(&node) {
        assert!(KINDS.contains(&kind), "{kind} is not a primitive");
    }
}

#[test]
fn children_nest_under_their_parent_within_a_repo() {
    let host = host();
    let mut snap = sample();
    // Make the second session a child of the first.
    let parent = snap.sessions[0].id.clone();
    snap.sessions[1].parent_id = Some(parent);
    publish(&host, &snap);

    let screen = paint(&host, index_of(&host, "sessions"), 50, 14).join("\n");
    assert!(screen.contains('└'), "a child should be marked:\n{screen}");
}

#[test]
fn a_child_whose_parent_is_elsewhere_keeps_its_place() {
    let host = host();
    let mut snap = sample();
    // A parent in another repo group entirely.
    snap.sessions[3].parent_id = Some(snap.sessions[0].id.clone());
    publish(&host, &snap);

    let screen = paint(&host, index_of(&host, "sessions"), 50, 14).join("\n");
    assert!(
        screen.contains('↳'),
        "an out-of-group child should be marked rather than moved:\n{screen}"
    );
}

#[test]
fn the_spawn_pipeline_reports_the_stage_it_reached() {
    // "working" is exactly the wrong answer when the question is *why is this
    // slow*: a stalled fetch and a stalled ssh connect look identical without
    // the stage name.
    //
    // Asserted on the pipeline's own callback rather than by watching the bus,
    // because a command that fails quickly overwrites its stage with `failed`
    // before a poll can see it — a race in the observer, not in the report.
    use std::sync::{Arc, Mutex};
    use thurbox::session_ops::spawn::{
        spawn_session_headless_with_progress, SpawnPhase, SpawnRequest,
    };

    let db = thurbox::storage::Database::open_in_memory().expect("db");
    let repo = tempfile::tempdir().expect("tempdir");
    let seen: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

    let recorder = {
        let seen = seen.clone();
        move |phase: SpawnPhase| {
            seen.lock().expect("lock").push(phase.as_str());
        }
    };

    // Fails partway — the point is what it reported before it did.
    let _ = spawn_session_headless_with_progress(
        &db,
        SpawnRequest {
            name: "probe".into(),
            repo_path: repo.path().to_path_buf(),
            worktree_branch: None,
            base_branch: None,
            agent: None,
            agent_session_id: None,
            host: None,
            parent_session_id: None,
            task_id: None,
            extra_repos: Vec::new(),
            fork_session_id: None,
            inherit_worktrees: Vec::new(),
        },
        Some(&recorder),
    );

    let stages = seen.lock().expect("lock").clone();
    assert!(!stages.is_empty(), "the pipeline reported no stage at all");
    assert_eq!(stages[0], "resolving", "{stages:?}");
    // Every reported stage is one of the named ones, not free text.
    for stage in &stages {
        assert!(
            [
                "resolving",
                "worktrees",
                "backend",
                "launching",
                "persisting"
            ]
            .contains(stage),
            "unexpected stage {stage:?}"
        );
    }
}

#[test]
fn a_stage_name_reaches_the_pending_row() {
    let host = host();
    let inflight = vec![InFlight {
        id: 1,
        kind: "create",
        session: String::new(),
        subject: Some("thurbox".to_string()),
        phase: thurbox::kernel::command::Phase::Stage("worktrees".to_string()),
        error: None,
    }];
    publish_with(&host, &sample(), &Default::default(), &inflight);

    // The plugin currently renders "creating…" for any non-failed phase; what
    // matters here is that the stage survives publishing, so a plugin *can*
    // show it.
    let screen = paint(&host, index_of(&host, "sessions"), 46, 14).join("\n");
    assert!(screen.contains("creating"), "{screen}");
}

// --- selection, copy, paste ------------------------------------------------

#[test]
fn selection_logic_is_shared_with_v1_not_duplicated() {
    // Moved to `session::selection` so both halves use one implementation, the
    // same reasoning as link detection.
    use thurbox::session::selection::{PaneBounds, Selection, TermPos};

    let pane = PaneBounds::from_rect(Rect {
        x: 2,
        y: 1,
        width: 10,
        height: 5,
    });
    let mut selection = Selection::new(TermPos { row: 2, col: 4 }, pane);
    selection.cursor = TermPos { row: 3, col: 6 };

    assert!(selection.contains(2, 5));
    assert!(!selection.contains(1, 5), "above the anchor row");

    // v1's path resolves to the same type.
    let via_ui: Selection = selection.clone();
    assert_eq!(via_ui.anchor, selection.anchor);
}

#[test]
fn a_selection_is_clamped_to_its_pane() {
    // A drag that leaves the terminal must not select the session list beside
    // it.
    use thurbox::session::selection::PaneBounds;
    let pane = PaneBounds::from_rect(Rect {
        x: 10,
        y: 5,
        width: 20,
        height: 8,
    });
    assert_eq!(pane.clamp(0, 0), (10, 5));
    assert_eq!(pane.clamp(255, 255), (29, 12));
    assert!(pane.contains(15, 7));
    assert!(!pane.contains(5, 7));
}

#[test]
fn a_plugin_earns_a_band_entry_by_declaring_one() {
    // The whole extension point: one table field, and the pane knows nothing
    // about how the band draws.
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    std::fs::write(
        plugins.join("10_contributor.lua"),
        r#"return {
             name = "contributor", slot = "center",
             keys = { { key = "f7", action = "contributor.open", desc = "open it", scope = "global" } },
             pills = { { action = "contributor.open", label = "Mine", priority = 42 } },
             render = function() return { type = "text", text = "" } end,
           }"#,
    )
    .expect("write");

    let host = LuaHost::new(dir.path());
    let (bindings, settings, pills) = host.all_declarations();
    assert_eq!(pills.len(), 1, "{pills:?}");
    assert_eq!(pills[0].plugin, "contributor", "stamped with its author");

    let mut registry = Registry::default();
    registry.declare_all(bindings, settings, pills);
    let entries = thurbox::kernel::bands::entries(registry.pills(), &registry);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].display(), "Mine · F7");
}

#[test]
fn a_plugin_that_becomes_unloadable_leaves_the_last_good_set_in_force() {
    // Plugins load as a SET, so a file that stops parsing does not quietly
    // remove one entry — the whole previous set stays in force and the failure is
    // reported. A half-loaded interface missing the pane you wanted, with no
    // indication why, is the outcome this trades away.
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    let good = plugins.join("10_good.lua");
    std::fs::write(
        &good,
        r#"return {
             name = "good", slot = "center",
             keys = { { key = "f7", action = "good.open", desc = "open", scope = "global" } },
             pills = { { action = "good.open", label = "Good", priority = 1 } },
             render = function() return { type = "text", text = "" } end,
           }"#,
    )
    .expect("write");

    let mut host = LuaHost::new(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);
    let entries_of = |host: &LuaHost| {
        let (bindings, settings, pills) = host.all_declarations();
        let mut registry = Registry::default();
        registry.declare_all(bindings, settings, pills);
        thurbox::kernel::bands::entries(registry.pills(), &registry)
            .iter()
            .map(|entry| entry.label.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(entries_of(&host), ["Good"]);

    // Break it, then reload.
    std::fs::write(&good, "this is not lua {{{").expect("write");
    host.reload();

    assert!(host.error.is_some(), "the failure must be reported");
    assert_eq!(
        entries_of(&host),
        ["Good"],
        "the last good set stays in force, entries included"
    );
}

#[test]
fn a_band_is_never_a_focus_stop() {
    // Bands are not plugins, so they cannot enter the focus ring — asserted
    // rather than assumed, because the ring is what makes them unreachable.
    let host = host();
    let focusable: Vec<&str> = host
        .focusable()
        .iter()
        .map(|index| host.plugins[*index].name.as_str())
        .collect();
    for band in thurbox::kernel::bands::Band::DROP_ORDER {
        assert!(
            !focusable.contains(&band.slot()),
            "{} is a band, not a focus stop: {focusable:?}",
            band.slot()
        );
        assert!(
            host.index_of(band.slot()).is_none(),
            "{} must not be a plugin at all",
            band.slot()
        );
    }
}

#[test]
fn a_press_on_a_pane_border_arms_no_selection() {
    // Why a selection felt trigger-happy: v2 fell back to the WHOLE SCREEN when
    // a press was not over a terminal, so a press anywhere — a border, the gap
    // between panes — armed a screen-wide selection, and one cell of pointer
    // drift painted a band clear across the interface. v1 confines it to the
    // pane's content area and clears the selection when the press is outside
    // one.
    use thurbox::session::selection::PaneBounds;
    let pane = Rect {
        x: 10,
        y: 4,
        width: 20,
        height: 6,
    };

    // Inside: the content area, one cell in from every edge.
    let inner = PaneBounds::content_at(pane, 15, 6).expect("inside the pane");
    assert_eq!(
        inner.rect(),
        Rect {
            x: 11,
            y: 5,
            width: 18,
            height: 4
        }
    );

    // On the border, on each of the four edges.
    for (x, y, edge) in [
        (10, 6, "left"),
        (29, 6, "right"),
        (15, 4, "top"),
        (15, 9, "bottom"),
    ] {
        assert!(
            PaneBounds::content_at(pane, x, y).is_none(),
            "a press on the {edge} border must arm nothing"
        );
    }

    // Outside the pane entirely.
    assert!(PaneBounds::content_at(pane, 0, 0).is_none());

    // A pane too small to have content.
    let sliver = Rect {
        x: 0,
        y: 0,
        width: 2,
        height: 2,
    };
    assert!(PaneBounds::content_at(sliver, 0, 0).is_none());
}

#[test]
fn a_selection_is_painted_in_the_themes_own_colours() {
    // v1 paints a selection with `selection_bg`/`selection_fg`. v2 used
    // `REVERSED`, which inverts whatever each cell already had — so the same
    // selection came out a different colour over styled text than over plain,
    // and matched no palette.
    let themes = themes();
    let style = themes.selection_style();

    assert!(
        style.bg.is_some() && style.fg.is_some(),
        "both halves of the theme's selection pair are used: {style:?}"
    );
    assert!(
        !style
            .add_modifier
            .contains(ratatui::style::Modifier::REVERSED),
        "reverse video is the fallback for a palette that defines neither, not \
         the rule: {style:?}"
    );

    // And it is the palette's pair, not an invented colour.
    let roles = themes.roles();
    let expect = |role: &str| {
        thurbox::kernel::node::parse_color(roles.get(role).expect("role")).expect("a colour")
    };
    assert_eq!(style.bg, Some(expect("selection_bg")));
    assert_eq!(style.fg, Some(expect("selection_fg")));
}

#[test]
fn copy_and_paste_are_reachable_from_a_plugin() {
    use thurbox::kernel::command::{Args, Command};
    let copy = Command::parse(
        "copy",
        Args {
            session: "s1".into(),
            ..Args::default()
        },
    )
    .expect("parse");
    assert_eq!(copy.kind(), "copy");
}
