use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use super::theme::Theme;
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
        .border_style(Style::default().fg(Theme::BORDER_UNFOCUSED));

    let mut lines = Vec::new();

    // ── Project section ──
    if let Some(proj) = project {
        let project_line = vec![
            Span::styled("Project: ", Theme::label()),
            Span::styled(
                &proj.config.name,
                Style::default()
                    .fg(Theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        lines.push(Line::from(project_line));

        if proj.config.repos.len() == 1 {
            lines.push(Line::from(vec![
                Span::styled("Repo: ", Theme::label()),
                Span::styled(
                    proj.config.repos[0].display().to_string(),
                    Style::default().fg(Theme::TEXT_MUTED),
                ),
            ]));
        } else {
            lines.push(Line::from(Span::styled("Repos:", Theme::label())));
            for repo in &proj.config.repos {
                lines.push(Line::from(Span::styled(
                    format!("  {}", repo.display()),
                    Style::default().fg(Theme::TEXT_MUTED),
                )));
            }
        }

        lines.push(Line::from(vec![
            Span::styled("Sessions: ", Theme::label()),
            Span::styled(
                proj.session_ids.len().to_string(),
                Style::default().fg(Theme::TEXT_PRIMARY),
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
            Span::styled("Roles: ", Theme::label()),
            Span::styled(roles_text, Style::default().fg(Theme::TEXT_PRIMARY)),
        ]));

        lines.push(separator());
    }

    // ── Session section ──
    lines.push(Line::from(vec![
        Span::styled("Name: ", Theme::label()),
        Span::styled(&info.name, Style::default().fg(Theme::TEXT_PRIMARY)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Status: ", Theme::label()),
        Span::styled(
            format!("{} {}", info.status.icon(), info.status),
            Style::default()
                .fg(super::status_color(info.status))
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Role: ", Theme::label()),
        Span::styled(
            &info.role,
            Style::default()
                .fg(Theme::ROLE_NAME)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("ID: ", Theme::label()),
        Span::styled(info.id.to_string(), Style::default().fg(Theme::TEXT_MUTED)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Claude: ", Theme::label()),
        Span::styled(
            info.claude_session_id.as_deref().unwrap_or("(none)"),
            Style::default().fg(Theme::TEXT_MUTED),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Backend: ", Theme::label()),
        Span::styled(
            info.backend_id.as_deref().unwrap_or("(none)"),
            Style::default().fg(Theme::TEXT_MUTED),
        ),
    ]));

    // ── Directories section ──
    if info.cwd.is_some() || !info.additional_dirs.is_empty() {
        lines.push(separator());
        lines.push(Line::from(Span::styled(
            "Directories",
            Theme::section_header(),
        )));
        if let Some(cwd) = &info.cwd {
            lines.push(Line::from(Span::styled(
                format!("  {} (cwd)", cwd.display()),
                Style::default().fg(Theme::TEXT_MUTED),
            )));
        }
        for dir in &info.additional_dirs {
            lines.push(Line::from(Span::styled(
                format!("  {}", dir.display()),
                Style::default().fg(Theme::TEXT_MUTED),
            )));
        }
    }

    // ── Worktrees section ──
    if !info.worktrees.is_empty() {
        lines.push(separator());
        let header = if info.worktrees.len() == 1 {
            "Worktree"
        } else {
            "Worktrees"
        };
        lines.push(Line::from(Span::styled(header, Theme::section_header())));
        for wt in &info.worktrees {
            lines.push(Line::from(vec![
                Span::styled("Branch: ", Theme::label()),
                Span::styled(&wt.branch, Style::default().fg(Theme::BRANCH_NAME)),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Path: ", Theme::label()),
                Span::styled(
                    wt.worktree_path.display().to_string(),
                    Style::default().fg(Theme::TEXT_MUTED),
                ),
            ]));
        }
    }

    // ── Role Details section ──
    if let Some(role_config) = project.and_then(|p| find_role(&p.config.roles, &info.role)) {
        lines.push(separator());
        lines.push(Line::from(Span::styled(
            "Role Details",
            Theme::section_header(),
        )));

        if !role_config.description.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("Desc: ", Theme::label()),
                Span::styled(
                    &role_config.description,
                    Style::default().fg(Theme::TEXT_PRIMARY),
                ),
            ]));
        }

        if let Some(mode) = &role_config.permissions.permission_mode {
            lines.push(Line::from(vec![
                Span::styled("Mode: ", Theme::label()),
                Span::styled(mode, Style::default().fg(Theme::KEYBIND_HINT)),
            ]));
        }

        if !role_config.permissions.allowed_tools.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("Allowed: ", Theme::label()),
                Span::styled(
                    role_config.permissions.allowed_tools.join(", "),
                    Style::default().fg(Theme::TOOL_ALLOWED),
                ),
            ]));
        }

        if !role_config.permissions.disallowed_tools.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("Disallowed: ", Theme::label()),
                Span::styled(
                    role_config.permissions.disallowed_tools.join(", "),
                    Style::default().fg(Theme::TOOL_DISALLOWED),
                ),
            ]));
        }
    }

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn separator<'a>() -> Line<'a> {
    Line::from(Span::styled(
        "──────────────────────",
        Style::default().fg(Theme::BORDER_UNFOCUSED),
    ))
}

fn find_role<'a>(roles: &'a [RoleConfig], name: &str) -> Option<&'a RoleConfig> {
    roles.iter().find(|r| r.name == name)
}
