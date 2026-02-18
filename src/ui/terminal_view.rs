use ratatui::{
    layout::{Margin, Position, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame,
};
use tui_term::widget::{Cursor, PseudoTerminal};

use super::focused_block;
use crate::session::SessionInfo;

pub fn render_terminal(
    frame: &mut Frame,
    area: Rect,
    parser: &mut vt100::Parser,
    info: &SessionInfo,
    focused: bool,
) {
    let scroll_offset = parser.screen().scrollback();

    // Compute total scrollback by temporarily setting to max and reading back
    let total_scrollback = {
        parser.screen_mut().set_scrollback(usize::MAX);
        let max = parser.screen().scrollback();
        parser.screen_mut().set_scrollback(scroll_offset);
        max
    };

    let title = {
        let base = if let Some(wt) = info.worktrees.first() {
            format!(
                " {} ({}) [{}] [{}] ",
                info.name, info.role, wt.branch, info.status
            )
        } else {
            format!(" {} ({}) [{}] ", info.name, info.role, info.status)
        };
        if scroll_offset > 0 {
            // Insert scroll indicator before the trailing space
            let trimmed = base.trim_end();
            format!("{trimmed} [{scroll_offset}\u{2191}] ")
        } else {
            base
        }
    };

    let block = focused_block(&title, focused);

    let mut pseudo_term = PseudoTerminal::new(parser.screen())
        .block(block)
        .style(Style::default().fg(Color::White).bg(Color::Reset));

    // Hide cursor when scrolled up
    if scroll_offset > 0 {
        let mut cursor = Cursor::default();
        cursor.hide();
        pseudo_term = pseudo_term.cursor(cursor);
    }

    frame.render_widget(pseudo_term, area);

    highlight_urls(frame, area, parser.screen());

    // Render scrollbar when there's scrollback content
    if total_scrollback > 0 {
        // Position scrollbar inside the block border
        let scrollbar_area = area.inner(Margin {
            vertical: 1,
            horizontal: 0,
        });

        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_style(Style::default().fg(Color::Cyan))
            .track_style(Style::default().fg(Color::DarkGray));

        // Invert: offset 0 (bottom) → position at max, offset max (top) → position at 0
        let position = total_scrollback.saturating_sub(scroll_offset);
        let (rows, _) = parser.screen().size();
        let mut scrollbar_state = ScrollbarState::new(total_scrollback)
            .position(position)
            .viewport_content_length(rows as usize);

        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }
}

/// Post-process the frame buffer to underline and colorize detected URLs.
fn highlight_urls(frame: &mut Frame, area: Rect, screen: &vt100::Screen) {
    let screen_rows = super::links::extract_screen_rows(screen);
    let links = super::links::detect_urls(&screen_rows);
    if links.is_empty() {
        return;
    }

    let inner = Block::default().borders(Borders::ALL).inner(area);
    let link_style = Style::default()
        .fg(Color::Blue)
        .add_modifier(Modifier::UNDERLINED);
    let buf = frame.buffer_mut();

    for link in &links {
        if link.row >= inner.height as usize {
            continue;
        }
        for col in link.start_col..link.end_col {
            if col >= inner.width as usize {
                break;
            }
            let pos = Position::new(inner.x + col as u16, inner.y + link.row as u16);
            if let Some(cell) = buf.cell_mut(pos) {
                cell.set_style(link_style);
            }
        }
    }
}

pub fn render_empty_terminal(frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" No Session ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let text = Paragraph::new("Press Ctrl+N to create a new session")
        .block(block)
        .style(Style::default().fg(Color::DarkGray));

    frame.render_widget(text, area);
}
