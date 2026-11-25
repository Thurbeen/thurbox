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

fn themes() -> Themes {
    Themes::load(None)
}

fn publish(host: &LuaHost, snapshot: &Snapshot) {
    publish_with(host, snapshot, &Default::default(), &[]);
}

fn publish_printing(host: &LuaHost, snapshot: &Snapshot, printing: &[&str]) {
    let printing: std::collections::HashSet<String> =
        printing.iter().map(|s| (*s).to_string()).collect();
    publish_full(host, snapshot, &Default::default(), &[], &printing);
}

fn publish_with(
    host: &LuaHost,
    snapshot: &Snapshot,
    attach_errors: &std::collections::HashMap<String, String>,
    inflight: &[thurbox::kernel::command::InFlight],
) {
    publish_full(host, snapshot, attach_errors, inflight, &Default::default());
}

fn publish_full(
    host: &LuaHost,
    snapshot: &Snapshot,
    attach_errors: &std::collections::HashMap<String, String>,
    inflight: &[thurbox::kernel::command::InFlight],
    printing: &std::collections::HashSet<String>,
) {
    let themes = themes();
    let registry = registry(host);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::new();
    host.publish(&thurbox::kernel::host::Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
        snapshot,
        attach_errors,
        starts: &Default::default(),
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
        hovered: None,
        printing,
    })
    .expect("publish");
}

fn registry(host: &LuaHost) -> Registry {
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    registry
}

fn ui_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ui")
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

#[test]
fn shipped_operator_actions_have_catalog_descriptors_and_handlers() {
    let host = host();
    let mut registry = Registry::default();
    thurbox::kernel::declare_interface(&mut registry, &host);
    let catalog = registry.action_catalog();
    let by_name = |name: &str| catalog.iter().find(|entry| entry.name == name);
    for binding in registry.bindings() {
        let descriptor = by_name(&binding.action)
            .unwrap_or_else(|| panic!("missing key action {}", binding.action));
        assert_eq!(
            descriptor.owner, binding.owner,
            "{} changed owner",
            binding.action
        );
    }
    for command in registry.commands() {
        let descriptor = by_name(&command.action)
            .unwrap_or_else(|| panic!("missing palette action {}", command.action));
        assert_eq!(descriptor.owner, command.owner);
    }
    for pill in registry.pills() {
        assert!(
            by_name(&pill.action).is_some(),
            "missing button action {}",
            pill.action
        );
    }
    for (_, name, _) in thurbox::kernel::registry::RESERVED_ACTIONS {
        assert!(by_name(name).is_some(), "missing reserved action {name}");
    }
    for descriptor in &catalog {
        if descriptor.owner != "kernel" {
            let index = host
                .index_of(&descriptor.owner)
                .expect("action owner loaded");
            assert!(
                host.action_handler_present(index),
                "{} has no on_action handler",
                descriptor.name
            );
        }
    }
    for declaration in host.action_declarations() {
        let descriptor = by_name(&declaration.name)
            .unwrap_or_else(|| panic!("missing declared action {}", declaration.name));
        assert_eq!(descriptor.owner, declaration.owner);
        assert_eq!(descriptor.arguments, declaration.arguments);
    }
}

fn row(name: &str, repo: &str, status: &str) -> SessionRow {
    SessionRow {
        id: format!("{name}-0000-0000-0000-000000000000"),
        name: name.to_string(),
        agent: "claude".to_string(),
        status: SessionState::from_hook_state(status).expect("a hook state"),
        cwd: None,
        repo: Some(repo.to_string()),
        repos: vec![repo.to_string()],
        branch: Some(format!("feat/{name}")),
        base_branch: None,
        backend: "local:tmux".to_string(),
        backend_id: Some("%1".to_string()),
        agent_session_id: None,
        remote_host: None,
        place: None,
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

fn paint(host: &LuaHost, index: usize, width: u16, height: u16) -> Vec<String> {
    let node = host
        .render(index, ctx(width, height, true))
        .unwrap_or_else(|e| panic!("plugin should render: {e}"))
        .node;
    paint_node(&node, width, height)
}

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
    host.mods
        .iter()
        .position(|loaded| loaded.name == name)
        .unwrap_or_else(|| panic!("no plugin named {name}"))
}

#[test]
fn the_bundled_mods_load_and_claim_their_slots() {
    let host = host();
    let names: Vec<&str> = host.mods.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "sessions",
            "agent",
            "confirm",
            "rename",
            "menu",
            "search",
            "new_session"
        ],
        "the bundled set changed"
    );
    assert_eq!(host.in_slot("search").len(), 1);

    assert_eq!(host.in_slot("sessions").len(), 1);
    let centre: Vec<&str> = host
        .in_slot("center")
        .iter()
        .map(|i| host.mods[*i].name.as_str())
        .collect();
    assert_eq!(centre, ["agent"], "{centre:?}");
    for gone in ["themes", "help", "settings"] {
        assert!(!centre.contains(&gone), "{gone} is a modal, not a pane");
    }
    assert_eq!(host.slot_mode("center"), SlotMode::Switch);

    let focusable: Vec<&str> = host
        .focusable()
        .iter()
        .map(|i| host.mods[*i].name.as_str())
        .collect();
    assert!(focusable.contains(&"sessions"), "{focusable:?}");
    assert!(focusable.contains(&"agent"), "{focusable:?}");
    for float in ["new_session", "confirm", "rename", "menu"] {
        assert!(!focusable.contains(&float), "{focusable:?}");
    }
    let floating: Vec<&str> = host
        .floating()
        .iter()
        .map(|i| host.mods[*i].name.as_str())
        .collect();
    assert_eq!(
        floating,
        ["confirm", "rename", "menu", "new_session"],
        "a float must also be excluded from the focus ring, or Tab would land on \
         a pane that draws nothing"
    );
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
    assert!(screen.contains("thurbox"), "{screen}");
    assert!(screen.contains("website"), "{screen}");
    assert!(screen.contains("Sessions"), "{screen}");
    assert!(!screen.contains("sessions · 4"), "{screen}");
}

