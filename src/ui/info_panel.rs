use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::project::ProjectConfig;
use crate::session::{SessionInfo, SessionStatus};

pub fn render_info_panel(
    frame: &mut Frame,
    area: Rect,
    info: &SessionInfo,
    project: Option<&ProjectConfig>,
) {
    let block = Block::default()
        .title(" Info ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Gray));

    let mut lines = Vec::new();

    // Project info (if available)
    if let Some(proj) = project {
        lines.push(Line::from(vec![
            Span::styled("Project: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                &proj.name,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        if proj.repos.len() == 1 {
            lines.push(Line::from(vec![
                Span::styled("Repo: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    proj.repos[0].display().to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        } else {
            lines.push(Line::from(Span::styled(
                "Repos:",
                Style::default().fg(Color::DarkGray),
            )));
            for repo in &proj.repos {
                lines.push(Line::from(Span::styled(
                    format!("  {}", repo.display()),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
        lines.push(Line::from(""));
    }

    // Session info
    lines.push(Line::from(vec![
        Span::styled("Name: ", Style::default().fg(Color::DarkGray)),
        Span::styled(&info.name, Style::default().fg(Color::White)),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("Status: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{} {}", info.status.icon(), info.status),
            Style::default()
                .fg(status_color(&info.status))
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("ID: ", Style::default().fg(Color::DarkGray)),
        Span::styled(info.id.to_string(), Style::default().fg(Color::DarkGray)),
    ]));

    if let Some(wt) = &info.worktree {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Worktree",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(vec![
            Span::styled("Branch: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&wt.branch, Style::default().fg(Color::Green)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Path: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                wt.worktree_path.display().to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn status_color(status: &SessionStatus) -> Color {
    match status {
        SessionStatus::Running => Color::Green,
        SessionStatus::Idle => Color::Yellow,
        SessionStatus::Error => Color::Red,
    }
}
