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
