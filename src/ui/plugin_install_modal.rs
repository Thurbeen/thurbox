use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::theme::Theme;
use super::{centered_fixed_height_rect, render_modal_frame, render_text_field};

/// Status line shown at the bottom of the install modal. Drives the colour
/// (idle = muted, in-progress = accent, success = idle status, error = danger).
#[derive(Debug, Clone)]
pub enum PluginInstallStatusView<'a> {
    Idle,
    InProgress(&'a str),
    Success(&'a str),
    Error(&'a str),
}

pub struct PluginInstallModalState<'a> {
    pub input: &'a str,
    pub cursor: usize,
    pub status: PluginInstallStatusView<'a>,
}

pub fn render_plugin_install_modal(frame: &mut Frame, state: &PluginInstallModalState<'_>) {
    // input(3) + status(1) + footer(1) = 5 rows + outer border(2)
    let area = centered_fixed_height_rect(60, 7, frame.area());
    let inner = render_modal_frame(frame, area, "Install Plugin");

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(inner);

    render_text_field(
        frame,
        chunks[0],
        "Source (path or git URL)",
        state.input,
        state.cursor,
        true,
    );

    let (label, color) = match state.status {
        PluginInstallStatusView::Idle => (
            "Paste a local path or a git URL (https://, git@…, ssh://).".to_string(),
            Theme::text_muted(),
        ),
        PluginInstallStatusView::InProgress(msg) => (msg.to_string(), Theme::accent()),
        PluginInstallStatusView::Success(msg) => (msg.to_string(), Theme::status_busy()),
        PluginInstallStatusView::Error(msg) => (msg.to_string(), Theme::danger()),
    };
    let status = Paragraph::new(Line::from(Span::styled(label, Style::default().fg(color))));
    frame.render_widget(status, chunks[1]);

    let footer = Line::from(vec![
        Span::styled("Enter", Theme::keybind()),
        Span::styled(" install  ", Theme::keybind_desc()),
        Span::styled("Esc", Theme::keybind()),
        Span::styled(" close", Theme::keybind_desc()),
    ]);
    frame.render_widget(Paragraph::new(footer), chunks[2]);
}
