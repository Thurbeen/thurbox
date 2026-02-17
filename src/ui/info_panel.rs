use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::project::ProjectInfo;
use crate::session::{RoleConfig, SessionInfo};

pub fn render_info_panel(
    frame: &mut Frame,
    area: Rect,
    info: &SessionInfo,
    project: Option<&ProjectInfo>,
) {
    let block = Block::default()
        .title(" Info ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Gray));

    let mut lines = Vec::new();

    // ── Project section ──
    if let Some(proj) = project {
        let mut project_line = vec![
            Span::styled("Project: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                &proj.config.name,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if proj.is_default {
            project_line.push(Span::raw(" "));
            project_line.push(Span::styled(
                "[Default]",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::from(project_line));

        if proj.config.repos.len() == 1 {
            lines.push(Line::from(vec![
                Span::styled("Repo: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    proj.config.repos[0].display().to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        } else {
            lines.push(Line::from(Span::styled(
                "Repos:",
                Style::default().fg(Color::DarkGray),
            )));
            for repo in &proj.config.repos {
                lines.push(Line::from(Span::styled(
                    format!("  {}", repo.display()),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }

        lines.push(Line::from(vec![
            Span::styled("Sessions: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                proj.session_ids.len().to_string(),
                Style::default().fg(Color::White),
            ),
        ]));

        let roles_text = if proj.config.roles.is_empty() {
            "(none)".to_string()
        } else {
            proj.config
                .roles
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        lines.push(Line::from(vec![
            Span::styled("Roles: ", Style::default().fg(Color::DarkGray)),
            Span::styled(roles_text, Style::default().fg(Color::White)),
        ]));

        lines.push(Line::from(""));
    }

    // ── Session section ──
    lines.push(Line::from(vec![
        Span::styled("Name: ", Style::default().fg(Color::DarkGray)),
        Span::styled(&info.name, Style::default().fg(Color::White)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Status: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{} {}", info.status.icon(), info.status),
            Style::default()
                .fg(super::status_color(info.status))
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Role: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            &info.role,
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("ID: ", Style::default().fg(Color::DarkGray)),
        Span::styled(info.id.to_string(), Style::default().fg(Color::DarkGray)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Claude: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            info.claude_session_id.as_deref().unwrap_or("(none)"),
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Backend: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            info.backend_id.as_deref().unwrap_or("(none)"),
            Style::default().fg(Color::DarkGray),
        ),
    ]));

    // ── Directories section ──
    if info.cwd.is_some() || !info.additional_dirs.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Directories",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        if let Some(cwd) = &info.cwd {
            lines.push(Line::from(Span::styled(
                format!("  {} (cwd)", cwd.display()),
                Style::default().fg(Color::DarkGray),
            )));
        }
        for dir in &info.additional_dirs {
            lines.push(Line::from(Span::styled(
                format!("  {}", dir.display()),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    // ── Worktree section ──
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

    // ── Role Details section ──
    if let Some(role_config) = project.and_then(|p| find_role(&p.config.roles, &info.role)) {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Role Details",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));

        if !role_config.description.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("Desc: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&role_config.description, Style::default().fg(Color::White)),
            ]));
        }

        if let Some(mode) = &role_config.permissions.permission_mode {
            lines.push(Line::from(vec![
                Span::styled("Mode: ", Style::default().fg(Color::DarkGray)),
                Span::styled(mode, Style::default().fg(Color::Yellow)),
            ]));
        }

        if !role_config.permissions.allowed_tools.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("Allowed: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    role_config.permissions.allowed_tools.join(", "),
                    Style::default().fg(Color::Green),
                ),
            ]));
        }

        if !role_config.permissions.disallowed_tools.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("Disallowed: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    role_config.permissions.disallowed_tools.join(", "),
                    Style::default().fg(Color::Red),
                ),
            ]));
        }
    }

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn find_role<'a>(roles: &'a [RoleConfig], name: &str) -> Option<&'a RoleConfig> {
    roles.iter().find(|r| r.name == name)
}
