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

/// Width of the gauge bar in characters.
const GAUGE_BAR_WIDTH: usize = 20;

/// Max display width for session names in the per-session metrics table.
const SESSION_NAME_WIDTH: usize = 14;

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
                        format!(
                            "  {:<width$}",
                            truncate_name(&sm.name, SESSION_NAME_WIDTH),
                            width = SESSION_NAME_WIDTH
                        ),
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
    let clamped = percent.clamp(0.0, 100.0);
    let filled = ((clamped / 100.0) * GAUGE_BAR_WIDTH as f32).round() as usize;
    let empty = GAUGE_BAR_WIDTH - filled;

    let right_text = suffix.unwrap_or_else(|| format!("{clamped:.0}%"));

    Line::from(vec![
        Span::styled(format!("{label} ["), Style::default().fg(Theme::TEXT_MUTED)),
        Span::styled("█".repeat(filled), Style::default().fg(Theme::ACCENT)),
        Span::styled("░".repeat(empty), Style::default().fg(Theme::TEXT_MUTED)),
        Span::styled("] ", Style::default().fg(Theme::TEXT_MUTED)),
        Span::styled(right_text, Style::default().fg(Theme::TEXT_PRIMARY)),
    ])
}

/// Format a used/total byte pair like "8.2/16.0 GB".
fn format_bytes_pair(used: u64, total: u64) -> String {
    let (total_val, divisor, unit) = human_bytes(total);
    let used_val = used as f64 / divisor;
    format!("{used_val:.1}/{total_val:.1} {unit}")
}

/// Format bytes as a human-readable string like "1.2 GB".
fn format_bytes(bytes: u64) -> String {
    let (val, _, unit) = human_bytes(bytes);
    format!("{val:.1} {unit}")
}

/// Convert bytes to a human-readable (value, divisor, unit) tuple.
fn human_bytes(bytes: u64) -> (f64, f64, &'static str) {
    const GB: f64 = 1_073_741_824.0;
    const MB: f64 = 1_048_576.0;
    const KB: f64 = 1_024.0;

    let b = bytes as f64;
    if b >= GB {
        (b / GB, GB, "GB")
    } else if b >= MB {
        (b / MB, MB, "MB")
    } else {
        (b / KB, KB, "KB")
    }
}

/// Truncate a name to fit within `max_len` characters, appending "…" if truncated.
fn truncate_name(name: &str, max_len: usize) -> String {
    let char_count = name.chars().count();
    if char_count <= max_len {
        name.to_string()
    } else {
        let mut s: String = name.chars().take(max_len - 1).collect();
        s.push('…');
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── human_bytes tests ──

    #[test]
    fn human_bytes_gigabytes() {
        let (val, _, unit) = human_bytes(2_147_483_648); // 2 GB
        assert!((val - 2.0).abs() < 0.01);
        assert_eq!(unit, "GB");
    }

    #[test]
    fn human_bytes_megabytes() {
        let (val, _, unit) = human_bytes(104_857_600); // 100 MB
        assert!((val - 100.0).abs() < 0.01);
        assert_eq!(unit, "MB");
    }

    #[test]
    fn human_bytes_kilobytes() {
        let (val, _, unit) = human_bytes(512_000); // ~500 KB
        assert_eq!(unit, "KB");
        assert!(val > 400.0 && val < 600.0);
    }

    #[test]
    fn human_bytes_zero() {
        let (val, _, unit) = human_bytes(0);
        assert!((val - 0.0).abs() < 0.01);
        assert_eq!(unit, "KB");
    }

    #[test]
    fn human_bytes_divisor_matches_unit() {
        let bytes = 3_221_225_472u64; // 3 GB
        let (val, divisor, unit) = human_bytes(bytes);
        assert_eq!(unit, "GB");
        assert!((bytes as f64 / divisor - val).abs() < 0.001);
    }

    // ── format_bytes tests ──

    #[test]
    fn format_bytes_gb() {
        let s = format_bytes(1_073_741_824);
        assert_eq!(s, "1.0 GB");
    }

    #[test]
    fn format_bytes_mb() {
        let s = format_bytes(524_288_000);
        assert_eq!(s, "500.0 MB");
    }

    // ── format_bytes_pair tests ──

    #[test]
    fn format_bytes_pair_same_unit() {
        let s = format_bytes_pair(8_589_934_592, 17_179_869_184); // 8/16 GB
        assert_eq!(s, "8.0/16.0 GB");
    }

    #[test]
    fn format_bytes_pair_zero_used() {
        let s = format_bytes_pair(0, 17_179_869_184);
        assert_eq!(s, "0.0/16.0 GB");
    }

    // ── truncate_name tests ──

    #[test]
    fn truncate_name_short() {
        assert_eq!(truncate_name("hello", 10), "hello");
    }

    #[test]
    fn truncate_name_exact() {
        assert_eq!(truncate_name("hello", 5), "hello");
    }

    #[test]
    fn truncate_name_long() {
        let result = truncate_name("very-long-session-name", 10);
        assert_eq!(result, "very-long…");
        assert_eq!(result.chars().count(), 10);
    }

    #[test]
    fn truncate_name_multibyte() {
        let result = truncate_name("café-session", 5);
        assert_eq!(result.chars().count(), 5);
        assert!(result.ends_with('…'));
    }

    // ── render_gauge tests ──

    #[test]
    fn render_gauge_zero_percent() {
        let line = render_gauge("CPU", 0.0, None);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("CPU ["));
        assert!(text.contains("0%"));
        // All empty chars
        assert!(text.contains(&"░".repeat(GAUGE_BAR_WIDTH)));
    }

    #[test]
    fn render_gauge_hundred_percent() {
        let line = render_gauge("CPU", 100.0, None);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("100%"));
        assert!(text.contains(&"█".repeat(GAUGE_BAR_WIDTH)));
    }

    #[test]
    fn render_gauge_fifty_percent() {
        let line = render_gauge("RAM", 50.0, None);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("50%"));
    }

    #[test]
    fn render_gauge_with_suffix() {
        let line = render_gauge("RAM", 50.0, Some("8.0/16.0 GB".to_string()));
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("8.0/16.0 GB"));
        assert!(!text.contains("50%"));
    }

    #[test]
    fn render_gauge_clamps_above_100() {
        let line = render_gauge("CPU", 150.0, None);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("100%"));
    }

    #[test]
    fn render_gauge_clamps_below_zero() {
        let line = render_gauge("CPU", -10.0, None);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("0%"));
    }

    // ── constants compile-time checks ──
    const _: () = assert!(GAUGE_BAR_WIDTH >= 10);
    const _: () = assert!(GAUGE_BAR_WIDTH <= 50);
    const _: () = assert!(SESSION_NAME_WIDTH >= 8);
    const _: () = assert!(SESSION_NAME_WIDTH <= 30);
}
