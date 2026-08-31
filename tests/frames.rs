//! The bundled panes' frames, pinned cell for cell.
//!
//! The other surface files assert substrings and single cells, which is precise
//! but sparse: a misaligned border, a broken group header, a name that shears
//! its row because a double-width glyph was counted as one, a selection that
//! stopped being painted — every one of those passes a `contains`. Here the
//! whole painted frame is the assertion.
//!
//! The expected frames are literals in this file rather than snapshot files:
//! a change to a pane is reviewed in the same diff as the code that made it,
//! and there is no tool to learn and no accept-all to reach for. When a frame
//! changes on purpose, a failing test prints the new one as a literal to paste
//! (`assert_frame`). Everything that could move is pinned — the `default`
//! preset by name, a fixed `elapsed` for the spinner, a fixed snapshot — so a
//! frame that changes did so because a pane did.
//!
//! Deliberately absent: the floats (creation flow, confirm, restore), which
//! open through state a real interaction writes and are covered by their own
//! interaction tests; and the arrangement as a whole, which is the binary's
//! `draw` and is asserted on a real terminal in `tests/tui_e2e.rs`.

use std::collections::HashMap;

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::Frame;
use ratatui::Terminal;

use thurbox::kernel::host::{KeyPress, LuaHost, Published, RenderContext};
use thurbox::kernel::paint::{render, PlaceholderSurfaces, ProgramPaint, SurfaceProvider};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;

// --- the world --------------------------------------------------------------

