//! Paint a resolved [`Node`] tree into a ratatui frame.
//!
//! Everything here is arithmetic plus ratatui calls — no Lua is touched, which
//! is what lets a plugin's tree be rendered after its VM has been thrown away.
//!
//! The tree arriving here has already been through the layout pass, so this
//! walks it with rects rather than computing them: a `box` divides its own
//! rect among children ([`super::node::divide`]) and recurses.

use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Borders as RatBorders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::node::{
    divide, to_line, Align, Axis, Borders, Frame as NodeFrame, Identity, Node, SurfaceSource,
};

/// A painted node that carried identity, and the rect it went into.
///
/// v1 built the same registry by hand, one `record_click` per renderer
/// (`App::click_targets`); here it falls out of the paint walk, so a hitbox
/// cannot drift from the cells it covers and a plugin nobody has written yet is
/// clickable the moment it gives a node an `id`.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub rect: Rect,
    pub identity: Identity,
}

/// Fills a session-backed surface.
///
/// The kernel owns this because a live terminal grid cannot round-trip through
/// Lua tables at 20 fps — see design.md D3. A plugin *places and frames* a
/// surface; the provider paints inside the rect it was given.
///
/// Painting into the frame rather than returning lines is deliberate: a live
/// terminal is rendered by `tui_term` straight off the vt100 screen, which
/// preserves colour, wide characters and the cursor. Flattening to `Line`s
/// first would mean reimplementing that badly.
pub trait SurfaceProvider {
    /// Paint `session` into `area`. Return `false` when there is nothing live
    /// behind it, and the caller draws a placeholder instead.
    fn render_session(&self, frame: &mut Frame, area: Rect, session: &str, scroll: u16) -> bool;

    /// Paint the program `surface` names into `area`, for a pane a plugin owns.
    ///
    /// Returns what to draw instead when there is nothing to paint, so a pane
    /// that has not been started and one whose program has ended read
    /// differently — a frozen grid and an empty box are the two answers this
    /// exists to avoid.
    fn render_program(&self, frame: &mut Frame, area: Rect, surface: &str) -> ProgramPaint;
}

/// What a program surface had to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramPaint {
    /// Painted its screen.
    Painted,
    /// Nothing has been started for this pane.
    NotStarted,
    /// The program ran and ended. Named, because "which one exited" is the first
    /// thing anybody asks.
    Exited(String),
}

/// A provider with nothing behind it — every surface falls back to the
/// placeholder.
///
/// Used by tests, so the arrangement, framing and sizing of a surface stay
/// testable without a tmux server.
pub struct PlaceholderSurfaces;

impl SurfaceProvider for PlaceholderSurfaces {
    fn render_session(
        &self,
        _frame: &mut Frame,
        _area: Rect,
        _session: &str,
        _scroll: u16,
    ) -> bool {
        false
    }

    fn render_program(&self, _frame: &mut Frame, _area: Rect, _surface: &str) -> ProgramPaint {
        ProgramPaint::NotStarted
    }
}

