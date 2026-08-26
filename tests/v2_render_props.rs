//! Crash invariants of the render pipeline, as properties.
//!
//! The display bugs that hurt are the ones no example thought to write: a pane
//! resolved one cell wide, a selection dragged past the grid, a screen full of
//! double-width glyphs, a chord nobody expected in the pane it landed in. Each
//! property here is a *class* of input the pipeline must survive whole, and
//! the assertion is usually "does not error" — a Lua error blanks the pane in
//! the running interface, a paint panic costs the frame, and a panic on a
//! reader thread poisons the parser mutex for the life of the process.
//!
//! `tests/kernel_limits.rs` owns the instruction and memory ceilings;
//! `agent::control_mode`'s own proptests own byte transparency. This file is
//! the geometry and the input: sizes, positions, glyph widths and keys.

use proptest::prelude::*;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use thurbox::kernel::host::{KeyPress, LuaHost, Published, RenderContext};
use thurbox::kernel::layout::resolve;
use thurbox::kernel::paint::{normalize_ambiguous_width, render, PlaceholderSurfaces};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::selection::{
    extract_text_from_buffer, extract_text_from_screen, highlight_buffer, PaneBounds, Selection,
    TermPos,
};
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;

fn host() -> LuaHost {
    let host = LuaHost::new(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui"));
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

fn row(name: &str, status: &str) -> SessionRow {
    SessionRow {
        id: format!("{name}-0000-0000-0000-000000000000"),
        name: name.to_string(),
        agent: "claude".to_string(),
        status: status.to_string(),
        cwd: Some(std::path::PathBuf::from("/src/thurbox")),
        repo: Some("thurbox".to_string()),
        repos: vec!["thurbox".to_string()],
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

fn registry(host: &LuaHost) -> Registry {
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    registry
}

fn publish(host: &LuaHost, rows: Vec<SessionRow>) {
    let themes = Themes::load(None);
    let registry = registry(host);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    let snapshot = Snapshot {
        sessions: rows,
        taken_at_ms: 1_700_000_000_000,
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
        hovered: None,
    })
    .expect("publish");
}

/// A world with something in every column: a working spinner, a blocked dot,
/// and a name wider than most panes.
fn sample() -> Vec<SessionRow> {
    vec![
        row("fix-osc52", "working"),
        row("add-wsl-tests", "blocked"),
        row("a-name-much-longer-than-most-panes-are-wide", "idle"),
    ]
}

fn ctx(width: u16, height: u16) -> RenderContext {
    RenderContext {
        width,
        height,
        focused: true,
        elapsed: 1.0,
        frame: 1,
    }
}

fn index_of(host: &LuaHost, name: &str) -> usize {
    host.index_of(name)
        .unwrap_or_else(|| panic!("no plugin named {name}"))
}

/// Render one pane and paint it, the way `draw_plugin` does — the two halves
/// that can each fail on a size nobody tried.
fn render_and_paint(host: &LuaHost, index: usize, width: u16, height: u16) {
    let node = host
        .render(index, ctx(width, height))
        .unwrap_or_else(|e| panic!("{} at {width}x{height}: {e}", host.plugins[index].name))
        .node;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| render(frame, frame.area(), &node, &PlaceholderSurfaces))
        .expect("draw");
}

// --- sizes -----------------------------------------------------------------

#[test]
fn every_bundled_pane_renders_and_paints_at_any_size() {
    // The size a pane is handed is the arrangement's business, not the
    // plugin's, so every bundled pane must render and paint whatever it gets,
    // down to a single cell. The floats are included: a float's rect is
    // clamped to the screen, which on a tiny one is a rect nobody sized for.
    let host = host();
    publish(&host, sample());
    let panes = host.plugins.len();

    proptest!(ProptestConfig::with_cases(64), |(width in 1u16..=200, height in 1u16..=80)| {
        for index in 0..panes {
            render_and_paint(&host, index, width, height);
        }
    });
}

#[test]
fn the_arrangement_places_every_slot_inside_the_screen_and_apart() {
    // The arrangement is Lua arithmetic; `resolve` must hand out rects that
    // fit the area whatever that arithmetic says — a rect past the edge is a
    // paint that indexes out of the buffer — and two slots may not share a
    // cell, or the second paint silently overwrites the first.
    let host = host();
    proptest!(ProptestConfig::with_cases(128), |(width in 1u16..=300, height in 1u16..=100)| {
        let area = Rect { x: 0, y: 0, width, height };
        let placed = resolve(&host.arrangement(width, height).expect("layout"), area);
        for (i, slot) in placed.iter().enumerate() {
            prop_assert!(
                slot.rect.right() <= width && slot.rect.bottom() <= height,
                "slot {} at {:?} escapes a {width}x{height} screen",
                slot.slot,
                slot.rect
            );
            for other in &placed[i + 1..] {
                let apart = slot.rect.width == 0
                    || slot.rect.height == 0
                    || other.rect.width == 0
                    || other.rect.height == 0
                    || !slot.rect.intersects(other.rect);
                prop_assert!(
                    apart,
                    "slots {} {:?} and {} {:?} overlap at {width}x{height}",
                    slot.slot,
                    slot.rect,
                    other.slot,
                    other.rect
                );
            }
        }
    });
}

// --- keys ------------------------------------------------------------------

/// Any key a terminal can deliver: a printable character, or a named key,
/// with any modifiers. Weighted toward the letters, which are where the
/// single-letter chords live.
fn any_key() -> impl Strategy<Value = KeyPress> {
    let named = prop::sample::select(vec![
        "esc",
        "enter",
        "tab",
        "backspace",
        "delete",
        "up",
        "down",
        "left",
        "right",
        "home",
        "end",
        "pageup",
        "pagedown",
        "space",
        "f1",
        "f5",
        "f9",
        "f12",
    ]);
    let key = prop_oneof![
        3 => proptest::char::range(' ', '~').prop_map(|ch| KeyPress {
            name: ch.to_lowercase().to_string(),
            ch: Some(ch),
            shift: ch.is_uppercase(),
            ..KeyPress::default()
        }),
        1 => named.prop_map(|name| KeyPress {
            name: name.to_string(),
            ch: (name == "space").then_some(' '),
            ..KeyPress::default()
        }),
    ];
    (key, any::<bool>(), any::<bool>()).prop_map(|(mut key, ctrl, alt)| {
        key.ctrl = ctrl;
        key.alt = alt;
        key
    })
}

/// Press a key the way the loop does: a declared chord resolves through the
/// registry to `on_action` on the plugin that claimed it, anything else goes
/// to the focused plugin's `on_key`. Returns the error, if either raised one.
fn press(
    host: &LuaHost,
    registry: &Registry,
    focused: usize,
    key: &KeyPress,
) -> Result<(), String> {
    let name = host.plugins[focused].name.clone();
    if let Some(binding) = registry.resolve(key, Some(&name)) {
        if let Some(index) = host.index_of(&binding.plugin) {
            if host
                .on_action(index, &binding.action)
                .map_err(|e| format!("{}: {e}", binding.action))?
            {
                return Ok(());
            }
        }
    }
    host.on_key(focused, key)
        .map(|_| ())
        .map_err(|e| format!("on_key {key:?}: {e}"))
}

#[test]
fn no_key_sequence_makes_a_bundled_pane_throw() {
    // A pane that throws on a key is a pane that goes blank on that key, and
    // — because the loop routes the press before it paints — one that can
    // leave the interface with the pointer on something no longer drawn. So
    // every pane is fed keys it never declared, in modifiers it never named,
    // in orders nobody would type, and must neither error nor stop rendering.
    // The creation flow is opened first, because a float that is not up
    // ignores everything, and it is the pane with the most state to corrupt.
    let host = host();
    publish(&host, sample());
    let registry = registry(&host);
    let sessions = index_of(&host, "sessions");
    let flow = index_of(&host, "new_session");

    proptest!(ProptestConfig::with_cases(48), |(
        keys in proptest::collection::vec(any_key(), 1..40),
        focus_flow in any::<bool>(),
    )| {
        let opener = KeyPress { name: "n".into(), ch: Some('n'), ctrl: true, ..KeyPress::default() };
        press(&host, &registry, sessions, &opener).map_err(TestCaseError::fail)?;
        let focused = if focus_flow { flow } else { sessions };
        for key in &keys {
            press(&host, &registry, focused, key).map_err(TestCaseError::fail)?;
        }
        for &index in &[sessions, flow, index_of(&host, "agent"), index_of(&host, "search")] {
            let rendered = host.render(index, ctx(80, 24));
            prop_assert!(
                rendered.is_ok(),
                "{} stopped rendering after {keys:?}: {:?}",
                host.plugins[index].name,
                rendered.err()
            );
        }
        // Put the flow away so the next case starts from the same place.
        let esc = KeyPress { name: "esc".into(), ..KeyPress::default() };
        for _ in 0..4 {
            let _ = press(&host, &registry, flow, &esc);
        }
    });
}

// --- glyph widths and selection --------------------------------------------

/// Arbitrary printable content, weighted toward the widths that break column
/// math: ASCII, CJK (double-width), emoji, and the variation selector that
/// `normalize_ambiguous_width` exists for.
fn glyph_soup() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            4 => proptest::char::range(' ', '~'),
            1 => proptest::char::range('\u{4e00}', '\u{4eff}'),
            1 => Just('🚀'),
            1 => Just('\u{FE0F}'),
        ],
        0..80,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

/// Paint `lines` one per row into a fresh buffer of the given size.
fn buffer_of(lines: &[String], width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| {
            for (i, line) in lines.iter().enumerate().take(height as usize) {
                frame.render_widget(
                    ratatui::widgets::Paragraph::new(line.as_str()),
                    Rect {
                        x: 0,
                        y: i as u16,
                        width,
                        height: 1,
                    },
                );
            }
        })
        .expect("draw");
    terminal.backend().buffer().clone()
}

