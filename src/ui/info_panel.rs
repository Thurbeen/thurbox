use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use std::path::Path;

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

/// Per-session CPU/memory metrics with context.
pub struct SessionMetrics {
    pub name: String,
    pub project_name: String,
    pub worktree_branch: Option<String>,
    pub is_vm: bool,
    pub is_container: bool,
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
    /// Per-session resource usage (all projects).
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

    let inner_width = area.width.saturating_sub(2) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;

    let mut lines = Vec::new();

    // ── Session section (most relevant: "what am I looking at") ──
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

    // ── Project section ──
    if let Some(proj) = project {
        lines.push(separator(inner_width));

        lines.push(Line::from(vec![
            Span::styled("Project: ", Theme::label()),
            Span::styled(
                &proj.config.name,
                Style::default()
                    .fg(Theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));

        if proj.config.repos.len() == 1 {
            lines.push(Line::from(vec![
                Span::styled("Repo: ", Theme::label()),
                Span::styled(
                    short_path(&proj.config.repos[0]),
                    Style::default().fg(Theme::TEXT_MUTED),
                ),
            ]));
        } else {
            lines.push(Line::from(Span::styled("Repos:", Theme::label())));
            for repo in &proj.config.repos {
                lines.push(Line::from(Span::styled(
                    format!("  {}", short_path(repo)),
                    Style::default().fg(Theme::TEXT_MUTED),
                )));
            }
        }

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
    }

    // ── System Resources section (global CPU/RAM) ──
    if let Some(m) = metrics {
        lines.push(separator(inner_width));
        lines.push(Line::from(Span::styled("System", Theme::section_header())));

        let gauge_lines = render_gauge_lines("CPU", m.cpu_percent, None, inner_width);
        lines.extend(gauge_lines);

        let ram_lines = render_gauge_lines(
            "RAM",
            if m.memory_total > 0 {
                (m.memory_used as f64 / m.memory_total as f64 * 100.0) as f32
            } else {
                0.0
            },
            Some(format_bytes_pair(m.memory_used, m.memory_total)),
            inner_width,
        );
        lines.extend(ram_lines);
    }

    // ── Middle sections (VM, Directories, Worktrees, Role Details) ──
    append_detail_sections(&mut lines, info, project, vm_details, inner_width);

    // ── Per-session CPU/RAM (all projects, height-capped, always last) ──
    if let Some(m) = metrics {
        if !m.per_session.is_empty() {
            let total = m.per_session.len();
            let section_header_cost = 2; // separator + "Sessions"
            let used = lines.len() + section_header_cost;
            let remaining = inner_height.saturating_sub(used);
            let fits = remaining / 2; // 2 lines per session
            let show_count = if fits >= total {
                total
            } else {
                // Reserve 1 line for "+N more"
                inner_height
                    .saturating_sub(used + 1)
                    .checked_div(2)
                    .unwrap_or(0)
                    .max(1)
                    .min(total)
            };

            lines.push(separator(inner_width));
            lines.push(Line::from(Span::styled(
                "Sessions",
                Theme::section_header(),
            )));

            let name_width = inner_width.saturating_sub(2);
            for sm in m.per_session.iter().take(show_count) {
                lines.push(render_session_name_line(sm, name_width));
                lines.push(render_session_stats_line(sm));
            }

            if show_count < total {
                lines.push(Line::from(Span::styled(
                    format!("  +{} more", total - show_count),
                    Style::default().fg(Theme::TEXT_MUTED),
                )));
            }
        }
    }

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

/// Render a session's name line with context tags (project, branch, VM).
fn render_session_name_line<'a>(sm: &'a SessionMetrics, name_width: usize) -> Line<'a> {
    let mut spans: Vec<Span<'a>> = vec![Span::styled(
        format!("  {}", truncate_name(&sm.name, name_width)),
        Style::default().fg(Theme::TEXT_PRIMARY),
    )];