/// What a session-backed surface shows when nothing live is behind it.
/// A centred two-line notice, for a surface with nothing to paint.
fn render_notice(frame: &mut Frame, area: Rect, title: &str, detail: &str) {
    // Clears its own rect: it stands in for a surface and paints only the few
    // centred rows, so whatever the layout left underneath would show through.
    frame.render_widget(Clear, area);
    let mut lines = Vec::new();
    let blank = usize::from(area.height).saturating_sub(2) / 2;
    for _ in 0..blank {
        lines.push(Line::default());
    }
    lines.push(
        Line::from(title.to_string())
            .style(Style::default().add_modifier(Modifier::BOLD))
            .centered(),
    );
    lines.push(
        Line::from(detail.to_string())
            .style(Style::default().add_modifier(Modifier::DIM))
            .centered(),
    );
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_detached(frame: &mut Frame, area: Rect, session: &str) {
    // Clears its own rect, for the reason `render_notice` does.
    frame.render_widget(Clear, area);
    let mut lines = Vec::new();
    let blank = usize::from(area.height).saturating_sub(3) / 2;
    for _ in 0..blank {
        lines.push(Line::default());
    }
    lines.push(
        Line::from("terminal surface")
            .style(Style::default().add_modifier(Modifier::BOLD))
            .centered(),
    );
    lines.push(
        Line::from(short_id(session))
            .style(Style::default().add_modifier(Modifier::DIM))
            .centered(),
    );
    lines.push(
        Line::from("not attached")
            .style(Style::default().add_modifier(Modifier::DIM))
            .centered(),
    );
    frame.render_widget(Paragraph::new(lines), area);
}

fn short_id(id: &str) -> String {
    id.split('-').next().unwrap_or(id).to_string()
}

/// Paint `node` into `area`.
pub fn render(frame: &mut Frame, area: Rect, node: &Node, surfaces: &dyn SurfaceProvider) {
    render_recording(frame, area, node, surfaces, &mut Vec::new());
}

/// Paint `node` into `area`, appending a [`Hit`] for every identified node.
///
/// Hits are appended **pre-order** — a parent before its children — so a caller
/// scanning in reverse finds the innermost node under a point first, and finds
/// a whole-pane fallback recorded before the tree only when nothing inside
/// matched. That is v1's ordering rule (specific targets recorded before the
/// pane's own rect, first match wins) read backwards.
pub fn render_recording(
    frame: &mut Frame,
    area: Rect,
    node: &Node,
    surfaces: &dyn SurfaceProvider,
    hits: &mut Vec<Hit>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // The OUTER rect, before any border is subtracted: v1's on-border
    // affordances (the collapse chevron, the tab strip) are painted on the
    // frame's own cells, so a hitbox that stopped at the inner rect could never
    // catch them.
    if !node.identity().is_empty() {
        hits.push(Hit {
            rect: area,
            identity: node.identity().clone(),
        });
    }
    let inner = match node.frame() {
        Some(spec) => {
            let block = build_block(spec);
            let inner = block.inner(area);
            frame.render_widget(block, area);
            pad(inner, spec.padding)
        }
        None => area,
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    match node {
        Node::Text {
            lines,
            align,
            wrap,
            scroll,
            ..
        } => {
            let text: Vec<Line> = lines.iter().map(|runs| to_line(runs)).collect();
            let mut paragraph = Paragraph::new(text).scroll((*scroll, 0));
            paragraph = match align {
                Align::Left => paragraph.left_aligned(),
                Align::Center => paragraph.centered(),
                Align::Right => paragraph.right_aligned(),
            };
            if *wrap {
                paragraph = paragraph.wrap(Wrap { trim: false });
            }
            frame.render_widget(paragraph, inner);
        }

        Node::Box {
            axis,
            gap,
            children,
            ..
        } => {
            if children.is_empty() {
                return;
            }
            let sizes: Vec<_> = children.iter().map(Node::size).collect();
            let total = match axis {
                Axis::Vertical => inner.height,
                Axis::Horizontal => inner.width,
            };
            let lengths = divide(total, &sizes, *gap);
            let mut cursor = match axis {
                Axis::Vertical => inner.y,
                Axis::Horizontal => inner.x,
            };
            for (child, length) in children.iter().zip(lengths) {
                let rect = match axis {
                    Axis::Vertical => Rect {
                        x: inner.x,
                        y: cursor,
                        width: inner.width,
                        height: length,
                    },
                    Axis::Horizontal => Rect {
                        x: cursor,
                        y: inner.y,
                        width: length,
                        height: inner.height,
                    },
                };
                render_recording(frame, rect, child, surfaces, hits);
                cursor = cursor.saturating_add(length).saturating_add(*gap);
            }
        }

        Node::Input {
            value,
            cursor,
            placeholder,
            focused,
            style,
            ..
        } => {
            let showing_placeholder = value.is_empty();
            let text = if showing_placeholder {
                placeholder
            } else {
                value
            };
            let mut line_style = *style;
            if showing_placeholder {
                line_style = line_style.add_modifier(Modifier::DIM);
            }
            frame.render_widget(
                Paragraph::new(Line::from(text.clone()).style(line_style)),
                inner,
            );
            // A real caret, in the one field that claims it — empty or not. A
            // field showing its placeholder is still the field being typed
            // into: keying the caret on the value being non-empty instead, as
            // this did, made the cursor blink into existence on the first
            // keystroke and out of it on the last backspace, and handed it to
            // whichever *other* field on screen happened to hold text. The
            // placeholder is dim, so a caret at its start cannot be mistaken
            // for one. Clamped into the rect so a cursor past the end cannot
            // paint outside it.
            if *focused {
                let offset = (*cursor).min(value.chars().count());
                let column = inner
                    .x
                    .saturating_add(offset.min(usize::from(inner.width)) as u16);
                if column < inner.x.saturating_add(inner.width) {
                    frame.set_cursor_position((column, inner.y));
                }
            }
        }

        Node::Surface { source, scroll, .. } => {
            // No blanket `Clear` here. `swap_buffers` resets the frame buffer
            // before every draw, so nothing stale survives to be erased, and a
            // live terminal covers its rect itself — clearing first was a second
            // full-grid write of ~9,000 cells for a frame about to overwrite
            // them (ADR-P17). Whatever does *not* cover its rect clears itself
            // instead: the paragraph below, `render_notice`/`render_detached`,
            // and `clear_uncovered` for a grid still catching up to a resize.
            match source {
                SurfaceSource::Cells(cells) => {
                    frame.render_widget(Clear, inner);
                    let lines: Vec<Line> = cells.iter().map(|runs| to_line(runs)).collect();
                    frame.render_widget(Paragraph::new(lines).scroll((*scroll, 0)), inner);
                }
                SurfaceSource::Session(id) => {
                    if !surfaces.render_session(frame, inner, id, *scroll) {
                        render_detached(frame, inner, id);
                    }
                }
                SurfaceSource::Program(id) => match surfaces.render_program(frame, inner, id) {
                    ProgramPaint::Painted => {}
                    ProgramPaint::NotStarted => {
                        render_notice(frame, inner, "program surface", "nothing started here yet")
                    }
                    // Said rather than shown: the grid a finished program left
                    // behind looks exactly like one that is still running.
                    ProgramPaint::Exited(program) => {
                        render_notice(frame, inner, &program, "exited")
                    }
                },
            }
        }
    }
}

/// Paint an error panel for a plugin that failed, in that plugin's own rect.
///
/// The isolation rule made visible: the neighbours around this rect are painted
/// normally and keep their state.
pub fn render_error(frame: &mut Frame, area: Rect, title: &str, message: &str) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let block = Block::default()
        .borders(RatBorders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ratatui::style::Color::Red))
        .title(format!(" {title} "));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(message.trim().to_string())
            .style(Style::default().fg(ratatui::style::Color::Red))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

/// The emoji-presentation selector, the one codepoint that makes a cell's width
/// a matter of opinion.
const VARIATION_SELECTOR_16: char = '\u{FE0F}';

/// How many bytes [`VARIATION_SELECTOR_16`] occupies in UTF-8.
///
/// A symbol shorter than this cannot contain it, which rejects every ASCII cell
/// on an integer compare — and this walks the whole buffer on every painted
/// frame. Derived rather than written as `3` so the two cannot drift apart.
const VARIATION_SELECTOR_16_LEN: usize = VARIATION_SELECTOR_16.len_utf8();

/// Strip the emoji-presentation selector (`U+FE0F`) from every cell of a
/// painted frame.
///
/// A cell is one column or two, `unicode-width` calls an emoji-presentation
/// sequence two, and a terminal is free to disagree — Windows Terminal and
/// Ghostty draw many of them in one. Both answers damage the screen, in opposite
/// directions. Drawn two columns wide, the row receives the emoji *and* the
/// blank ratatui pairs with it, so it emits three columns for two cells and
/// wraps into the next row. Drawn one column wide, the trailing column is left
/// holding whatever was there before, because the diff only prints a cell whose
/// contents changed — which is the leftover glyphs a pane opening or closing
/// exposes, since the diff believes those columns already say what the frame
/// says.
///
/// Guessing which kind of terminal this is would settle neither, so the
/// ambiguity is removed instead: without the selector the base glyph is one
/// column everywhere, and the blank ratatui already reserved beside it keeps the
/// row the width it was measured at. Applied to the finished frame, so it covers
/// panes, bands, modals and the vt100 surfaces alike — an agent printing `✔️`
/// corrupts a row exactly as a plugin does.
///
/// The clusters it cannot help (a regional-indicator flag is two columns here
/// and a different number there) are why a reflow repaints in full — see
/// [`force_full_repaint`] and `App::last_placed`.
pub fn normalize_ambiguous_width(buf: &mut Buffer) {
    // The buffer's storage is its area exactly, row-major, so walking it
    // directly visits the same cells the coordinate loop did — minus a
    // bounds-checked `cell_mut` per cell, on a pass that touches ~10k of them
    // every painted frame.
    for cell in buf.content.iter_mut() {
        let symbol = cell.symbol();
        if symbol.len() < VARIATION_SELECTOR_16_LEN || !symbol.contains(VARIATION_SELECTOR_16) {
            continue;
        }
        let stripped: String = cell
            .symbol()
            .chars()
            .filter(|c| *c != VARIATION_SELECTOR_16)
            .collect();
        // A cell holding nothing but the selector would print nothing at
        // all, which is the leftover this exists to prevent.
        cell.set_symbol(if stripped.is_empty() { " " } else { &stripped });
    }
}

/// Mark every cell of a painted frame as one the diff must print, so the next
/// flush repaints the whole screen.
///
/// This is what a reflow owes (see `App::last_placed`): the cell diff is only
/// correct while ratatui's idea of a glyph's width matches the terminal's, and
/// where they cannot agree the pane that just closed leaves characters behind in
/// the column that replaced it.
///
/// Erasing the screen first would do the same job and is what `Terminal::clear`
/// is for, but it is the wrong instrument here, for three reasons. It flushes a
/// blank screen and the repaint arrives in the *next* flush, so every pane
/// toggle blinks. It asks the terminal where the cursor is first — a synchronous
/// round trip on the input stream, on a keypress. And it is unnecessary: what a
/// reflow actually needs is not an empty terminal but a print of every cell, and
/// [`CellDiffOption::AlwaysUpdate`] says exactly that without emitting anything
/// of its own. The screen goes straight from the old arrangement to the new one.
///
/// The marks travel into the buffer the next frame is diffed against, so the
/// frame after a reflow prints in full as well. That is one extra full write,
/// invisible and bounded by how often a layout moves — which is a keypress.
pub fn force_full_repaint(buf: &mut Buffer) {
    let area = buf.area;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
                cell.set_diff_option(CellDiffOption::AlwaysUpdate);
            }
        }
    }
}