fn host() -> LuaHost {
    let host = LuaHost::new(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui"));
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

/// The `default` preset by name, never the environment's active choice: the
/// styled frame below records real colours.
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

fn publish_with(host: &LuaHost, snapshot: &Snapshot, attach_errors: &HashMap<String, String>) {
    let themes = themes();
    let registry = registry(host);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
        snapshot,
        attach_errors,
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

fn row(name: &str, repo: &str, status: &str) -> SessionRow {
    SessionRow {
        id: format!("{name}-0000-0000-0000-000000000000"),
        name: name.to_string(),
        agent: "claude".to_string(),
        status: status.to_string(),
        cwd: Some(std::path::PathBuf::from(format!("/src/{repo}"))),
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
        stopped: false,
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

/// The one sample most frames draw from: two repo groups, every status, and a
/// parent → child pair so the tree prefix is on record.
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

fn ctx(width: u16, height: u16, focused: bool) -> RenderContext {
    RenderContext {
        width,
        height,
        focused,
        // Fixed, so the working spinner picks the same frame every run.
        elapsed: 1.0,
        frame: 1,
    }
}

fn index_of(host: &LuaHost, name: &str) -> usize {
    host.index_of(name)
        .unwrap_or_else(|| panic!("no plugin named {name}"))
}

/// Press a key the way the loop does: a declared chord resolves through the
/// registry to `on_action`, anything else goes to `on_key`.
fn press(host: &LuaHost, plugin: &str, ch: char) {
    let key = KeyPress {
        name: ch.to_lowercase().to_string(),
        ch: Some(ch),
        ..KeyPress::default()
    };
    if let Some(binding) = registry(host).resolve(&key, Some(plugin)) {
        if let Some(index) = host.index_of(&binding.plugin) {
            if host.on_action(index, &binding.action).expect("action") {
                return;
            }
        }
    }
    host.on_key(index_of(host, plugin), &key).expect("key");
}

// --- painting ---------------------------------------------------------------

/// Render one pane at a size and paint it into a fresh buffer.
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

/// The buffer as rows of text, trailing blanks trimmed, and read as a
/// terminal shows it: a double-width glyph owns the blank cell after it, so
/// the cell is skipped rather than printed as a space — which is what keeps a
/// literal aligned in a monospace view and a CJK row the same length as its
/// neighbours.
fn text(buffer: &Buffer) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    let area = buffer.area;
    (0..area.height)
        .map(|y| {
            let mut line = String::new();
            let mut owed = 0;
            for x in 0..area.width {
                if owed > 0 {
                    owed -= 1;
                    continue;
                }
                let symbol = buffer[(x, y)].symbol();
                owed = symbol.width().saturating_sub(1);
                line.push_str(symbol);
            }
            line.trim_end().to_string()
        })
        .collect()
}

/// One row of the buffer as style *runs*: consecutive cells sharing a style
/// collapse into `⟨fg/bg/mods⟩text`. Compact enough to read, precise enough
/// that a lost selection highlight, a dropped dim or a recoloured status dot
/// changes it.
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

/// Compare rows of text with the expected literal; on a mismatch, print the
/// actual rows as a literal ready to paste over the old one.
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

// --- the session list -------------------------------------------------------

#[test]
fn the_session_list_groups_by_repo_and_nests_a_child_under_its_parent() {
    let host = host();
    publish(&host, &sample());
    assert_frame(
        &text(&paint(&host, "sessions", 40, 12, true)),
        &[
            "╭ Sessions ───────────────────────⠇○◆●○╮",
            "│── thurbox ───────────────────────────│",
            "│ ⠇ ⑂ fix-osc52                        │",
            "│ ○ └ ⑂ fix-osc52-tests                │",
            "│ ◆ ⑂ add-wsl-tests  Blocked           │",
            "│ ● ⑂ perf-cache                       │",
            "│── website ───────────────────────────│",
            "│ ○ ⑂ update-deps                      │",
            "│                                      │",
            "│                                      │",
            "│                                      │",
            "╰──────────────────────────────────────╯",
        ],
    );
}

#[test]
fn the_session_list_windows_more_rows_than_it_has_lines() {
    // Twenty, not more: the border title carries one status dot per session
    // and eats the ` Sessions ` label once they outnumber the width. This
    // frame is about the window and its overflow marker, so it stops short of
    // that.
    let host = host();
    let rows: Vec<SessionRow> = (0..20)
        .map(|n| row(&format!("session-{n:02}"), "thurbox", "idle"))
        .collect();
    publish(&host, &snapshot(rows));
    assert_frame(
        &text(&paint(&host, "sessions", 40, 10, true)),
        &[
            "╭ Sessions ────────○○○○○○○○○○○○○○○○○○○○╮",
            "│── thurbox ───────────────────────────│",
            "│ ○ ⑂ session-00                       │",
            "│ ○ ⑂ session-01                       │",
            "│ ○ ⑂ session-02                       │",
            "│ ○ ⑂ session-03                       │",
            "│ ○ ⑂ session-04                       │",
            "│ ○ ⑂ session-05                       │",
            "│ ○ ⑂ session-06                       │",
            "╰─────────────────────────────────▼ 13 ╯",
        ],
    );
}

#[test]
fn the_session_list_keeps_its_columns_under_double_width_names() {
    // Where column budgets go wrong: a CJK or emoji name that miscounts its
    // width shears the whole row, and the border with it.
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
            "╭ Sessions ─────────────────────────○◆○╮",
            "│── thurbox ───────────────────────────│",
            "│ ○ ⑂ 修复终端宽度                     │",
            "│ ◆ ⑂ emoji-🚀-name  Blocked           │",
            "│ ○ ⑂ plain-name                       │",
            "│                                      │",
            "│                                      │",
            "╰──────────────────────────────────────╯",
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
            "╭ Sessions ─────⠇○◆●○╮",
            "│── thurbox ─────────│",
            "│ ⠇ ⑂ fix-osc52      │",
            "│ ○ └ ⑂ fix-osc52-tes│",
            "│ ◆ ⑂ add-wsl-tests  │",
            "│ ● ⑂ perf-cache     │",
            "│── website ─────────│",
            "│ ○ ⑂ update-deps    │",
            "│                    │",
            "╰────────────────────╯",
        ],
    );
}

#[test]
fn the_selection_is_a_style_and_moves_with_j() {
    // The selection is a STYLE, not a glyph (v1's rule), so only a styled row
    // can see it move — and only one can see a status dot's colour. Two rows
    // on record: the one the cursor left and the one it landed on.
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
            "⟨LightCyan/Reset/NONE⟩│⟨White/Indexed(24)/BOLD⟩ ⠇ ⑂ fix-osc52                        ⟨LightCyan/Reset/NONE⟩│",
            "⟨LightCyan/Reset/NONE⟩│⟨Green/Reset/NONE⟩ ○ ⟨DarkGray/Reset/NONE⟩└ ⟨Green/Reset/NONE⟩⑂ ⟨White/Reset/NONE⟩fix-osc52-tests⟨Reset/Reset/NONE⟩                ⟨LightCyan/Reset/NONE⟩│",
            "⟨LightCyan/Reset/NONE⟩│⟨Yellow/Reset/NONE⟩ ⠇ ⟨Green/Reset/NONE⟩⑂ ⟨White/Reset/NONE⟩fix-osc52⟨Reset/Reset/NONE⟩                        ⟨LightCyan/Reset/NONE⟩│",
            "⟨LightCyan/Reset/NONE⟩│⟨White/Indexed(24)/BOLD⟩ ○ └ ⑂ fix-osc52-tests                ⟨LightCyan/Reset/NONE⟩│",
        ],
    );
}