    let mut context = String::new();
    context.push_str(&sm.project_name);
    if let Some(ref branch) = sm.worktree_branch {
        context.push(' ');
        context.push_str(branch);
    }
    if sm.is_vm {
        context.push_str(" VM");
    } else if sm.is_container {
        context.push_str(" Container");
    }
    if !context.is_empty() {
        spans.push(Span::styled(
            format!(
                " [{}]",
                truncate_name(&context, name_width.saturating_sub(4))
            ),
            Style::default().fg(Theme::TEXT_MUTED),
        ));
    }

    Line::from(spans)
}

/// Render a session's CPU% and RAM stats line.
fn render_session_stats_line(sm: &SessionMetrics) -> Line<'static> {
    let ram = format_bytes(sm.memory_bytes);
    Line::from(vec![
        Span::styled("    ", Style::default()),
        Span::styled(
            format!("{:>3}%", sm.cpu_percent as u32),
            Style::default().fg(Theme::ACCENT),
        ),
        Span::styled(" CPU  ", Style::default().fg(Theme::TEXT_MUTED)),
        Span::styled(ram, Style::default().fg(Theme::ACCENT)),
        Span::styled(" RAM", Style::default().fg(Theme::TEXT_MUTED)),
    ])
}

/// Append detail sections (VM, Directories, Worktrees, Role Details) to `lines`.
fn append_detail_sections<'a>(
    lines: &mut Vec<Line<'a>>,
    info: &'a SessionInfo,
    project: Option<&'a ProjectInfo>,
    vm_details: Option<&'a VmDetails>,
    inner_width: usize,
) {
    // ── VM section (for sandboxed sessions) ──
    if info.vm_id.is_some() {
        lines.push(separator(inner_width));
        lines.push(Line::from(Span::styled("VM", Theme::section_header())));

        if let Some(vm) = vm_details {
            lines.push(Line::from(vec![
                Span::styled("State: ", Theme::label()),
                Span::styled(&vm.state, Style::default().fg(Theme::TEXT_PRIMARY)),
            ]));
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
        lines.push(separator(inner_width));
        lines.push(Line::from(Span::styled(
            "Directories",
            Theme::section_header(),
        )));
        if let Some(cwd) = &info.cwd {
            lines.push(Line::from(Span::styled(
                format!("  {} (cwd)", short_path(cwd)),
                Style::default().fg(Theme::TEXT_MUTED),
            )));
        }
        for dir in &info.additional_dirs {
            lines.push(Line::from(Span::styled(
                format!("  {}", short_path(dir)),
                Style::default().fg(Theme::TEXT_MUTED),
            )));
        }
    }

    // ── Worktree section ──
    if let Some(wt) = info.worktrees.first() {
        lines.push(separator(inner_width));
        lines.push(Line::from(vec![
            Span::styled("Worktree: ", Theme::label()),
            Span::styled(&wt.branch, Style::default().fg(Theme::BRANCH_NAME)),
        ]));
    }

    // ── Role Details section ──
    if let Some(role_config) = project.and_then(|p| find_role(&p.config.roles, &info.role)) {
        lines.push(separator(inner_width));
        lines.push(Line::from(Span::styled(
            "Role Details",
            Theme::section_header(),
        )));

        if let Some(mode) = &role_config.permissions.permission_mode {
            lines.push(Line::from(vec![
                Span::styled("Mode: ", Theme::label()),
                Span::styled(mode, Style::default().fg(Theme::KEYBIND_HINT)),
            ]));
        }

        if !role_config.permissions.allowed_tools.is_empty() {
            let label = "Allowed: ";
            let max = inner_width.saturating_sub(label.len());
            lines.push(Line::from(vec![
                Span::styled(label, Theme::label()),
                Span::styled(
                    truncate_name(&role_config.permissions.allowed_tools.join(", "), max),
                    Style::default().fg(Theme::TOOL_ALLOWED),
                ),
            ]));
        }

        if !role_config.permissions.disallowed_tools.is_empty() {
            let label = "Disallowed: ";
            let max = inner_width.saturating_sub(label.len());
            lines.push(Line::from(vec![
                Span::styled(label, Theme::label()),
                Span::styled(
                    truncate_name(&role_config.permissions.disallowed_tools.join(", "), max),
                    Style::default().fg(Theme::TOOL_DISALLOWED),
                ),
            ]));
        }
    }
}

fn separator(width: usize) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width),
        Style::default().fg(Theme::BORDER_UNFOCUSED),
    ))
}

