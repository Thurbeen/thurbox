use ratatui::{
    layout::{Margin, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use tui_term::widget::{Cursor, PseudoTerminal};

use super::focus_block;
use super::scrollbar::{self, ScrollbarGeom};
use super::theme::Theme;
use super::FocusLevel;
use crate::session::SessionInfo;

/// Render the terminal pane. Returns the scrollbar geometry when scrollback is
/// present (so the caller can record it as a drag target), else `None`.
pub fn render_terminal(
    frame: &mut Frame,
    area: Rect,
    parser: &mut vt100::Parser<impl vt100::Callbacks>,
    info: &SessionInfo,
    level: FocusLevel,
    is_shell: bool,
) -> Option<ScrollbarGeom> {
    let scroll_offset = parser.screen().scrollback();

    // Compute total scrollback by temporarily setting to max and reading back
    let total_scrollback = {
        parser.screen_mut().set_scrollback(usize::MAX);
        let max = parser.screen().scrollback();
        parser.screen_mut().set_scrollback(scroll_offset);
        max
    };

    let title = {
        let base = if is_shell {
            format!(" {} (shell) ", info.name)
        } else if let Some(wt) = info.worktrees.first() {
            format!(
                " {} ({}) [{}] [{}] ",
                info.name, info.agent, wt.branch, info.status
            )
        } else {
            format!(" {} ({}) [{}] ", info.name, info.agent, info.status)
        };
        if scroll_offset > 0 {
            // Insert scroll indicator before the trailing space
            let trimmed = base.trim_end();
            format!("{trimmed} [{scroll_offset}\u{2191}] ")
        } else {
            base
        }
    };

    let block = focus_block(&title, level);

    let mut pseudo_term = PseudoTerminal::new(parser.screen())
        .block(block)
        .style(Style::default().fg(Theme::text_primary()).bg(Color::Reset));

    // Hide cursor when scrolled up
    if scroll_offset > 0 {
        let mut cursor = Cursor::default();
        cursor.hide();
        pseudo_term = pseudo_term.cursor(cursor);
    }

    frame.render_widget(pseudo_term, area);

    // Render scrollbar when there's scrollback content.
    if total_scrollback == 0 {
        return None;
    }
    // Position scrollbar inside the block border.
    let scrollbar_area = area.inner(Margin {
        vertical: 1,
        horizontal: 0,
    });
    // Invert: offset 0 (bottom) → position at max, offset max (top) → position at 0.
    let position = total_scrollback.saturating_sub(scroll_offset);
    let (rows, _) = parser.screen().size();
    scrollbar::render_into(
        frame,
        scrollbar_area,
        total_scrollback,
        rows as usize,
        position,
    )
}

pub fn render_empty_terminal(frame: &mut Frame, area: Rect) {
    use ratatui::layout::{Alignment, Constraint, Direction, Layout};
    use ratatui::text::{Line, Span};

    let block = Block::default()
        .title(" No Session ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::text_muted()));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Centered hint box
    let box_width: u16 = 33;
    let box_height: u16 = 6;

    if inner.width >= box_width && inner.height >= box_height {
        let vert = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(box_height),
                Constraint::Min(0),
            ])
            .split(inner);
        let horiz = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(box_width),
                Constraint::Min(0),
            ])
            .split(vert[1]);
        let center = horiz[1];

        let hint_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Theme::border_unfocused()));

        let hint_inner = hint_block.inner(center);
        frame.render_widget(hint_block, center);

        let lines = vec![
            Line::from(Span::styled(
                "No active sessions",
                Style::default().fg(Theme::text_secondary()),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("  Ctrl+N", Theme::keybind()),
                Span::styled("  New session", Style::default().fg(Theme::text_muted())),
            ]),
            Line::from(vec![
                Span::styled("  F1    ", Theme::keybind()),
                Span::styled("  Help", Style::default().fg(Theme::text_muted())),
            ]),
        ];
        frame.render_widget(Paragraph::new(lines).alignment(Alignment::Left), hint_inner);
    }
}

