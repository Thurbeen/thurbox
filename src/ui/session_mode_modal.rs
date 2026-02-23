use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

use super::centered_fixed_height_rect;
use super::theme::Theme;

const MODES_BASE: [&str; 2] = ["Normal", "Worktree"];
const MODE_SANDBOX_VM: &str = "Sandbox VM";

pub struct SessionModeState {
    pub selected_index: usize,
    /// Whether the "Sandbox VM" option should be shown.
    pub vm_available: bool,
}

impl SessionModeState {
    /// Number of visible modes.
    pub fn mode_count(&self) -> usize {
        if self.vm_available {
            MODES_BASE.len() + 1
        } else {
            MODES_BASE.len()
        }
    }
}

pub fn render_session_mode_modal(frame: &mut Frame, state: &SessionModeState) {
    let mode_count = state.mode_count();
    // Height = modes + 2 (border) + 1 (footer)
    let height = mode_count as u16 + 3;
    let area = centered_fixed_height_rect(50, height, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Session Mode ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::ACCENT));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // Mode list
            Constraint::Length(1), // Footer
        ])
        .split(inner);

    let modes: Vec<&str> = if state.vm_available {
        MODES_BASE
            .iter()
            .copied()
            .chain(std::iter::once(MODE_SANDBOX_VM))
            .collect()
    } else {
        MODES_BASE.to_vec()
    };

    let items: Vec<ListItem<'_>> = modes
        .iter()
        .enumerate()
        .map(|(i, mode)| {
            let style = if i == state.selected_index {
                Theme::selected_item()
            } else {
                Theme::normal_item()
            };
            let prefix = if i == state.selected_index {
                "▸ "
            } else {
                "  "
            };
            ListItem::new(Line::from(Span::styled(format!("{prefix}{mode}"), style)))
        })
        .collect();

    let list = List::new(items);
    frame.render_widget(list, chunks[0]);

    let footer = Line::from(vec![
        Span::styled("j/k", Theme::keybind()),
        Span::styled(" navigate  ", Theme::keybind_desc()),
        Span::styled("Enter", Theme::keybind()),
        Span::styled(" select  ", Theme::keybind_desc()),
        Span::styled("Esc", Theme::keybind()),
        Span::styled(" cancel", Theme::keybind_desc()),
    ]);
    frame.render_widget(Paragraph::new(footer), chunks[1]);
}