fn find_role<'a>(roles: &'a [RoleConfig], name: &str) -> Option<&'a RoleConfig> {
    roles.iter().find(|r| r.name == name)
}

/// Render a gauge as two lines:
/// Line 1: label left-aligned, suffix/percent right-aligned
/// Line 2: full-width bar `[███░░░░]` scaled to `width - 2`
fn render_gauge_lines(
    label: &str,
    percent: f32,
    suffix: Option<String>,
    width: usize,
) -> Vec<Line<'static>> {
    let clamped = percent.clamp(0.0, 100.0);
    let right_text = suffix.unwrap_or_else(|| format!("{clamped:.0}%"));

    // Line 1: label + right-aligned suffix
    let label_len = label.chars().count();
    let right_len = right_text.chars().count();
    let padding = width.saturating_sub(label_len + right_len);
    let header_line = Line::from(vec![
        Span::styled(label.to_string(), Style::default().fg(Theme::TEXT_MUTED)),
        Span::raw(" ".repeat(padding)),
        Span::styled(right_text, Style::default().fg(Theme::TEXT_PRIMARY)),
    ]);

    // Line 2: bar scaled to width - 2 (for [ and ])
    let bar_width = width.saturating_sub(2);
    let filled = ((clamped / 100.0) * bar_width as f32).round() as usize;
    let empty = bar_width.saturating_sub(filled);
    let bar_line = Line::from(vec![
        Span::styled("[", Style::default().fg(Theme::TEXT_MUTED)),
        Span::styled("█".repeat(filled), Style::default().fg(Theme::ACCENT)),
        Span::styled("░".repeat(empty), Style::default().fg(Theme::TEXT_MUTED)),
        Span::styled("]", Style::default().fg(Theme::TEXT_MUTED)),
    ]);

    vec![header_line, bar_line]
}

/// Format bytes as a human-readable string like "1.2 GB".
fn format_bytes(bytes: u64) -> String {
    let (val, _, unit) = human_bytes(bytes);
    format!("{val:.1} {unit}")
}