#[test]
fn a_selection_over_any_buffer_extracts_and_highlights() {
    // The pane rect, the drag endpoints and the buffer contents are three
    // independent coordinate systems; every combination — including endpoints
    // far outside both the pane and the buffer — must extract and highlight
    // without panicking, and extraction must stay inside the pane it was
    // confined to.
    proptest!(|(
        lines in proptest::collection::vec(glyph_soup(), 1..20),
        pane_x in 0u16..40, pane_y in 0u16..20,
        pane_w in 1u16..40, pane_h in 1u16..20,
        a_row in 0usize..80, a_col in 0usize..200,
        c_row in 0usize..80, c_col in 0usize..200,
    )| {
        let mut buffer = buffer_of(&lines, 60, 24);
        let pane = PaneBounds::from_rect(Rect { x: pane_x, y: pane_y, width: pane_w, height: pane_h });
        let mut selection = Selection::new(TermPos { row: a_row, col: a_col }, pane);
        selection.cursor = TermPos { row: c_row, col: c_col };

        let text = extract_text_from_buffer(&buffer, &selection);
        prop_assert!(
            text.lines().count() <= usize::from(pane_y + pane_h),
            "extraction escaped the pane: {} lines from a pane ending at row {}",
            text.lines().count(),
            pane_y + pane_h
        );
        highlight_buffer(
            &mut buffer,
            &selection,
            ratatui::style::Style::default().bg(ratatui::style::Color::Blue),
        );
    });
}

