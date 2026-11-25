use std::collections::HashMap;

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;
use ratatui::Terminal;

use thurbox::kernel::host::{KeyPress, LuaHost, Published, RenderContext};
use thurbox::kernel::paint::{render, PlaceholderSurfaces, ProgramPaint, SurfaceProvider};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::terminal::AgentMeta;
use thurbox::kernel::theme::Themes;
use thurbox::session::SessionState;

fn host() -> LuaHost {
    let host = LuaHost::new(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ui"));
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

fn themes() -> Themes {
    let mut themes = Themes::load(None);
    themes
        .preview("default")
        .expect("the default preset exists");
    themes
}

fn registry(host: &LuaHost) -> Registry {
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    registry
}

fn publish(host: &LuaHost, snapshot: &Snapshot) {
    publish_with(host, snapshot, &HashMap::new());
}

fn publish_hovered(
    host: &LuaHost,
    snapshot: &Snapshot,
    hovered: Option<&thurbox::kernel::node::Identity>,
) {
    publish_inner(host, snapshot, &HashMap::new(), &HashMap::new(), hovered);
}

fn publish_with(host: &LuaHost, snapshot: &Snapshot, attach_errors: &HashMap<String, String>) {
    publish_inner(host, snapshot, attach_errors, &HashMap::new(), None);
}

fn publish_meta(
    host: &LuaHost,
    snapshot: &Snapshot,
    attach_errors: &HashMap<String, String>,
    meta: &HashMap<String, AgentMeta>,
) {
    publish_inner(host, snapshot, attach_errors, meta, None);
}

fn publish_inner(
    host: &LuaHost,
    snapshot: &Snapshot,
    attach_errors: &HashMap<String, String>,
    meta: &HashMap<String, AgentMeta>,
    hovered: Option<&thurbox::kernel::node::Identity>,
) {
    let themes = themes();
    let registry = registry(host);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::new();
    host.publish(&Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
        snapshot,
        attach_errors,
        starts: &Default::default(),
        inflight: &[],
        themes: &themes,
        registry: &registry,
        diffs: &diffs,
        links: &Default::default(),
        search: None,
        meta,
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

fn row(name: &str, repo: &str, status: &str) -> SessionRow {
    SessionRow {
        id: format!("{name}-0000-0000-0000-000000000000"),
        name: name.to_string(),
        agent: "claude".to_string(),
        status: SessionState::from_hook_state(status).expect("a hook state"),
        cwd: Some(std::path::PathBuf::from(format!("/src/{repo}"))),
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

fn remote_row(name: &str, repo: &str, status: &str, host: &str) -> SessionRow {
    let mut row = row(name, repo, status);
    row.backend = format!("ssh:{host}");
    row.remote_host = Some(host.to_string());
    row
}

fn snapshot(rows: Vec<SessionRow>) -> Snapshot {
    Snapshot {
        sessions: rows,
        taken_at_ms: 1_700_000_000_000,
        ..Snapshot::default()
    }
}

fn sample() -> Snapshot {
    let mut rows = vec![
        row("fix-osc52", "thurbox", "working"),
        row("add-wsl-tests", "thurbox", "blocked"),
        row("perf-cache", "thurbox", "done"),
        row("update-deps", "website", "idle"),
    ];
    let mut child = row("fix-osc52-tests", "thurbox", "idle");
    child.parent_id = Some(rows[0].id.clone());
    rows.push(child);
    snapshot(rows)
}

const PINNED_SPINNER_ELAPSED: f64 = 1.0;

fn ctx(width: u16, height: u16, focused: bool) -> RenderContext {
    RenderContext {
        width,
        height,
        focused,
        elapsed: PINNED_SPINNER_ELAPSED,
        frame: 1,
    }
}

fn index_of(host: &LuaHost, name: &str) -> usize {
    host.index_of(name)
        .unwrap_or_else(|| panic!("no plugin named {name}"))
}

fn press(host: &LuaHost, mod_name: &str, ch: char) {
    let key = KeyPress {
        name: ch.to_lowercase().to_string(),
        ch: Some(ch),
        ..KeyPress::default()
    };
    if let Some(binding) = registry(host).resolve(&key, Some(mod_name)) {
        if let Some(index) = host.index_of(&binding.owner) {
            if host.on_action(index, &binding.action).expect("action") {
                return;
            }
        }
    }
    host.on_key(index_of(host, mod_name), &key).expect("key");
}

fn paint(host: &LuaHost, name: &str, width: u16, height: u16, focused: bool) -> Buffer {
    paint_over(host, name, width, height, focused, &PlaceholderSurfaces)
}

fn paint_over(
    host: &LuaHost,
    name: &str,
    width: u16,
    height: u16,
    focused: bool,
    surfaces: &dyn SurfaceProvider,
) -> Buffer {
    let node = host
        .render(index_of(host, name), ctx(width, height, focused))
        .unwrap_or_else(|e| panic!("{name} should render: {e}"))
        .node;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| render(frame, frame.area(), &node, surfaces))
        .expect("draw");
    terminal.backend().buffer().clone()
}

fn text(buffer: &Buffer) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    let area = buffer.area;
    (0..area.height)
        .map(|y| {
            let mut line = String::new();
            let mut cells_held_by_wide_glyph = 0;
            for x in 0..area.width {
                if cells_held_by_wide_glyph > 0 {
                    cells_held_by_wide_glyph -= 1;
                    continue;
                }
                let symbol = buffer[(x, y)].symbol();
                // A terminal shows a double-width glyph over the blank cell after it.
                cells_held_by_wide_glyph = symbol.width().saturating_sub(1);
                line.push_str(symbol);
            }
            line.trim_end().to_string()
        })
        .collect()
}

fn style_runs(buffer: &Buffer, y: u16) -> String {
    let mut line = String::new();
    let mut last: Option<String> = None;
    for x in 0..buffer.area.width {
        let cell = &buffer[(x, y)];
        let style = format!("{:?}/{:?}/{:?}", cell.fg, cell.bg, cell.modifier);
        if last.as_deref() != Some(style.as_str()) {
            line.push_str(&format!("⟨{style}⟩"));
            last = Some(style);
        }
        line.push_str(cell.symbol());
    }
    line.trim_end().to_string()
}

#[track_caller]
fn assert_frame(actual: &[String], expected: &[&str]) {
    if actual
        .iter()
        .map(String::as_str)
        .eq(expected.iter().copied())
    {
        return;
    }
    let literal = actual
        .iter()
        .map(|line| format!("    {:?},", line))
        .collect::<Vec<_>>()
        .join("\n");
    panic!("frame differs from the expected literal.\nactual, as a literal:\n[\n{literal}\n]");
}

#[test]
fn the_session_list_groups_by_repo_and_nests_a_child_under_its_parent() {
    let host = host();
    publish(&host, &sample());
    assert_frame(
        &text(&paint(&host, "sessions", 40, 12, true)),
        &[
            "┏ ▸ Sessions ━━━━━━━━━━━━━━━━━━━━━━━━━━┓",
            "┃ ▾ ⌂ local  !2 ─ 5 sessions · 1 active┃",
            "┃  ▾ thurbox ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┃",
            "┃   ▾ ⠇ fix-osc52                      ┃",
            "┃       ○ fix-osc52-tests              ┃",
            "┃     ◆ add-wsl-tests  Blocked         ┃",
            "┃     ● perf-cache                     ┃",
            "┃  ▾ website ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┃",
            "┃     ○ update-deps                    ┃",
            "┃                                      ┃",
            "┃                                      ┃",
            "┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛",
        ],
    );
}

#[test]
fn the_session_list_groups_by_host_when_sessions_span_machines() {
    let host = host();
    publish(
        &host,
        &snapshot(vec![
            row("fix-osc52", "thurbox", "working"),
            row("perf-cache", "thurbox", "done"),
            row("update-deps", "website", "idle"),
            remote_row("remote-build", "thurbox", "idle", "buildbox"),
        ]),
    );
    assert_frame(
        &text(&paint(&host, "sessions", 40, 12, true)),
        &[
            "┏ ▸ Sessions ━━━━━━━━━━━━━━━━━━━━━━━━━━┓",
            "┃ ▾ ⌂ local  !1 ─ 3 sessions · 1 active┃",
            "┃  ▾ thurbox ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┃",
            "┃     ⠇ fix-osc52                      ┃",
            "┃     ● perf-cache                     ┃",
            "┃  ▾ website ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┃",
            "┃     ○ update-deps                    ┃",
            "┃ ▾ @ buildbox ────────────── 1 session┃",
            "┃  ▾ thurbox ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┃",
            "┃     ○ remote-build                   ┃",
            "┃                                      ┃",
            "┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛",
        ],
    );
}

#[test]
fn one_remote_host_has_a_fold_handle_above_its_repo_groups() {
    let host = host();
    publish(
        &host,
        &snapshot(vec![
            remote_row("fix-osc52", "thurbox", "working", "buildbox"),
            remote_row("perf-cache", "thurbox", "done", "buildbox"),
            remote_row("update-deps", "website", "idle", "buildbox"),
        ]),
    );
    assert_frame(
        &text(&paint(&host, "sessions", 40, 10, true)),
        &[
            "┏ ▸ Sessions ━━━━━━━━━━━━━━━━━━━━━━━━━━┓",
            "┃ ▾ @ buildbox  !1 ───────── 3 sessions┃",
            "┃  ▾ thurbox ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┃",
            "┃     ⠇ fix-osc52                      ┃",
            "┃     ● perf-cache                     ┃",
            "┃  ▾ website ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┃",
            "┃     ○ update-deps                    ┃",
            "┃                                      ┃",
            "┃                                      ┃",
            "┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛",
        ],
    );
    press(&host, "sessions", 'h');
    assert_frame(
        &text(&paint(&host, "sessions", 40, 10, true)),
        &[
            "┏ ▸ Sessions ━━━━━━━━━━━━━━━━━━━━━━━━━━┓",
            "┃ ▸ @ buildbox  !1 ───────── 3 sessions┃",
            "┃                                      ┃",
            "┃                                      ┃",
            "┃                                      ┃",
            "┃                                      ┃",
            "┃                                      ┃",
            "┃                                      ┃",
            "┃                                      ┃",
            "┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛",
        ],
    );
}

#[test]
fn a_host_named_local_is_a_second_machine_rather_than_this_one() {
    let host = host();
    publish(
        &host,
        &snapshot(vec![
            row("fix-osc52", "thurbox", "working"),
            remote_row("remote-build", "thurbox", "idle", "local"),
        ]),
    );
    assert_frame(
        &text(&paint(&host, "sessions", 40, 8, true)),
        &[
            "┏ ▸ Sessions ━━━━━━━━━━━━━━━━━━━━━━━━━━┓",
            "┃ ▾ ⌂ local ────── 1 session · 1 active┃",
            "┃  ▾ thurbox ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┃",
            "┃     ⠇ fix-osc52                      ┃",
            "┃ ▾ @ local ───────────────── 1 session┃",
            "┃  ▾ thurbox ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┃",
            "┃     ○ remote-build                   ┃",
            "┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛",
        ],
    );
}

#[test]
fn the_session_list_windows_more_rows_than_it_has_lines() {
    let host = host();
    let rows: Vec<SessionRow> = (0..20)
        .map(|n| row(&format!("session-{n:02}"), "thurbox", "idle"))
        .collect();
    publish(&host, &snapshot(rows));
    assert_frame(
        &text(&paint(&host, "sessions", 40, 10, true)),
        &[
            "┏ ▸ Sessions ━━━━━━━━━━━━━━━━━━━━━━━━━━┓",
            "┃ ▾ ⌂ local ─────────────── 20 sessions┃",
            "┃  ▾ thurbox ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┃",
            "┃     ○ session-00                     ┃",
            "┃     ○ session-01                     ┃",
            "┃     ○ session-02                     ┃",
            "┃     ○ session-03                     ┃",
            "┃     ○ session-04                     ┃",
            "┃     ○ session-05                     ┃",
            "┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━▼ 14 ┛",
        ],
    );
}

#[test]
fn the_session_list_keeps_its_columns_under_double_width_names() {
    let host = host();
    publish(
        &host,
        &snapshot(vec![
            row("修复终端宽度", "thurbox", "idle"),
            row("emoji-🚀-name", "thurbox", "blocked"),
            row("plain-name", "thurbox", "idle"),
        ]),
    );
    assert_frame(
        &text(&paint(&host, "sessions", 40, 8, true)),
        &[
            "┏ ▸ Sessions ━━━━━━━━━━━━━━━━━━━━━━━━━━┓",
            "┃ ▾ ⌂ local  !1 ──────────── 3 sessions┃",
            "┃  ▾ thurbox ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┃",
            "┃     ○ 修复终端宽度                   ┃",
            "┃     ◆ emoji-🚀-name  Blocked         ┃",
            "┃     ○ plain-name                     ┃",
            "┃                                      ┃",
            "┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛",
        ],
    );
}

#[test]
fn a_double_width_name_budgets_the_status_by_the_columns_it_takes() {
    let host = host();
    let world = snapshot(vec![row("修复终端宽度", "thurbox", "blocked")]);
    let meta = HashMap::from([(
        world.sessions[0].id.clone(),
        AgentMeta {
            activity: None,
            notification: Some("waiting for your review".to_string()),
        },
    )]);
    publish_meta(&host, &world, &HashMap::new(), &meta);
    assert_frame(
        &text(&paint(&host, "sessions", 40, 5, true)),
        &[
            "┏ ▸ Sessions ━━━━━━━━━━━━━━━━━━━━━━━━━━┓",
            "┃ ▾ ⌂ local  !1 ───────────── 1 session┃",
            "┃  ▾ thurbox ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┃",
            "┃     ◆ 修复终端宽度  waiting for your…┃",
            "┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛",
        ],
    );
}

#[test]
fn the_session_list_truncates_rather_than_overflows_when_narrow() {
    let host = host();
    publish(&host, &sample());
    assert_frame(
        &text(&paint(&host, "sessions", 22, 10, true)),
        &[
            "┏ ▸ Sessions ━━━━━━━━┓",
            "┃ ▾ ⌂ local  !2  5 s…┃",
            "┃  ▾ thurbox ╌╌╌╌╌╌╌╌┃",
            "┃   ▾ ⠇ fix-osc52    ┃",
            "┃       ○ fix-osc52-…┃",
            "┃     ◆ add-wsl-tests┃",
            "┃     ● perf-cache   ┃",
            "┃  ▾ website ╌╌╌╌╌╌╌╌┃",
            "┃     ○ update-deps  ┃",
            "┗━━━━━━━━━━━━━━━━━━━━┛",
        ],
    );
}

#[test]
fn the_selection_is_a_style_and_moves_with_j() {
    let host = host();
    publish(&host, &sample());
    let before = paint(&host, "sessions", 40, 12, true);
    press(&host, "sessions", 'j');
    let after = paint(&host, "sessions", 40, 12, true);

    assert_frame(
        &[
            style_runs(&before, 2),
            style_runs(&before, 3),
            style_runs(&after, 2),
            style_runs(&after, 3),
        ],
        &[
    "⟨Cyan/Reset/BOLD⟩┃⟨Reset/Reset/NONE⟩  ⟨Cyan/Reset/NONE⟩▾ thurbox⟨DarkGray/Reset/NONE⟩ ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌⟨Cyan/Reset/BOLD⟩┃",
    "⟨Cyan/Reset/BOLD⟩┃⟨White/Indexed(24)/BOLD⟩   ▾ ⠇ fix-osc52                      ⟨Cyan/Reset/BOLD⟩┃",
    "⟨Cyan/Reset/BOLD⟩┃⟨Reset/Reset/NONE⟩  ⟨Cyan/Reset/NONE⟩▾ thurbox⟨DarkGray/Reset/NONE⟩ ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌⟨Cyan/Reset/BOLD⟩┃",
    "⟨Cyan/Reset/BOLD⟩┃⟨Reset/Reset/NONE⟩   ⟨Cyan/Reset/NONE⟩▾ ⟨Yellow/Reset/NONE⟩⠇ ⟨White/Reset/BOLD⟩fix-osc52⟨Reset/Reset/NONE⟩                      ⟨Cyan/Reset/BOLD⟩┃",
        ],
    );
}

#[test]
fn a_host_row_with_no_room_for_its_rule_keeps_the_session_total_whole() {
    let host = host();
    publish(
        &host,
        &snapshot(vec![
            remote_row("a", "thurbox", "working", "abcdefghijklmnopqrst"),
            remote_row("b", "thurbox", "idle", "abcdefghijklmnopqrst"),
            remote_row("c", "thurbox", "idle", "abcdefghijklmnopqrst"),
            remote_row("d", "thurbox", "idle", "abcdefghijklmnopqrst"),
            remote_row("e", "thurbox", "idle", "abcdefghijklmnopqrst"),
        ]),
    );
    let frame = text(&paint(&host, "sessions", 39, 10, true));
    assert_eq!(
        frame[1], "┃ ▾ @ abcdefghijklmnopqrst  5 sessions┃",
        "{frame:#?}"
    );
}

#[test]
fn a_selected_host_row_is_one_bar_rule_included() {
    let host = host();
    publish(&host, &sample());
    press(&host, "sessions", 'k');
    press(&host, "sessions", 'k');
    assert_frame(
        &[style_runs(&paint(&host, "sessions", 40, 12, true), 1)],
        &[
    "⟨Cyan/Reset/BOLD⟩┃⟨White/Indexed(24)/BOLD⟩ ▾ ⌂ local  !2 ─ 5 sessions · 1 active⟨Cyan/Reset/BOLD⟩┃",
        ],
    );
}

fn paint_lua_node(source: &str, width: u16, height: u16) -> Buffer {
    let lua = mlua::Lua::new();
    let value: mlua::Value = lua.load(source).eval().expect("the table evaluates");
    let node =
        thurbox::kernel::convert::to_node(&value, "mods/90_test.lua").expect("the table converts");
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| render(frame, frame.area(), &node, &PlaceholderSurfaces))
        .expect("draw");
    terminal.backend().buffer().clone()
}

#[test]
fn a_styled_text_node_paints_its_style_across_its_whole_rect() {
    let buffer = paint_lua_node(
        r#"{ text = "hi", style = { fg = "white", bg = 24, bold = true } }"#,
        10,
        1,
    );
    for x in 0..10 {
        let cell = &buffer[(x, 0)];
        assert_eq!(
            cell.bg,
            Color::Indexed(24),
            "cell {x} should carry the band"
        );
        assert_eq!(cell.fg, Color::Gray, "cell {x} should carry the band's fg");
        assert!(cell.modifier.contains(Modifier::BOLD), "cell {x} bold");
    }
}

#[test]
fn a_span_keeps_the_colour_it_names_over_the_node_style() {
    let buffer = paint_lua_node(
        r#"{
             style = { fg = "white", bg = 24 },
             text = { { { text = "ab" }, { text = "cd", style = "green" } } },
           }"#,
        6,
        1,
    );
    assert_eq!(buffer[(1, 0)].fg, Color::Gray);
    assert_eq!(buffer[(2, 0)].fg, Color::Green);
    assert_eq!(
        buffer[(2, 0)].bg,
        Color::Indexed(24),
        "the band paints through"
    );
}

#[test]
fn a_frame_title_takes_the_alignment_it_asks_for() {
    let left = paint_lua_node(r#"{ text = "", frame = { title = "T" } }"#, 8, 3);
    let centre = paint_lua_node(
        r#"{ text = "", frame = { title = "T", title_align = "center" } }"#,
        8,
        3,
    );
    let right = paint_lua_node(
        r#"{ text = "", frame = { title = "T", title_align = "right" } }"#,
        8,
        3,
    );
    assert_frame(
        &[
            text(&left)[0].clone(),
            text(&centre)[0].clone(),
            text(&right)[0].clone(),
        ],
        &[
            "\u{256d}T\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}",
            "\u{256d}\u{2500}\u{2500}T\u{2500}\u{2500}\u{2500}\u{256e}",
            "\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}T\u{256e}",
        ],
    );
}

#[test]
fn a_frame_draws_the_border_type_it_asks_for() {
    assert_frame(
        &text(&paint_lua_node(r#"{ text = "", frame = true }"#, 4, 3)),
        &[
            "\u{256d}\u{2500}\u{2500}\u{256e}",
            "\u{2502}  \u{2502}",
            "\u{2570}\u{2500}\u{2500}\u{256f}",
        ],
    );
    for spelling in ["square", "plain"] {
        assert_frame(
            &text(&paint_lua_node(
                &format!(r#"{{ text = "", frame = {{ border_type = "{spelling}" }} }}"#),
                4,
                3,
            )),
            &[
                "\u{250c}\u{2500}\u{2500}\u{2510}",
                "\u{2502}  \u{2502}",
                "\u{2514}\u{2500}\u{2500}\u{2518}",
            ],
        );
    }
    assert_frame(
        &text(&paint_lua_node(
            r#"{ text = "", frame = { border_type = "thick" } }"#,
            4,
            3,
        )),
        &["┏━━┓", "┃  ┃", "┗━━┛"],
    );
}

const OVERLAY_NODE: &str = r#"{
    text = "",
    frame = {
      overlay = {
        top_left = { { text = "ab" } },
        top_right = { { text = "cd" } },
        bottom_left = { { text = "ef" } },
        bottom_right = { { text = "gh" } },
        right_column = { { text = "x" }, { text = "y" } },
      },
    },
  }"#;

#[test]
fn a_frame_overlay_paints_onto_the_border_cells() {
    assert_frame(
        &text(&paint_lua_node(OVERLAY_NODE, 10, 4)),
        &[
            "\u{256d}ab\u{2500}\u{2500}\u{2500}\u{2500}cd\u{256e}",
            "\u{2502}        x",
            "\u{2502}        y",
            "\u{2570}ef\u{2500}\u{2500}\u{2500}\u{2500}gh\u{256f}",
        ],
    );
}

#[test]
fn an_overlay_never_paints_over_a_corner() {
    let buffer = paint_lua_node(
        r#"{ text = "", frame = { overlay = { top_left = { { text = "abcdefgh" } } } } }"#,
        6,
        3,
    );
    assert_frame(
        &text(&buffer),
        &[
            "\u{256d}abcd\u{256e}",
            "\u{2502}    \u{2502}",
            "\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}",
        ],
    );
}

fn paint_probe(render_body: &str, hovered: Option<&str>, width: u16, height: u16) -> Buffer {
    let dir = tempfile::tempdir().expect("tempdir");
    let report = thurbox::kernel::bundled::materialize(dir.path());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    std::fs::write(
        dir.path().join("mods").join("95_probe.lua"),
        format!(
            "local widgets = require(\"lib.widgets\")\n\
             return {{ name = \"probe\", slot = \"center\", focusable = true,\n\
             render = function(ctx) {render_body} end }}"
        ),
    )
    .expect("write the probe");

    let host = LuaHost::new(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);
    let identity = hovered.map(|id| thurbox::kernel::node::Identity {
        id: Some(id.to_string()),
        classes: Vec::new(),
        role: Some("row".to_string()),
    });
    publish_hovered(&host, &snapshot(Vec::new()), identity.as_ref());

    let node = host
        .render(index_of(&host, "probe"), ctx(width, height, true))
        .expect("the probe renders")
        .node;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| render(frame, frame.area(), &node, &PlaceholderSurfaces))
        .expect("draw");
    terminal.backend().buffer().clone()
}

const PROBE_LIST: &str = r#"
    return widgets.list({
      rows = { { spans = "one", id = "a" }, { spans = "two", id = "b" } },
      selected = 1,
      height = ctx.height,
      selected_style = { bg = 24, fg = "white" },
      hover_style = { bg = 17 },
    })
"#;

#[test]
fn a_list_paints_its_selected_and_hovered_rows_edge_to_edge() {
    let buffer = paint_probe(PROBE_LIST, Some("b"), 12, 2);
    for x in 0..12 {
        assert_eq!(buffer[(x, 0)].bg, Color::Indexed(24), "selected cell {x}");
        assert_eq!(buffer[(x, 1)].bg, Color::Indexed(17), "hovered cell {x}");
    }
}

#[test]
fn a_list_without_the_style_props_paints_no_band() {
    let buffer = paint_probe(
        r#"return widgets.list({ rows = { "one" }, selected = 1, height = ctx.height })"#,
        None,
        12,
        1,
    );
    for x in 0..12 {
        assert_eq!(buffer[(x, 0)].bg, Color::Reset, "cell {x}");
    }
}

#[test]
fn the_agent_pane_with_nothing_selected() {
    let host = host();
    publish(&host, &snapshot(Vec::new()));
    assert_frame(
        &text(&paint(&host, "agent", 50, 8, true)),
        &[
            "┏ ▸ No Session ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓",
            "┃       ┌───────────────────────────────┐        ┃",
            "┃       │No active sessions             │        ┃",
            "┃       │                               │        ┃",
            "┃       │  F1      Help                 │        ┃",
            "┃       └───────────────────────────────┘        ┃",
            "┃                                                ┃",
            "┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛",
        ],
    );
}

#[test]
fn the_no_session_title_takes_the_borders_muted_colour_not_the_terminals_default() {
    let host = host();
    publish(&host, &snapshot(Vec::new()));
    let buffer = paint(&host, "agent", 50, 8, false);
    let row = text(&buffer);
    let title_x = row[0].find('N').expect("the title is on the top border") as u16;
    let border_x = row[0].find('─').expect("the border has a horizontal rule") as u16;
    let title_fg = buffer[(title_x, 0)].fg;
    assert_ne!(
        title_fg,
        Color::Reset,
        "the title must not fall back to the terminal's own default"
    );
    assert_eq!(
        title_fg,
        buffer[(border_x, 0)].fg,
        "the title and the border it sits in share one colour"
    );
}

#[test]
fn the_agent_pane_before_its_session_is_attached() {
    let host = host();
    publish(&host, &sample());
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render list");
    assert_frame(
        &text(&paint(&host, "agent", 60, 8, true)),
        &[
            "┏ ◀ F9 ━ Agent ━ Shell · F8 ━━ ▸ fix-osc52 (claude) [feat/…┓",
            "┃                                                          ┃",
            "┃                     terminal surface                     ┃",
            "┃                            fix                           ┃",
            "┃                       not attached                       ┃",
            "┃                                                          ┃",
            "┃                                                          ┃",
            "┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛",
        ],
    );
}

#[test]
fn the_agent_pane_closes_its_border_over_a_double_width_name() {
    let host = host();
    let world = snapshot(vec![row("修复终端宽度", "thurbox", "idle")]);
    publish(&host, &world);
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render list");
    assert_frame(
        &text(&paint(&host, "agent", 60, 4, true)),
        &[
            "┏ ◀ F9 ━ Agent ━ Shell · F8 ━━ ▸ 修复终端宽度 (claude) [fe…┓",
            "┃                     terminal surface                     ┃",
            "┃                       修复终端宽度                       ┃",
            "┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛",
        ],
    );
}

#[test]
fn the_agent_pane_explains_an_attach_failure() {
    let host = host();
    let errors: HashMap<String, String> = sample()
        .sessions
        .iter()
        .map(|row| (row.id.clone(), "can't find pane: %45".to_string()))
        .collect();
    publish_with(&host, &sample(), &errors);
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render list");
    assert_frame(
        &text(&paint(&host, "agent", 70, 8, true)),
        &[
            "┏ ◀ F9 ━ Agent ━ Shell · F8 ━━ ▸ fix-osc52 (claude) [feat/fix-osc52]…┓",
            "┃                                                                    ┃",
            "┃                                                                    ┃",
            "┃                          no live terminal                          ┃",
            "┃                        can't find pane: %45                        ┃",
            "┃                                                                    ┃",
            "┃                                                                    ┃",
            "┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛",
        ],
    );
}

struct LiveScreen {
    session: String,
    parser: vt100::Parser,
}

impl SurfaceProvider for LiveScreen {
    fn render_session(&self, frame: &mut Frame, area: Rect, session: &str, _scroll: u16) -> bool {
        if session != self.session {
            return false;
        }
        frame.render_widget(
            tui_term::widget::PseudoTerminal::new(self.parser.screen()).style(Style::default()),
            area,
        );
        true
    }

    fn render_program(&self, _frame: &mut Frame, _area: Rect, _surface: &str) -> ProgramPaint {
        ProgramPaint::NotStarted
    }
}

#[test]
fn the_agent_pane_paints_a_live_screen_inside_its_border() {
    let host = host();
    let world = sample();
    publish(&host, &world);
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render list");

    let mut parser = vt100::Parser::new(6.try_into().unwrap(), 58.try_into().unwrap(), 0);
    parser.process(b"$ cargo test\r\n   Compiling thurbox v0.0.0-dev\r\n\x1b[32mtest result: ok.\x1b[m 3 passed \xe2\x9c\x93 \xe4\xb8\xad\r\n$ ");
    let live = LiveScreen {
        session: world.sessions[0].id.clone(),
        parser,
    };
    assert_frame(
        &text(&paint_over(&host, "agent", 60, 8, true, &live)),
        &[
            "┏ ◀ F9 ━ Agent ━ Shell · F8 ━━ ▸ fix-osc52 (claude) [feat/…┓",
            "┃$ cargo test                                              ┃",
            "┃   Compiling thurbox v0.0.0-dev                           ┃",
            "┃test result: ok. 3 passed ✓ 中                            ┃",
            "┃$ █                                                       ┃",
            "┃                                                          ┃",
            "┃                                                          ┃",
            "┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛",
        ],
    );
}
