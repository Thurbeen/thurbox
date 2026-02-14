use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::project::ProjectConfig;
use crate::session::SessionInfo;

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
                .fg(super::status_color(info.status))
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("Role: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            &info.role,
            Style::default()
                .fg(Color::Magenta)
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
            Span::styled("Sync:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} {}", info.sync_status.icon(), info.sync_status),
                Style::default()
                    .fg(super::sync_status_color(info.sync_status))
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        if let Some(last) = info.last_sync {
            let ago = format_elapsed(last.elapsed());
            lines.push(Line::from(Span::styled(
                format!("        (last sync {ago})"),
                Style::default().fg(Color::DarkGray),
            )));
        }
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

fn format_elapsed(duration: std::time::Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{}h ago", secs / 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_elapsed_just_now() {
        assert_eq!(
            format_elapsed(std::time::Duration::from_secs(0)),
            "just now"
        );
        assert_eq!(
            format_elapsed(std::time::Duration::from_secs(59)),
            "just now"
        );
    }

    #[test]
    fn format_elapsed_minutes() {
        assert_eq!(format_elapsed(std::time::Duration::from_secs(60)), "1m ago");
        assert_eq!(
            format_elapsed(std::time::Duration::from_secs(300)),
            "5m ago"
        );
        assert_eq!(
            format_elapsed(std::time::Duration::from_secs(3599)),
            "59m ago"
        );
    }

    #[test]
    fn format_elapsed_hours() {
        assert_eq!(
            format_elapsed(std::time::Duration::from_secs(3600)),
            "1h ago"
        );
        assert_eq!(
            format_elapsed(std::time::Duration::from_secs(7200)),
            "2h ago"
        );
    }
}