/// Format a used/total byte pair like "8.2/16.0 GB".
fn format_bytes_pair(used: u64, total: u64) -> String {
    let (total_val, divisor, unit) = human_bytes(total);
    let used_val = used as f64 / divisor;
    format!("{used_val:.1}/{total_val:.1} {unit}")
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

/// Display a path with `$HOME` replaced by `~`.
fn short_path(path: &Path) -> String {
    let s = path.display().to_string();
    if let Ok(home) = std::env::var("HOME") {
        if let Some(rest) = s.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    s
}

/// Truncate a name to fit within `max_len` characters, appending "…" if truncated.
fn truncate_name(name: &str, max_len: usize) -> String {
    let char_count = name.chars().count();
    if char_count <= max_len {
        name.to_string()
    } else {
        let mut s: String = name.chars().take(max_len - 1).collect();
        s.push('\u{2026}');
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── render_session_name_line / render_session_stats_line tests ──

    fn sample_metrics() -> SessionMetrics {
        SessionMetrics {
            name: "worker-1".into(),
            project_name: "myproj".into(),
            worktree_branch: None,
            is_vm: false,
            is_container: false,
            cpu_percent: 42.0,
            memory_bytes: 1_073_741_824, // 1 GB
        }
    }

    #[test]
    fn session_name_line_shows_name_and_context() {
        let sm = sample_metrics();
        let line = render_session_name_line(&sm, 40);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("worker-1"));
        assert!(text.contains("[myproj]"));
    }

    #[test]
    fn session_name_line_with_branch_and_vm() {
        let sm = SessionMetrics {
            worktree_branch: Some("feat-x".into()),
            is_vm: true,
            ..sample_metrics()
        };
        let line = render_session_name_line(&sm, 50);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("[myproj feat-x VM]"));
    }

    #[test]
    fn session_name_line_with_container() {
        let sm = SessionMetrics {
            is_container: true,
            ..sample_metrics()
        };
        let line = render_session_name_line(&sm, 50);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("[myproj Container]"));
    }

    #[test]
    fn session_name_line_no_context() {
        let sm = SessionMetrics {
            project_name: String::new(),
            ..sample_metrics()
        };
        let line = render_session_name_line(&sm, 30);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains('['));
    }

    #[test]
    fn session_stats_line_shows_cpu_and_ram() {
        let sm = sample_metrics();
        let line = render_session_stats_line(&sm);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("42%"));
        assert!(text.contains("CPU"));
        assert!(text.contains("1.0 GB"));
        assert!(text.contains("RAM"));
    }

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
        assert_eq!(result, "very-long\u{2026}");
        assert_eq!(result.chars().count(), 10);
    }

    #[test]
    fn truncate_name_multibyte() {
        let result = truncate_name("caf\u{00e9}-session", 5);
        assert_eq!(result.chars().count(), 5);
        assert!(result.ends_with('\u{2026}'));
    }

    // ── render_gauge_lines tests ──

    #[test]
    fn render_gauge_lines_zero_percent() {
        let lines = render_gauge_lines("CPU", 0.0, None, 20);
        assert_eq!(lines.len(), 2);
        let header: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(header.contains("CPU"));
        assert!(header.contains("0%"));
        let bar: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        // bar_width = 20 - 2 = 18, all empty
        assert!(bar.contains(&"░".repeat(18)));
        assert!(!bar.contains('█'));
    }

    #[test]
    fn render_gauge_lines_hundred_percent() {
        let lines = render_gauge_lines("CPU", 100.0, None, 20);
        let header: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(header.contains("100%"));
        let bar: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        // bar_width = 18, all filled
        assert!(bar.contains(&"█".repeat(18)));
        assert!(!bar.contains('░'));
    }

    #[test]
    fn render_gauge_lines_fifty_percent() {
        let lines = render_gauge_lines("RAM", 50.0, None, 20);
        let header: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(header.contains("50%"));
    }

    #[test]
    fn render_gauge_lines_with_suffix() {
        let lines = render_gauge_lines("RAM", 50.0, Some("8.0/16.0 GB".to_string()), 20);
        let header: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(header.contains("8.0/16.0 GB"));
        assert!(!header.contains("50%"));
    }

    #[test]
    fn render_gauge_lines_clamps_above_100() {
        let lines = render_gauge_lines("CPU", 150.0, None, 20);
        let header: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(header.contains("100%"));
    }

    #[test]
    fn render_gauge_lines_clamps_below_zero() {
        let lines = render_gauge_lines("CPU", -10.0, None, 20);
        let header: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(header.contains("0%"));
    }

    #[test]
    fn render_gauge_lines_bar_width_matches_inner_width() {
        let inner_width = 16;
        let lines = render_gauge_lines("CPU", 42.0, None, inner_width);
        let bar: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        // bar should be [<filled><empty>] with total bar_width = inner_width - 2
        let bar_content_len = bar.chars().count();
        assert_eq!(bar_content_len, inner_width); // [ + bar_width + ]
    }

    // ── short_path tests ──

    #[test]
    fn short_path_replaces_home() {
        let home = std::env::var("HOME").unwrap();
        let p = std::path::PathBuf::from(format!("{home}/projects/foo"));
        assert_eq!(short_path(&p), "~/projects/foo");
    }

    #[test]
    fn short_path_leaves_non_home_unchanged() {
        let p = std::path::PathBuf::from("/tmp/something");
        assert_eq!(short_path(&p), "/tmp/something");
    }

    // ── format_bytes tests ──

    #[test]
    fn format_bytes_gb() {
        assert_eq!(format_bytes(1_073_741_824), "1.0 GB");
    }

    #[test]
    fn format_bytes_mb() {
        assert_eq!(format_bytes(524_288_000), "500.0 MB");
    }

    // ── separator tests ──

    #[test]
    fn separator_width_matches() {
        let line = separator(16);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.chars().count(), 16);
        assert!(text.chars().all(|c| c == '─'));
    }
}