#[test]
fn a_session_with_no_reported_status_never_draws_the_idle_dot() {
    let host = host();
    let mut driven = row("fm-worker", "thurbox", "idle");
    driven.agent = "zsh".to_string();
    driven.status = SessionState::Running;
    driven.detected_agent = Some("claude".to_string());
    let mut uncovered = row("bare-shell", "thurbox", "idle");
    uncovered.agent = "zsh".to_string();
    uncovered.status = SessionState::Uncovered;
    let mut unreported = row("just-spawned", "thurbox", "idle");
    unreported.status = SessionState::Unreported;

    publish(&host, &snapshot(vec![driven, uncovered, unreported]));
    let screen = paint(&host, index_of(&host, "sessions"), 48, 12).join("\n");

    assert!(!screen.contains('○'), "an idle dot survived:\n{screen}");
    assert!(screen.contains('◉'), "no running glyph in:\n{screen}");
    assert!(screen.contains('◌'), "no silence glyph in:\n{screen}");
    assert!(screen.contains("claude"), "{screen}");
    assert!(screen.contains("no status reported"), "{screen}");
    assert!(!screen.contains("working"), "{screen}");

    let title = paint(&host, index_of(&host, "agent"), 90, 8).join("\n");
    assert!(title.contains("zsh → claude"), "{title}");
    assert!(title.contains("Running"), "{title}");
}

#[test]
fn a_running_session_animates_only_while_its_pane_prints() {
    let host = host();
    let mut driven = row("fm-worker", "thurbox", "idle");
    driven.agent = "zsh".to_string();
    driven.status = SessionState::Running;
    let id = driven.id.clone();
    let rows = snapshot(vec![driven]);

    publish_printing(&host, &rows, &[]);
    let quiet = paint(&host, index_of(&host, "sessions"), 48, 12).join("\n");
    assert!(quiet.contains('◉'), "no static running glyph in:\n{quiet}");
    assert!(
        !quiet.chars().any(is_spinner_frame),
        "a quiet pane animated, which claims a turn nobody observed:\n{quiet}"
    );

    publish_printing(&host, &rows, &[&id]);
    let busy = paint(&host, index_of(&host, "sessions"), 48, 12).join("\n");
    assert!(
        busy.chars().any(is_spinner_frame),
        "a printing pane stayed static:\n{busy}"
    );
    assert!(!busy.contains('◉'), "the static glyph survived:\n{busy}");
}

#[test]
fn output_animates_running_and_nothing_else() {
    let host = host();
    let mut uncovered = row("bare-shell", "thurbox", "idle");
    uncovered.agent = "zsh".to_string();
    uncovered.status = SessionState::Uncovered;
    let mut unreported = row("just-spawned", "thurbox", "idle");
    unreported.status = SessionState::Unreported;
    let mut resting = row("finished", "thurbox", "idle");
    resting.status = SessionState::Idle;
    let ids: Vec<String> = [&uncovered, &unreported, &resting]
        .iter()
        .map(|r| r.id.clone())
        .collect();
    let rows = snapshot(vec![uncovered, unreported, resting]);

    let printing: Vec<&str> = ids.iter().map(String::as_str).collect();
    publish_printing(&host, &rows, &printing);
    let screen = paint(&host, index_of(&host, "sessions"), 48, 12).join("\n");
    assert!(
        !screen.chars().any(is_spinner_frame),
        "output set a status other than `running` in motion:\n{screen}"
    );
    assert!(screen.contains('◌'), "no silence glyph in:\n{screen}");
    assert!(screen.contains('○'), "no idle glyph in:\n{screen}");
}