/// Property/fuzz tests proving the **rendering** path is transparent: whatever
/// the vt100 model holds is exactly what lands in the frame buffer, and no raw
/// control byte ever leaks into a rendered cell. Complements the transport
/// proptests in `agent::control_mode` — together they cover the whole
/// agent-bytes → screen pipeline, so a green suite rules thurbox out as a source
/// of glitched/stray characters.
#[cfg(test)]
mod render_proptests {
    use proptest::prelude::*;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::layout::{Position, Rect};
    use ratatui::Terminal;

    use super::render_terminal;
    use crate::session::SessionInfo;
    use crate::ui::selection::{PaneBounds, Selection, TermPos};
    use crate::ui::FocusLevel;

    const COLS: u16 = 40;
    const ROWS: u16 = 12;
    // The block draws a 1-cell border on each side, so the vt100 grid (and the
    // rendered content region) is the area shrunk by one in every direction.
    const INNER_COLS: u16 = COLS - 2;
    const INNER_ROWS: u16 = ROWS - 2;

    /// Treat empty and single-space symbols as equivalent, so blank cells
    /// compare equal across ratatui (renders `" "`) and vt100 (empty contents
    /// for a blank cell or a wide char's trailing column).
    fn norm(sym: &str) -> &str {
        if sym.is_empty() || sym == " " {
            " "
        } else {
            sym
        }
    }

    /// Render `bytes` through `render_terminal` into a headless `TestBackend`.
    /// Returns the full frame buffer, the vt100 model's visible rows, and the
    /// rendered content region's visible rows — the latter two with the cursor
    /// cell masked on *both* sides (a focused pane draws a block cursor glyph
    /// that has no counterpart in the model, and is UI chrome, not content).
    fn render(bytes: &[u8]) -> (Buffer, Vec<String>, Vec<String>) {
        crate::ui::theme::ensure_initialized();
        let mut parser = vt100::Parser::new(INNER_ROWS, INNER_COLS, 0);
        parser.process(bytes);
        let info = SessionInfo::new("t".to_string());
        let mut term = Terminal::new(TestBackend::new(COLS, ROWS)).unwrap();
        term.draw(|f| {
            render_terminal(
                f,
                Rect::new(0, 0, COLS, ROWS),
                &mut parser,
                &info,
                FocusLevel::Focused,
                false,
            );
        })
        .unwrap();
        let buf = term.backend().buffer().clone();

        let screen = parser.screen();
        let (cur_row, cur_col) = screen.cursor_position();
        let cursor_visible = !screen.hide_cursor();
        let masked = |r: u16, c: u16| cursor_visible && r == cur_row && c == cur_col;

        let model: Vec<String> = (0..INNER_ROWS)
            .map(|r| {
                let mut s = String::new();
                for c in 0..INNER_COLS {
                    if masked(r, c) {
                        s.push(' ');
                        continue;
                    }
                    let sym = screen
                        .cell(r, c)
                        .map(|cell| cell.contents())
                        .unwrap_or_default();
                    s.push_str(norm(sym));
                }
                s.trim_end().to_string()
            })
            .collect();

        let rendered: Vec<String> = (1..ROWS - 1)
            .map(|y| {
                let mut s = String::new();
                for x in 1..COLS - 1 {
                    if masked(y - 1, x - 1) {
                        s.push(' ');
                        continue;
                    }
                    let sym = buf
                        .cell(Position::new(x, y))
                        .map(|c| c.symbol())
                        .unwrap_or("");
                    s.push_str(norm(sym));
                }
                s.trim_end().to_string()
            })
            .collect();

        (buf, model, rendered)
    }

