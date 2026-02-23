use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use super::theme::Theme;
use crate::project::ProjectInfo;
use crate::session::{RoleConfig, SessionInfo, SessionStatus};

/// Rich VM details for the info panel, sourced from the database VM record.
pub struct VmDetails {
    pub state: String,
    pub cpus: u32,
    pub memory_mb: u32,
    pub ssh_port: u16,
    pub base_image: String,
}

/// Per-session CPU/memory metrics.
pub struct SessionMetrics {
    pub name: String,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
}

/// System-wide and per-session resource metrics.
pub struct SystemMetrics {
    /// Overall CPU usage 0-100.
    pub cpu_percent: f32,
    /// Total RAM used in bytes.
    pub memory_used: u64,
    /// Total RAM in bytes.
    pub memory_total: u64,
    /// Per-session resource usage.
    pub per_session: Vec<SessionMetrics>,
}

pub fn render_info_panel(
    frame: &mut Frame,
    area: Rect,
    info: &SessionInfo,
    project: Option<&ProjectInfo>,
    vm_details: Option<&VmDetails>,
    metrics: Option<&SystemMetrics>,
) {
    let block = Block::default()
        .title(" Info ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::BORDER_UNFOCUSED));

    let mut lines = Vec::new();

    // ── System metrics section ──
    if let Some(m) = metrics {
        lines.push(Line::from(Span::styled(
            "System Resources",
            Theme::section_header(),
        )));
        lines.push(render_gauge("CPU", m.cpu_percent, None));
        lines.push(render_gauge(
            "RAM",
            if m.memory_total > 0 {
                (m.memory_used as f64 / m.memory_total as f64 * 100.0) as f32
            } else {
                0.0
            },
            Some(format_bytes_pair(m.memory_used, m.memory_total)),
        ));

        if !m.per_session.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Session Resources",
                Theme::section_header(),
            )));
            for sm in &m.per_session {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {:<14}", truncate_name(&sm.name, 14)),
                        Style::default().fg(Theme::TEXT_PRIMARY),
                    ),
                    Span::styled(
                        format!("{:>3}%", sm.cpu_percent as u32),
                        Style::default().fg(Theme::ACCENT),
                    ),
                    Span::styled(" CPU  ", Style::default().fg(Theme::TEXT_MUTED)),
                    Span::styled(
                        format_bytes(sm.memory_bytes),
                        Style::default().fg(Theme::ACCENT),
                    ),
                ]));
            }
        }

        lines.push(separator());
    }

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
            info.agent_session_id.as_deref().unwrap_or("(none)"),
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

    // ── VM section (for sandboxed sessions) ──
    if let Some(vm_id) = &info.vm_id {
        lines.push(separator());
        lines.push(Line::from(Span::styled(
            "Sandbox VM",
            Theme::section_header(),
        )));

        if let Some(vm) = vm_details {
            lines.push(Line::from(vec![
                Span::styled("State: ", Theme::label()),
                Span::styled(&vm.state, Style::default().fg(Theme::TEXT_PRIMARY)),
            ]));
        }

        // Show short VM ID (first 8 chars)
        let short_id = if vm_id.len() > 8 { &vm_id[..8] } else { vm_id };
        lines.push(Line::from(vec![
            Span::styled("VM ID: ", Theme::label()),
            Span::styled(short_id, Style::default().fg(Theme::TEXT_MUTED)),
        ]));

        if let Some(vm) = vm_details {
            lines.push(Line::from(vec![
                Span::styled("CPUs: ", Theme::label()),
                Span::styled(
                    vm.cpus.to_string(),
                    Style::default().fg(Theme::TEXT_PRIMARY),
                ),
                Span::styled("  RAM: ", Theme::label()),
                Span::styled(
                    format!("{} MB", vm.memory_mb),
                    Style::default().fg(Theme::TEXT_PRIMARY),
                ),
            ]));
            if vm.ssh_port > 0 {
                lines.push(Line::from(vec![
                    Span::styled("SSH: ", Theme::label()),
                    Span::styled(
                        format!("localhost:{}", vm.ssh_port),
                        Style::default().fg(Theme::TEXT_PRIMARY),
                    ),
                ]));
            }
            lines.push(Line::from(vec![
                Span::styled("Image: ", Theme::label()),
                Span::styled(&vm.base_image, Style::default().fg(Theme::TEXT_MUTED)),
            ]));
        }

        // Show provisioning step when provisioning.
        if info.status == SessionStatus::Provisioning {
            if let Some(ref step) = info.provisioning_step {
                lines.push(Line::from(vec![
                    Span::styled("Step: ", Theme::label()),
                    Span::styled(step, Style::default().fg(Theme::ACCENT)),
                ]));
            }
        }
    }

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

/// Render a gauge bar like: `CPU [████████░░░░░░░░░░░░] 42%`
fn render_gauge<'a>(label: &'a str, percent: f32, suffix: Option<String>) -> Line<'a> {
    let bar_width = 20;
    let clamped = percent.clamp(0.0, 100.0);
    let filled = ((clamped / 100.0) * bar_width as f32).round() as usize;
    let empty = bar_width - filled;

    let filled_str: String = "█".repeat(filled);
    let empty_str: String = "░".repeat(empty);

    let right_text = match suffix {
        Some(s) => s,
        None => format!("{clamped:.0}%"),
    };

    Line::from(vec![
        Span::styled(format!("{label} ["), Style::default().fg(Theme::TEXT_MUTED)),
        Span::styled(filled_str, Style::default().fg(Theme::ACCENT)),
        Span::styled(empty_str, Style::default().fg(Theme::TEXT_MUTED)),
        Span::styled("] ", Style::default().fg(Theme::TEXT_MUTED)),
        Span::styled(right_text, Style::default().fg(Theme::TEXT_PRIMARY)),
    ])
}

/// Format a used/total byte pair like "8.2/16.0 GB".
fn format_bytes_pair(used: u64, total: u64) -> String {
    let (total_val, unit) = human_bytes(total);
    let used_val = used as f64 / unit_divisor(unit);
    format!("{used_val:.1}/{total_val:.1} {unit}")
}

/// Format bytes as a human-readable string like "1.2 GB".
fn format_bytes(bytes: u64) -> String {
    let (val, unit) = human_bytes(bytes);
    format!("{val:.1} {unit}")
}

fn human_bytes(bytes: u64) -> (f64, &'static str) {
    const GB: u64 = 1_073_741_824;
    const MB: u64 = 1_048_576;
    const KB: u64 = 1_024;

    if bytes >= GB {
        (bytes as f64 / GB as f64, "GB")
    } else if bytes >= MB {
        (bytes as f64 / MB as f64, "MB")
    } else {
        (bytes as f64 / KB as f64, "KB")
    }
}

fn unit_divisor(unit: &str) -> f64 {
    match unit {
        "GB" => 1_073_741_824.0,
        "MB" => 1_048_576.0,
        _ => 1_024.0,
    }
}

/// Truncate a name to fit within `max_len` characters, appending "…" if truncated.
fn truncate_name(name: &str, max_len: usize) -> String {
    if name.len() <= max_len {
        name.to_string()
    } else {
        let mut s: String = name.chars().take(max_len - 1).collect();
        s.push('…');
        s
    }
}