#[test]
fn a_working_session_animates_without_needing_the_pane() {
    let host = host();
    let working = row("mid-turn", "thurbox", "working");
    let rows = snapshot(vec![working]);

    publish_printing(&host, &rows, &[]);
    let screen = paint(&host, index_of(&host, "sessions"), 48, 12).join("\n");
    assert!(
        screen.chars().any(is_spinner_frame),
        "a working row stopped animating:\n{screen}"
    );
}

fn is_spinner_frame(c: char) -> bool {
    "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(c)
}

#[test]
fn a_running_session_with_no_determined_agent_says_so_without_a_name() {
    let host = host();
    let mut ambiguous = row("fm-worker", "thurbox", "idle");
    ambiguous.agent = "zsh".to_string();
    ambiguous.status = SessionState::Running;
    ambiguous.detected_agent = None;

    publish(&host, &snapshot(vec![ambiguous]));
    let screen = paint(&host, index_of(&host, "sessions"), 64, 12).join("\n");

    assert!(!screen.contains('○'), "an idle dot survived:\n{screen}");
    assert!(screen.contains('◉'), "no running glyph in:\n{screen}");
    assert!(
        screen.contains("agent running · no status reported"),
        "{screen}"
    );
}

#[test]
fn each_status_paints_its_own_glyph() {
    let host = host();
    publish(&host, &sample());
    let screen = paint(&host, index_of(&host, "sessions"), 40, 12).join("\n");

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
fn a_mod_is_told_its_own_rect_not_the_screens() {
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
    let host = host();
    let rows: Vec<SessionRow> = (0..40)
        .map(|n| row(&format!("session-{n:02}"), "thurbox", "idle"))
        .collect();
    publish(&host, &snapshot(rows));

    let screen = paint(&host, index_of(&host, "sessions"), 40, 10).join("\n");
    assert!(
        screen.contains('▼'),
        "a windowed list must mark what it hid:\n{screen}"
    );
}

#[test]
fn the_agent_pane_places_a_surface_it_does_not_paint() {
    let host = host();
    publish(&host, &sample());

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
    assert!(screen.contains("No active sessions"), "{screen}");
    assert!(
        !screen.contains("Ctrl+N"),
        "the empty state hard-codes the new-session chord:\n{screen}"
    );
}

#[test]
fn the_arrangement_drops_the_side_column_when_narrow() {
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
fn mod_reads_are_served_from_the_snapshot() {
    let host = host();
    publish(&host, &sample());
    let index = index_of(&host, "sessions");
    for _ in 0..1000 {
        host.render(index, ctx(40, 12, false)).expect("render");
    }
}

#[test]
fn the_kernel_still_exposes_exactly_four_node_kinds() {
    assert_eq!(
        KINDS,
        ["text", "box", "input", "surface"],
        "the node catalog grew — that is the failure mode v2 exists to avoid"
    );
}

#[test]
fn the_panes_are_built_from_primitives_not_kernel_widgets() {
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
fn a_broken_mod_costs_only_its_own_pane() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    std::fs::write(
        mods.join("10_good.lua"),
        r#"return { name = "good", slot = "a", render = function() return { text = "fine" } end }"#,
    )
    .expect("write good");
    std::fs::write(
        mods.join("20_bad.lua"),
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
    assert_eq!(bad.owner, "bad", "the failure must name its plugin");
    assert!(bad.message.contains("boom"), "{}", bad.message);
}

#[test]
fn a_failed_reload_keeps_the_last_good_mods_running() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    let file = mods.join("10_pane.lua");
    std::fs::write(
        &file,
        r#"return { name = "pane", slot = "a", render = function() return { text = "v1" } end }"#,
    )
    .expect("write");

    let mut host = LuaHost::new(dir.path());
    let reloads = host.reloads;

    std::fs::write(&file, "return { this is not lua").expect("write broken");
    host.reload();

    assert!(host.error.is_some(), "the failure must be surfaced");
    assert_eq!(host.reloads, reloads, "a failed reload must not count");
    let screen = paint(&host, index_of(&host, "pane"), 10, 1).join("\n");
    assert!(screen.contains("v1"), "the last good plugin keeps running");

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
fn an_unterminated_mod_is_interrupted_rather_than_hanging() {
    // Lua 5.4 has no set_interrupt, so only the kernel's count hook stops this loop.
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    std::fs::write(
        mods.join("10_spin.lua"),
        r#"return { name = "spin", slot = "a", render = function() while true do end end }"#,
    )
    .expect("write");

    let host = LuaHost::new(dir.path());
    let error = host
        .render(index_of(&host, "spin"), ctx(20, 3, false))
        .expect_err("an unterminated render must be interrupted");
    assert_eq!(error.owner, "spin");
    assert!(
        error.message.contains("instruction budget"),
        "{}",
        error.message
    );
}

#[test]
fn mods_have_no_filesystem_or_process_access() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    std::fs::write(
        mods.join("10_probe.lua"),
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
fn mod_state_is_private_and_survives_a_reload() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    for (file, name, value) in [("10_a.lua", "a", 1), ("20_b.lua", "b", 2)] {
        std::fs::write(
            mods.join(file),
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

    host.reload();
    assert!(host.error.is_none(), "{:?}", host.error);
    assert!(paint(&host, index_of(&host, "a"), 10, 1)[0].contains("n=1"));
    assert!(paint(&host, index_of(&host, "b"), 10, 1)[0].contains("n=2"));
}

#[test]
fn a_slot_divides_its_rect_among_its_occupants() {
    let host = host();
    let members = host.in_slot("sessions");
    let sizes: Vec<_> = members.iter().map(|i| host.mods[*i].size).collect();
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
    let host = host();
    let agent = &host.mods[index_of(&host, "agent")];
    assert!(agent.session_input, "the agent pane must ask for raw input");

    let sessions = &host.mods[index_of(&host, "sessions")];
    assert!(
        !sessions.session_input,
        "the session list must not swallow keys into a pty"
    );
}

#[test]
fn the_focused_session_is_read_off_the_rendered_tree() {
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
    let host = host();
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
    let host = host();
    publish(&host, &sample());
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render list");

    let screen = paint(&host, index_of(&host, "agent"), 60, 10).join("\n");
    assert!(screen.contains("terminal surface"), "{screen}");
    assert!(screen.contains("not attached"), "{screen}");
}

use thurbox::kernel::command::{Command, InFlight, Phase};
use thurbox::session::SessionState;

fn press_key(host: &LuaHost, mod_name: &str, ch: char) {
    press_chord(host, mod_name, ch, false);
}

fn press_ctrl_key(host: &LuaHost, mod_name: &str, ch: char) {
    press_chord(host, mod_name, ch, true);
}

fn press_chord(host: &LuaHost, mod_name: &str, ch: char, ctrl: bool) {
    let registry = registry(host);
    let key = KeyPress {
        name: ch.to_lowercase().to_string(),
        ch: Some(ch),
        ctrl,
        ..KeyPress::default()
    };
    if let Some(binding) = registry.resolve(&key, Some(mod_name)) {
        if let Some(index) = host.index_of(&binding.owner) {
            if host.on_action(index, &binding.action).expect("action") {
                return;
            }
        }
    }
    host.on_key(index_of(host, mod_name), &key).expect("key");
}

fn keys_to_commands(host: &LuaHost, mod_name: &str, ch: char) -> Vec<Command> {
    press_key(host, mod_name, ch);
    host.drain_commands()
}

fn ctrl_keys_to_commands(host: &LuaHost, mod_name: &str, ch: char) -> Vec<Command> {
    press_ctrl_key(host, mod_name, ch);
    host.drain_commands()
}

#[test]
fn deleting_from_the_session_list_asks_before_issuing_a_command() {
    let host = host();
    publish(&host, &sample());
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render");

    let issued = ctrl_keys_to_commands(&host, "sessions", 'd');
    assert!(issued.is_empty());
    host.render(index_of(&host, "confirm"), ctx(40, 12, false))
        .expect("draw the question");
    press_key(&host, "confirm", 'y');
    let issued = host.drain_commands();
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
    let host = host();
    publish(&host, &sample());
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render");

    let issued = keys_to_commands(&host, "sessions", 'J');
    assert_eq!(
        issued.len(),
        1,
        "a legacy terminal's bare \"J\" (no SHIFT) must reorder: got {issued:?}"
    );
    assert_eq!(issued[0].kind(), "order");

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
fn a_group_cannot_be_moved_onto_another_machine() {
    let host = host();
    let mut remote = row("remote-build", "thurbox", "idle");
    remote.backend = "ssh:buildbox".to_string();
    remote.remote_host = Some("buildbox".to_string());
    publish(
        &host,
        &snapshot(vec![
            row("fix-osc52", "thurbox", "working"),
            row("update-deps", "website", "idle"),
            remote,
        ]),
    );
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render");

    press_key(&host, "sessions", 'j');
    press_key(&host, "sessions", 'j');
    host.drain_commands();

    let issued = keys_to_commands(&host, "sessions", 'J');
    assert!(
        issued.is_empty(),
        "a group may not be moved onto another machine, got {issued:?}"
    );

    let issued = keys_to_commands(&host, "sessions", 'K');
    assert_eq!(issued.len(), 1, "expected a reorder, got {issued:?}");
    assert_eq!(issued[0].kind(), "order");
}

#[test]
fn the_top_row_cannot_move_up() {
    let host = host();
    publish(&host, &sample());
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render");

    assert!(keys_to_commands(&host, "sessions", 'K').is_empty());
}

#[test]
fn the_arrows_navigate_the_session_list_like_j_and_k() {
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
fn a_malformed_command_is_reported_against_the_mod() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    std::fs::write(
        mods.join("10_bad.lua"),
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
    let host = host();
    let target = sample().sessions[0].id.clone();
    let inflight = vec![InFlight {
        id: 1,
        kind: "rename",
        session: target,
        subject: None,
        host: None,
        phase: Phase::Running,
        error: None,
    }];

    publish_with(&host, &sample(), &Default::default(), &inflight);
    let screen = paint(&host, index_of(&host, "sessions"), 46, 12).join("\n");
    assert!(screen.contains('◌'), "pending marker missing:\n{screen}");
    assert!(screen.contains("rename"), "{screen}");
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
        host: None,
        phase: Phase::Failed,
        error: Some("session not found".to_string()),
    }];

    publish_with(&host, &sample(), &Default::default(), &inflight);
    let screen = paint(&host, index_of(&host, "sessions"), 46, 12).join("\n");
    assert!(screen.contains('✗'), "{screen}");
    assert!(screen.contains("failed"), "{screen}");
    assert!(screen.contains("fix-osc52"), "{screen}");
}

#[test]
fn the_manual_order_is_published_so_a_reorder_is_visible() {
    let host = host();
    let mut snap = sample();
    for (position, row) in snap.sessions.iter_mut().enumerate() {
        row.display_order = Some(100 - position as i64);
    }
    let expected_first = snap.sessions.last().expect("rows").name.clone();

    publish(&host, &snap);
    let screen = paint(&host, index_of(&host, "sessions"), 46, 12).join("\n");

    let names: Vec<&str> = snap.sessions.iter().map(|row| row.name.as_str()).collect();
    let first_session_line = screen
        .lines()
        .find(|line| names.iter().any(|name| line.contains(name)))
        .unwrap_or_default()
        .to_string();
    assert!(
        first_session_line.contains(&expected_first),
        "manual order ignored: expected {expected_first} first, got {first_session_line:?}"
    );
}

#[test]
fn the_theme_reaches_mods_as_roles() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    std::fs::write(
        mods.join("10_probe.lua"),
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
        .any(|w| w[0] == '"' && w[1] == '#' && w[2..8].iter().all(char::is_ascii_hexdigit))
}

#[test]
fn no_bundled_mod_hardcodes_a_colour() {
    let ui = ui_dir();
    let mut offenders = Vec::new();
    for dir in [ui.join("mods"), ui.join("lib")] {
        for entry in std::fs::read_dir(&dir).expect("read ui dir") {
            let path = entry.expect("entry").path();
            if !path.extension().is_some_and(|e| e == "lua") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("read");
            if path.file_name().is_some_and(|n| n == "theme.lua") {
                continue;
            }
            offenders.extend(colour_offenders(&path, &source));
        }
    }
    assert!(
        offenders.is_empty(),
        "bundled plugins must name roles, not colours:\n{}",
        offenders.join("\n")
    );
}

fn colour_offenders(path: &std::path::Path, source: &str) -> Vec<String> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("--") && hardcodes_colour(line))
        .map(|(n, line)| format!("{}:{}: {}", path.display(), n + 1, line.trim()))
        .collect()
}

#[test]
fn every_bundled_mod_declares_its_keys_as_data() {
    let host = host();
    let registry = registry(&host);
    assert!(!registry.bindings().is_empty());

    for mod_name in ["sessions", "agent"] {
        assert!(
            registry.bindings().iter().any(|b| b.owner == mod_name),
            "{mod_name} declares no keys"
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

#[test]
fn the_lua_vm_stays_non_send() {
    let manifest =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("read Cargo.toml");
    let mlua_line = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("mlua = "))
        .expect("mlua dependency");
    assert!(
        !mlua_line.contains("\"send\""),
        "mlua's send feature must stay off, so a mod reaching the render thread fails to \
         compile: {mlua_line}"
    );
}

#[test]
fn the_granted_capability_set_matches_a_declared_list() {
    const GRANTED: [&str; 7] = [
        "require",
        "state",
        "store",
        "store_writer",
        "command",
        "thurbox",
        // Lua's `#` counts bytes and `utf8.len` codepoints; neither is display width.
        "text",
    ];

    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    std::fs::write(
        mods.join("10_probe.lua"),
        r#"
        return {
          name = "probe", slot = "a",
          render = function() return { text = "" } end,
        }
        "#,
    )
    .expect("write");

    let host = LuaHost::new(dir.path());
    publish(&host, &snapshot(Vec::new()));
    let globals = host.reachable_globals(index_of(&host, "probe")).join(" ");

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
        let known = GRANTED.contains(&name)
            || STDLIB.contains(&name)
            || [
                "assert",
                "error",
                "next",
                "pcall",
                "xpcall",
                "rawequal",
                "rawget",
                "rawlen",
                "rawset",
                "select",
                "setmetatable",
                "getmetatable",
                "unpack",
                "require",
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
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    std::fs::write(
        mods.join("10_fields.lua"),
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
    // Lua's `pairs` skips nil fields, so every field under test needs a value.
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

#[test]
fn a_create_command_carries_what_the_flow_chose() {
    use thurbox::kernel::command::{Args, Command};
    let parsed = Command::parse(
        "create",
        Args {
            repo: Some("/tmp/demo".into()),
            agent: Some("claude".into()),
            ..Args::default()
        },
    )
    .expect("parse");
    match parsed {
        Command::Create { repo, agent, .. } => {
            assert_eq!(repo, "/tmp/demo");
            assert_eq!(agent.as_deref(), Some("claude"));
        }
        other => panic!("expected create, got {other:?}"),
    }
}

#[test]
fn a_bookmark_command_needs_a_path_and_an_explicit_verb() {
    use thurbox::kernel::command::{Args, BookmarkEdit, Command};
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
        ("create", BookmarkEdit::Create),
        ("init", BookmarkEdit::Init),
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
                path: "~/src/thing".into(),
                edit: expected,
            },
            "{verb}"
        );
    }
}

#[test]
fn a_bookmark_write_is_published_with_the_path_it_writes() {
    use thurbox::kernel::command::{Args, Command};
    let command = Command::parse(
        "bookmark",
        Args {
            repo: Some(" ~/src/new ".into()),
            action: Some("init".into()),
            ..Args::default()
        },
    )
    .expect("parse");
    assert_eq!(command.subject().as_deref(), Some("~/src/new"));
}

#[test]
fn a_bookmark_clone_needs_a_url_that_cannot_pass_for_an_option() {
    use thurbox::kernel::command::{Args, BookmarkEdit, Command};
    let clone = |url: Option<&str>| {
        Command::parse(
            "bookmark",
            Args {
                repo: Some("~/src/new".into()),
                action: Some("clone".into()),
                text: url.map(str::to_string),
                ..Args::default()
            },
        )
    };
    assert_eq!(
        clone(Some("  git@github.com:me/new.git  ")).expect("parse"),
        Command::Bookmark {
            host: String::new(),
            path: "~/src/new".into(),
            edit: BookmarkEdit::Clone {
                url: "git@github.com:me/new.git".into()
            },
        }
    );
    for missing in [None, Some("   ")] {
        let error = clone(missing).unwrap_err();
        assert!(error.contains("url"), "{error}");
    }
    // `git clone -- <url>` already stops option parsing; refusing here says why.
    let error = clone(Some("--upload-pack=touch /tmp/x")).unwrap_err();
    assert!(error.contains("url"), "{error}");
}

#[test]
fn creating_without_a_repo_is_refused() {
    use thurbox::kernel::command::{Args, Command};
    let error = Command::parse("create", Args::default()).unwrap_err();
    assert!(error.contains("needs a repo"), "{error}");
}

#[test]
fn a_pending_creation_draws_in_the_repo_it_will_land_in() {
    let host = host();
    let inflight = vec![InFlight {
        id: 1,
        kind: "create",
        session: String::new(),
        subject: Some("thurbox".to_string()),
        host: None,
        phase: Phase::Running,
        error: None,
    }];
    publish_with(&host, &sample(), &Default::default(), &inflight);

    let screen = paint(&host, index_of(&host, "sessions"), 46, 14).join("\n");
    assert!(screen.contains("creating"), "{screen}");

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
        "placeholder should follow its repo row:\n{screen}"
    );
}

#[test]
fn a_pending_creation_draws_on_the_machine_it_was_asked_for() {
    let host = host();
    let mut remote = row("remote-docs", "website", "idle");
    remote.backend = "ssh:buildbox".to_string();
    remote.remote_host = Some("buildbox".to_string());
    let world = snapshot(vec![row("fix-osc52", "thurbox", "working"), remote]);

    let create = |subject: &str, on: Option<&str>| {
        let command = Command::parse(
            "create",
            thurbox::kernel::command::Args {
                repo: Some(format!("/src/{subject}")),
                host: on.map(str::to_string),
                ..Default::default()
            },
        )
        .expect("parse a create");
        vec![InFlight {
            id: 1,
            kind: "create",
            session: String::new(),
            subject: command.subject(),
            host: command.host(),
            phase: Phase::Running,
            error: None,
        }]
    };
    let screen_for = |inflight: &[InFlight]| {
        publish_with(&host, &world, &Default::default(), inflight);
        paint(&host, index_of(&host, "sessions"), 46, 14).join("\n")
    };
    let line_of = |screen: &str, needle: &str| {
        screen
            .lines()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no {needle:?} in:\n{screen}"))
    };

    let screen = screen_for(&create("website", Some("ssh:buildbox")));
    assert!(
        !screen.contains("ssh:buildbox"),
        "a backend name is not a machine the list can group by:\n{screen}"
    );
    assert_eq!(
        screen.matches("buildbox").count(),
        1,
        "the creation joins the existing host row:\n{screen}"
    );
    assert!(
        line_of(&screen, "creating") > line_of(&screen, "buildbox"),
        "and draws inside it:\n{screen}"
    );

    let screen = screen_for(&create("website", None));
    assert!(
        line_of(&screen, "creating") > line_of(&screen, "▾ website"),
        "a creation on this machine draws under this machine:\n{screen}"
    );
    assert!(
        line_of(&screen, "creating") < line_of(&screen, "buildbox"),
        "and not under the host that happens to hold that repo:\n{screen}"
    );
}

#[test]
fn creating_the_first_session_on_a_host_names_the_machine_straight_away() {
    let host = host();
    let command = Command::parse(
        "create",
        thurbox::kernel::command::Args {
            repo: Some("/src/thurbox".into()),
            host: Some("ssh:buildbox".into()),
            ..Default::default()
        },
    )
    .expect("parse a create");
    let inflight = vec![InFlight {
        id: 1,
        kind: "create",
        session: String::new(),
        subject: command.subject(),
        host: command.host(),
        phase: Phase::Running,
        error: None,
    }];
    publish_with(
        &host,
        &snapshot(vec![row("fix-osc52", "thurbox", "working")]),
        &Default::default(),
        &inflight,
    );

    let screen = paint(&host, index_of(&host, "sessions"), 46, 14).join("\n");
    assert!(
        screen.contains("fix-osc52"),
        "the local row remains:\n{screen}"
    );
    let creating = screen
        .lines()
        .position(|l| l.contains("creating"))
        .unwrap_or_else(|| panic!("no placeholder in:\n{screen}"));
    let header = screen
        .lines()
        .position(|l| l.contains("buildbox"))
        .unwrap_or_else(|| panic!("no host row for the machine asked for in:\n{screen}"));
    assert!(
        creating > header,
        "the placeholder names the machine it is being created on:\n{screen}"
    );
}

#[test]
fn a_session_being_deleted_leaves_the_list_at_once() {
    let host = host();
    let sessions = index_of(&host, "sessions");
    let snapshot = sample();
    let doomed = snapshot.sessions[0].id.clone();

    publish(&host, &snapshot);
    let before = paint(&host, sessions, 46, 14).join("\n");
    assert!(before.contains("fix-osc52"), "{before}");

    let inflight = vec![InFlight {
        id: 1,
        kind: "delete",
        session: doomed,
        subject: None,
        host: None,
        phase: Phase::Running,
        error: None,
    }];
    publish_with(&host, &snapshot, &Default::default(), &inflight);

    let screen = paint(&host, sessions, 46, 14).join("\n");
    assert!(
        !screen.contains("fix-osc52"),
        "the deleted row should be gone, not tagged:\n{screen}"
    );
    assert!(
        !screen.contains("delete"),
        "and nothing should be left saying so:\n{screen}"
    );
    assert!(screen.contains("add-wsl-tests"), "{screen}");
}

#[test]
fn a_creation_into_a_fresh_repo_brings_its_own_header() {
    let host = host();
    let inflight = vec![InFlight {
        id: 1,
        kind: "create",
        session: String::new(),
        subject: Some("brand-new".to_string()),
        host: None,
        phase: Phase::Queued,
        error: None,
    }];
    publish_with(&host, &sample(), &Default::default(), &inflight);

    let screen = paint(&host, index_of(&host, "sessions"), 46, 16).join("\n");
    assert!(screen.contains("brand-new"), "{screen}");
    assert!(screen.contains("creating"), "{screen}");
}

#[test]
fn a_failing_decorator_leaves_the_pane_drawn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    std::fs::write(
        mods.join("10_pane.lua"),
        r#"return { name = "pane", slot = "a",
                   render = function() return { text = "content" } end }"#,
    )
    .expect("write");
    std::fs::write(
        mods.join("20_bad.lua"),
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
    assert_eq!(error.owner, "bad");
    assert!(paint_node(&node, 20, 3).join("").contains("content"));
}

#[test]
fn the_review_body_is_a_surface_not_a_tree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    std::fs::write(
        mods.join("10_body.lua"),
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

fn painted_caret(node: &Node, width: u16, height: u16) -> Option<(u16, u16)> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| render(frame, frame.area(), node, &PlaceholderSurfaces))
        .expect("draw");
    let backend = terminal.backend();
    let position = backend.cursor_position();
    backend.cursor_visible().then_some((position.x, position.y))
}

fn field(value: &str, cursor: usize, focused: bool) -> Node {
    Node::Input {
        identity: Default::default(),
        size: Default::default(),
        frame: None,
        value: value.into(),
        cursor,
        placeholder: "name".into(),
        focused,
        style: Default::default(),
    }
}

#[test]
fn an_empty_focused_field_still_owns_the_caret() {
    assert_eq!(painted_caret(&field("", 0, true), 20, 1), Some((0, 0)));
    assert_eq!(painted_caret(&field("ab", 2, true), 20, 1), Some((2, 0)));
}

#[test]
fn only_the_focused_field_owns_the_caret() {
    let two = Node::Box {
        identity: Default::default(),
        size: Default::default(),
        frame: None,
        axis: thurbox::kernel::node::Axis::Vertical,
        gap: 0,
        children: vec![field("typed here", 4, true), field("stale", 5, false)],
    };
    assert_eq!(painted_caret(&two, 20, 2), Some((4, 0)));

    let none = Node::Box {
        identity: Default::default(),
        size: Default::default(),
        frame: None,
        axis: thurbox::kernel::node::Axis::Vertical,
        gap: 0,
        children: vec![field("typed", 3, false)],
    };
    assert_eq!(
        painted_caret(&none, 20, 2),
        None,
        "a field nobody is typing into must not take the caret"
    );
}

#[test]
fn link_detection_has_one_definition() {
    let rows = vec!["see https://example.com/x for more".to_string()];
    let found = thurbox::kernel::terminal::links::detect_urls(&rows);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].url, "https://example.com/x");
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
    let host = host();
    publish(&host, &sample());
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render");
    let index = index_of(&host, "agent");

    host.on_action(index, "shell.open").expect("open");
    let issued = host.drain_commands();
    let kinds: Vec<&str> = issued.iter().map(|command| command.kind()).collect();
    assert!(kinds.contains(&"shell"), "{kinds:?}");

    let node = host.render(index, ctx(50, 8, true)).expect("render").node;
    let session = node
        .first_session_surface()
        .expect("the shell pane places a surface");
    assert!(session.ends_with("#shell"), "{session}");
    for kind in collect_kinds(&node) {
        assert!(KINDS.contains(&kind), "{kind} is not a primitive");
    }
}