#[test]
fn a_selection_over_any_vt100_stream_extracts() {
    // The terminal-pane half: whatever bytes an agent emits — control
    // sequences, half a UTF-8 glyph, a wide character straddling the last
    // column — the grid that results must be selectable. This is the reader
    // whose panic would poison the parser mutex, so "never panics" is the
    // whole point.
    proptest!(ProptestConfig::with_cases(64), |(
        bytes in proptest::collection::vec(any::<u8>(), 0..2048),
        rows in 2u16..40, cols in 2u16..120,
        a_row in 0usize..60, a_col in 0usize..160,
        c_row in 0usize..60, c_col in 0usize..160,
    )| {
        let mut parser = vt100::Parser::new(rows, cols, 50);
        parser.process(&bytes);
        let pane = PaneBounds::from_rect(Rect { x: 1, y: 1, width: cols, height: rows });
        let mut selection = Selection::new(TermPos { row: a_row, col: a_col }, pane);
        selection.cursor = TermPos { row: c_row, col: c_col };
        let _ = extract_text_from_screen(parser.screen(), &selection, (1, 1));
    });
}

#[test]
fn normalizing_ambiguous_width_strips_the_selector_and_nothing_else() {
    // The one disagreement `normalize_ambiguous_width` resolves is U+FE0F; it
    // must strip every occurrence and leave every other cell as it was.
    proptest!(|(lines in proptest::collection::vec(glyph_soup(), 1..10))| {
        let width = 40u16;
        let height = lines.len() as u16;
        let before = buffer_of(&lines, width, height);
        let mut after = before.clone();

        normalize_ambiguous_width(&mut after);

        prop_assert_eq!(after.area, before.area);
        for y in 0..height {
            for x in 0..width {
                let was = before[(x, y)].symbol();
                let now = after[(x, y)].symbol();
                prop_assert!(
                    !now.contains('\u{FE0F}'),
                    "U+FE0F survived normalization at ({x},{y})"
                );
                prop_assert_eq!(
                    now,
                    was.replace('\u{FE0F}', ""),
                    "cell ({},{}) changed beyond the selector",
                    x,
                    y
                );
            }
        }
    });
}