// --- the agent pane ---------------------------------------------------------

#[test]
fn the_agent_pane_with_nothing_selected() {
    let host = host();
    publish(&host, &snapshot(Vec::new()));
    assert_frame(
        &text(&paint(&host, "agent", 50, 8, true)),
        &[
            "┌ No Session ────────────────────────────────────┐",
            "│       ┌───────────────────────────────┐        │",
            "│       │No active sessions             │        │",
            "│       │                               │        │",
            "│       │  F1      Help                 │        │",
            "│       └───────────────────────────────┘        │",
            "│                                                │",
            "└────────────────────────────────────────────────┘",
        ],
    );
}

#[test]
fn the_agent_pane_before_its_session_is_attached() {
    // Nothing live behind the surface, so it draws the detached notice — the
    // frame a fresh boot shows before the first attach lands.
    let host = host();
    publish(&host, &sample());
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render list");
    assert_frame(
        &text(&paint(&host, "agent", 60, 8, true)),
        &[
            "╭ ◀ F9 ─ Agent ─ Shell · F8 ── fix-osc52 (claude) [feat/fi…╮",
            "│                                                          │",
            "│                     terminal surface                     │",
            "│                            fix                           │",
            "│                       not attached                       │",
            "│                                                          │",
            "│                                                          │",
            "╰──────────────────────────────────────────────────────────╯",
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
            "╭ ◀ F9 ─ Agent ─ Shell · F8 ── fix-osc52 (claude) [feat/fix-osc52] […╮",
            "│                                                                    │",
            "│                                                                    │",
            "│                          no live terminal                          │",
            "│                        can't find pane: %45                        │",
            "│                                                                    │",
            "│                                                                    │",
            "╰────────────────────────────────────────────────────────────────────╯",
        ],
    );
}

/// A provider with a real vt100 screen behind one session, painted the way
/// `Terminals` paints a live pane — so the frame shows where the agent's
/// output lands inside the pane's chrome.
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
    // The surface is the one node kind with a process behind it; every other
    // frame here has a placeholder there. This one puts a real grid behind
    // it: the pane must give the screen its whole interior and nothing of
    // its chrome, with a double-width glyph landing on its own two cells.
    let host = host();
    let world = sample();
    publish(&host, &world);
    host.render(index_of(&host, "sessions"), ctx(40, 12, true))
        .expect("render list");

    let mut parser = vt100::Parser::new(6, 58, 0);
    parser.process(b"$ cargo test\r\n   Compiling thurbox v0.0.0-dev\r\n\x1b[32mtest result: ok.\x1b[m 3 passed \xe2\x9c\x93 \xe4\xb8\xad\r\n$ ");
    let live = LiveScreen {
        session: world.sessions[0].id.clone(),
        parser,
    };
    assert_frame(
        &text(&paint_over(&host, "agent", 60, 8, true, &live)),
        &[
            "╭ ◀ F9 ─ Agent ─ Shell · F8 ── fix-osc52 (claude) [feat/fi…╮",
            "│$ cargo test                                              │",
            "│   Compiling thurbox v0.0.0-dev                           │",
            "│test result: ok. 3 passed ✓ 中                            │",
            "│$ █                                                       │",
            "│                                                          │",
            "│                                                          │",
            "╰──────────────────────────────────────────────────────────╯",
        ],
    );
}