    /// A strategy producing adversarial "agent output": text, CSI/OSC escape
    /// sequences, wide/Unicode chars, control bytes, and raw bytes interleaved.
    fn agent_output() -> impl Strategy<Value = Vec<u8>> {
        let token = prop_oneof![
            // printable ASCII runs
            proptest::string::string_regex("[ -~]{0,8}")
                .unwrap()
                .prop_map(String::into_bytes),
            // CSI sequence: ESC [ <params> <final>
            (
                proptest::string::string_regex("[0-9;]{0,6}").unwrap(),
                prop::sample::select(vec![b'm', b'H', b'J', b'K', b'A', b'B', b'C', b'D']),
            )
                .prop_map(|(params, fin)| {
                    let mut v = vec![0x1b, b'['];
                    v.extend(params.bytes());
                    v.push(fin);
                    v
                }),
            // OSC sequence: ESC ] <text> BEL
            proptest::string::string_regex("[ -~]{0,8}")
                .unwrap()
                .prop_map(|s| {
                    let mut v = vec![0x1b, b']'];
                    v.extend(s.bytes());
                    v.push(0x07);
                    v
                }),
            // wide / multi-byte / combining chars
            prop::sample::select(vec!["你好", "🎉", "café", "日本語", "→★", "a\u{0301}"])
                .prop_map(|s| s.as_bytes().to_vec()),
            // lone control bytes
            prop::sample::select(vec![b'\n', b'\r', b'\t', 0x08, 0x07]).prop_map(|b| vec![b]),
            // arbitrary raw bytes (incl. invalid UTF-8 fragments)
            prop::collection::vec(any::<u8>(), 0..4),
        ];
        prop::collection::vec(token, 0..40).prop_map(|tokens| tokens.concat())
    }

    proptest! {
        /// No rendered cell — anywhere in the frame, content or chrome — ever
        /// contains a control character. A glitch where an escape sequence is
        /// shown as literal text would surface here.
        #[test]
        fn rendered_cells_never_contain_control_chars(bytes in agent_output()) {
            let (buf, _, _) = render(&bytes);
            for y in buf.area.y..buf.area.y + buf.area.height {
                for x in buf.area.x..buf.area.x + buf.area.width {
                    if let Some(cell) = buf.cell(Position::new(x, y)) {
                        prop_assert!(
                            !cell.symbol().chars().any(|c| c.is_control()),
                            "control char rendered at ({},{}): {:?}",
                            x,
                            y,
                            cell.symbol()
                        );
                    }
                }
            }
        }

        /// The rendered content region matches the vt100 model row for row — the
        /// render step neither drops, duplicates, nor invents characters.
        #[test]
        fn rendered_content_matches_vt100_model(bytes in agent_output()) {
            let (_buf, model, rendered) = render(&bytes);
            prop_assert_eq!(rendered, model);
        }

        /// The selection overlay is non-destructive: highlighting an arbitrary
        /// rectangle changes only cell *styles*, never the glyphs.
        #[test]
        fn selection_overlay_preserves_glyphs(
            bytes in agent_output(),
            a in (0u16..INNER_ROWS, 0u16..INNER_COLS),
            b in (0u16..INNER_ROWS, 0u16..INNER_COLS),
        ) {
            let (original, _, _) = render(&bytes);
            let mut buf = original.clone();
            let pane = PaneBounds::from_rect(Rect::new(1, 1, INNER_COLS, INNER_ROWS));
            let sel = Selection {
                anchor: TermPos { row: (a.0 + 1) as usize, col: (a.1 + 1) as usize },
                cursor: TermPos { row: (b.0 + 1) as usize, col: (b.1 + 1) as usize },
                dragging: false,
                pane,
            };
            crate::ui::selection::highlight_buffer(
                &mut buf,
                &sel,
                ratatui::style::Style::default().fg(ratatui::style::Color::Red),
            );
            for y in buf.area.y..buf.area.y + buf.area.height {
                for x in buf.area.x..buf.area.x + buf.area.width {
                    let pos = Position::new(x, y);
                    prop_assert_eq!(
                        buf.cell(pos).map(|c| c.symbol()),
                        original.cell(pos).map(|c| c.symbol()),
                        "glyph changed by selection at ({},{})",
                        x,
                        y
                    );
                }
            }
        }

        // NOTE: chunk-split invariance is tested in `agent::backend` instead,
        // against the real UTF-8-boundary-safe chunker (`utf8_ready_prefix_len`)
        // that feeds the vt100 parser. vt100 itself is *not* split-invariant for
        // chunks that end mid-codepoint (it can swallow a following newline), so
        // the guarantee lives in thurbox's reader loop, not in vt100 — and that
        // is where the test belongs (the `ui` layer may not reference `agent`).
    }
}