fn build_block(spec: &NodeFrame) -> Block<'_> {
    let mut block = Block::default().style(spec.style);
    block = match spec.borders {
        Borders::All => block
            .borders(RatBorders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(spec.border_style),
        Borders::None => block.borders(RatBorders::NONE),
    };
    if let Some(title) = &spec.title {
        // A run carrying no colour of its own takes the border's. An unstyled title
        // took the TERMINAL's default foreground, which is somebody else's palette:
        // under the light themes that came out cream on cream, and panel titles —
        // the labels a newcomer navigates by — were the one thing on screen you
        // could not read. A title and the border it sits in are one piece of
        // chrome, so the default is shared and it is always a theme role. A run
        // that does set a colour keeps it, which is how the agent pane paints its
        // status word without the rest of the title changing.
        //
        // Spans borrow the runs' text (the reason `to_line` borrows), so the
        // patched style is the only thing built per frame.
        let spans: Vec<_> = title
            .iter()
            .map(|run| {
                let style = if run.style.fg.is_some() {
                    run.style
                } else {
                    spec.border_style.patch(run.style)
                };
                ratatui::text::Span::styled(run.text.as_str(), style)
            })
            .collect();
        block = block.title(Line::from(spans));
    }
    block
}

/// Shrink a rect by `padding` on every side, never past zero.
fn pad(area: Rect, padding: u16) -> Rect {
    if padding == 0 {
        return area;
    }
    let horizontal = padding.saturating_mul(2);
    Rect {
        x: area.x.saturating_add(padding),
        y: area.y.saturating_add(padding),
        width: area.width.saturating_sub(horizontal),
        height: area.height.saturating_sub(horizontal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::kernel::convert::to_node;
    use mlua::{Lua, Value};

    /// The emoji-presentation selector is what makes a cell's width a matter of
    /// opinion between ratatui and the terminal, and the leftover glyphs a pane
    /// toggle used to expose are that disagreement showing. Stripped, the glyph
    /// is one column everywhere.
    #[test]
    fn variation_selector_is_stripped_from_painted_cells() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 1));
        buffer.set_string(0, 0, "⚠️a", ratatui::style::Style::default());
        assert!(buffer[(0, 0)].symbol().contains('\u{FE0F}'));

        normalize_ambiguous_width(&mut buffer);

        assert_eq!(buffer[(0, 0)].symbol(), "⚠");
        // The blank ratatui reserved beside it stays, so nothing after the glyph
        // moves: the row is the width it was measured at.
        assert_eq!(buffer[(1, 0)].symbol(), " ");
        assert_eq!(buffer[(2, 0)].symbol(), "a");
    }

    /// The fast path must not eat multi-byte glyphs that merely *look* like
    /// candidates. `len < VARIATION_SELECTOR_16_LEN` rejects a cell before the
    /// substring search, so the boundary — a symbol as long as the selector, or
    /// longer, carrying no selector — is where a wrong comparison would corrupt
    /// the screen rather than merely slow it down.
    #[test]
    fn multi_byte_glyphs_without_the_selector_are_left_alone() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 8, 1));
        // 2 bytes (below the threshold), 3 bytes (exactly at it), 4 bytes (above).
        buffer[(0, 0)].set_symbol("é");
        buffer[(1, 0)].set_symbol("中");
        buffer[(2, 0)].set_symbol("𝄞");

        normalize_ambiguous_width(&mut buffer);

        assert_eq!(buffer[(0, 0)].symbol(), "é");
        assert_eq!(buffer[(1, 0)].symbol(), "中");
        assert_eq!(buffer[(2, 0)].symbol(), "𝄞");
    }

    /// The threshold is derived from the selector rather than written as `3`,
    /// so this pins the two together: a symbol shorter than the selector cannot
    /// contain it, and skipping it is what makes the common cell free.
    #[test]
    fn the_length_threshold_is_the_selectors_own_width() {
        assert_eq!(VARIATION_SELECTOR_16_LEN, VARIATION_SELECTOR_16.len_utf8());
        assert_eq!(VARIATION_SELECTOR_16_LEN, 3);
        assert!("a".len() < VARIATION_SELECTOR_16_LEN);
    }

    /// A cell holding nothing but the selector would print nothing at all, which
    /// is the leftover the pass exists to prevent.
    #[test]
    fn lone_variation_selector_becomes_a_blank() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 2, 1));
        buffer[(0, 0)].set_symbol("\u{FE0F}");

        normalize_ambiguous_width(&mut buffer);

        assert_eq!(buffer[(0, 0)].symbol(), " ");
    }

    /// A reflow repaints in full by marking the frame, not by erasing the
    /// screen: every cell of an otherwise-identical frame has to reach the
    /// terminal, and nothing may be emitted before it — an erase flushed on its
    /// own is a blank screen the user sees.
    #[test]
    fn a_forced_repaint_prints_every_cell_of_an_unchanged_frame() {
        let area = Rect::new(0, 0, 4, 2);
        let mut previous = Buffer::empty(area);
        previous.set_string(0, 0, "abcd", Style::default());
        let mut next = previous.clone();
        assert_eq!(previous.diff_iter(&next).count(), 0);

        force_full_repaint(&mut next);

        assert_eq!(
            previous.diff_iter(&next).count(),
            usize::from(area.width) * usize::from(area.height)
        );
    }

    /// Render a Lua-authored tree and return the screen as text lines.
    fn screen(source: &str, width: u16, height: u16) -> Vec<String> {
        let lua = Lua::new();
        let value: Value = lua
            .load(format!("return {source}"))
            .eval()
            .expect("source evaluates");
        let node = to_node(&value, "plugins/90_test.lua").expect("tree converts");

        let mut terminal =
            Terminal::new(TestBackend::new(width, height)).expect("test terminal builds");
        terminal
            .draw(|frame| render(frame, frame.area(), &node, &PlaceholderSurfaces))
            .expect("draw succeeds");

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

    #[test]
    fn text_paints_its_content() {
        let lines = screen("{ text = \"hello\" }", 10, 1);
        assert_eq!(lines[0], "hello");
    }

    #[test]
    fn a_vertical_box_stacks_children_in_order() {
        let lines = screen("{ children = { \"top\", \"bottom\" } }", 10, 2);
        assert_eq!(lines, vec!["top", "bottom"]);
    }

    #[test]
    fn a_horizontal_box_places_children_side_by_side() {
        let lines = screen(
            "{ axis = \"horizontal\", children = { { text = \"L\", len = 5 }, { text = \"R\" } } }",
            10,
            1,
        );
        assert_eq!(lines[0], "L    R");
    }

    #[test]
    fn a_frame_draws_a_border_and_insets_its_child() {
        let lines = screen("{ text = \"x\", frame = \"T\" }", 6, 3);
        assert!(lines[0].contains('T'), "title should be in the top border");
        assert!(lines[1].contains('x'), "content sits inside the border");
        assert!(
            lines[1].starts_with('│'),
            "left border present: {:?}",
            lines[1]
        );
    }

    #[test]
    fn alignment_moves_text_within_its_rect() {
        let lines = screen("{ text = \"ab\", align = \"right\" }", 6, 1);
        assert_eq!(lines[0], "    ab");
    }

    #[test]
    fn a_surface_paints_its_own_cells() {
        let lines = screen(
            "{ type = \"surface\", cells = { \"line one\", \"line two\" } }",
            12,
            2,
        );
        assert_eq!(lines, vec!["line one", "line two"]);
    }

    #[test]
    fn a_session_surface_uses_the_provider() {
        let lines = screen("{ type = \"surface\", session = \"abcdef12-0000\" }", 30, 5);
        let joined = lines.join("\n");
        assert!(joined.contains("terminal surface"), "{joined}");
        assert!(joined.contains("abcdef12"), "{joined}");
    }

    #[test]
    fn a_zero_height_rect_paints_nothing_rather_than_panicking() {
        // Guards the responsive case where a slot is squeezed out entirely.
        let lines = screen("{ text = \"x\" }", 10, 0);
        assert!(lines.is_empty());
    }

    #[test]
    fn a_child_squeezed_to_nothing_does_not_panic() {
        let lines = screen(
            "{ axis = \"horizontal\", children = { { text = \"aaa\", len = 20 }, { text = \"b\", len = 20 } } }",
            8,
            1,
        );
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn an_error_panel_shows_the_message() {
        let mut terminal = Terminal::new(TestBackend::new(40, 5)).expect("terminal");
        terminal
            .draw(|frame| render_error(frame, frame.area(), "session_list", "boom"))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let text: String = (0..5)
            .map(|y| {
                (0..40)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect();
        assert!(text.contains("session_list"), "{text}");
        assert!(text.contains("boom"), "{text}");
    }

    #[test]
    fn padding_never_underflows_a_small_rect() {
        assert_eq!(pad(Rect::new(0, 0, 1, 1), 4).width, 0);
    }
}