#[test]
fn children_nest_under_their_parent_within_a_repo() {
    use unicode_width::UnicodeWidthStr;

    let host = host();
    let mut snap = sample();
    let parent = snap.sessions[0].id.clone();
    snap.sessions[1].parent_id = Some(parent);
    publish(&host, &snap);

    let lines = paint(&host, index_of(&host, "sessions"), 50, 14);
    let parent = lines
        .iter()
        .position(|line| line.contains("fix-osc52"))
        .unwrap();
    let child = lines
        .iter()
        .position(|line| line.contains("add-wsl-tests"))
        .unwrap();
    assert_eq!(child, parent + 1, "{lines:#?}");
    let parent_indent = lines[parent][..lines[parent].find("fix-osc52").unwrap()].width();
    let child_indent = lines[child][..lines[child].find("add-wsl-tests").unwrap()].width();
    assert_eq!(child_indent, parent_indent + 2, "{lines:#?}");
    assert!(!lines[child].contains('└'));
}

#[test]
fn a_child_whose_parent_is_elsewhere_keeps_its_place() {
    let host = host();
    let mut snap = sample();
    snap.sessions[3].parent_id = Some(snap.sessions[0].id.clone());
    publish(&host, &snap);

    let screen = paint(&host, index_of(&host, "sessions"), 50, 14).join("\n");
    assert!(
        screen.contains('↳'),
        "an out-of-group child should be marked rather than moved:\n{screen}"
    );
}

#[test]
fn a_stage_name_reaches_the_pending_row() {
    let host = host();
    let inflight = vec![InFlight {
        id: 1,
        kind: "create",
        session: String::new(),
        subject: Some("thurbox".to_string()),
        host: None,
        phase: thurbox::kernel::command::Phase::Stage("worktrees".to_string()),
        error: None,
    }];
    publish_with(&host, &sample(), &Default::default(), &inflight);

    let screen = paint(&host, index_of(&host, "sessions"), 46, 14).join("\n");
    assert!(screen.contains("creating"), "{screen}");
}

#[test]
fn selection_logic_has_one_implementation() {
    use thurbox::kernel::selection::{PaneBounds, Selection, TermPos};

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
}

#[test]
fn a_selection_is_clamped_to_its_pane() {
    use thurbox::kernel::selection::PaneBounds;
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
fn a_mod_earns_a_band_entry_by_declaring_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    std::fs::write(
        mods.join("10_contributor.lua"),
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
    assert_eq!(pills[0].owner, "contributor", "stamped with its author");

    let mut registry = Registry::default();
    registry.declare_all(bindings, settings, pills);
    let entries = thurbox::kernel::bands::entries(registry.pills(), &registry);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].display(), "Mine · F7");
}

#[test]
fn a_mod_that_becomes_unloadable_leaves_the_last_good_set_in_force() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    let good = mods.join("10_good.lua");
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
    let host = host();
    let focusable: Vec<&str> = host
        .focusable()
        .iter()
        .map(|index| host.mods[*index].name.as_str())
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
    use thurbox::kernel::selection::PaneBounds;
    let pane = Rect {
        x: 10,
        y: 4,
        width: 20,
        height: 6,
    };

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

    assert!(PaneBounds::content_at(pane, 0, 0).is_none());

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

    let roles = themes.roles();
    let expect = |role: &str| {
        thurbox::kernel::node::parse_color(roles.get(role).expect("role")).expect("a colour")
    };
    assert_eq!(style.bg, Some(expect("selection_bg")));
    assert_eq!(style.fg, Some(expect("selection_fg")));
}

#[test]
fn copy_and_paste_are_reachable_from_a_mod() {
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
