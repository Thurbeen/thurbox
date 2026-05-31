use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use super::theme::Theme;
use crate::session::{AgentMetrics, SessionInfo};

/// View-only entry for scheduled commands shown in the info panel.
pub struct ScheduledCommandEntry {
    pub command_preview: String,
    pub countdown: String,
}

/// System-wide and active-session resource metrics.
pub struct SystemMetrics {
    /// Overall CPU usage 0-100.
    pub cpu_percent: f32,
    /// Total RAM used in bytes.
    pub memory_used: u64,
    /// Total RAM in bytes.
    pub memory_total: u64,
    /// Active session CPU usage 0-100+.
    pub session_cpu_percent: f32,
    /// Active session memory in bytes.
    pub session_memory_bytes: u64,
}

pub fn render_info_panel(
    frame: &mut Frame,
    area: Rect,
    info: &SessionInfo,
    metrics: Option<&SystemMetrics>,
    scheduled_commands: &[ScheduledCommandEntry],
) {
    let block = Block::default()
        .title(" Info ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::border_unfocused()));

    let inner_width = area.width.saturating_sub(2) as usize;

    let mut lines = Vec::new();

    // ── Session section (most relevant: "what am I looking at") ──
    lines.push(Line::from(vec![
        Span::styled("Name: ", Theme::label()),
        Span::styled(&info.name, Style::default().fg(Theme::text_primary())),
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
        Span::styled("Agent: ", Theme::label()),
        Span::styled(
            &info.agent,
            Style::default()
                .fg(Theme::role_name())
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    // Live activity from the agent-emitted OSC terminal title.
    if let Some(activity) = info.agent_activity.as_deref() {
        lines.push(Line::from(vec![
            Span::styled("Activity: ", Theme::label()),
            Span::styled(activity, Style::default().fg(Theme::text_secondary())),
        ]));
    }

    // ── Repos ──
    append_repos_section(&mut lines, info);

    // ── Session CPU/RAM ──
    if let Some(m) = metrics {
        if m.session_cpu_percent > 0.0 || m.session_memory_bytes > 0 {
            let cpu_gauge = render_gauge_lines("CPU", m.session_cpu_percent, None, inner_width);
            lines.extend(cpu_gauge);

            lines.push(Line::from(vec![
                Span::styled("RAM", Style::default().fg(Theme::text_muted())),
                Span::styled(
                    format!("  {}", format_bytes(m.session_memory_bytes)),
                    Style::default().fg(Theme::text_primary()),
                ),
            ]));
        }
    }

    // ── Agent section (Claude CLI metrics) ──
    if let Some(ref metrics) = info.agent_metrics {
        append_agent_section(&mut lines, metrics, inner_width);
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

    // ── Scheduled commands section ──
    if !scheduled_commands.is_empty() {
        lines.push(separator(inner_width));
        lines.push(Line::from(Span::styled(
            format!("Scheduled ({})", scheduled_commands.len()),
            Theme::section_header(),
        )));
        for entry in scheduled_commands {
            lines.push(Line::from(vec![
                Span::styled(&entry.countdown, Style::default().fg(Theme::accent())),
                Span::styled("  ", Style::default()),
                Span::styled(
                    &entry.command_preview,
                    Style::default().fg(Theme::text_secondary()),
                ),
            ]));
        }
    }

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

/// Append the Agent section showing Claude CLI metrics.
fn append_agent_section(lines: &mut Vec<Line<'_>>, metrics: &AgentMetrics, inner_width: usize) {
    lines.push(separator(inner_width));

    let header = match (&metrics.model_display_name, &metrics.cli_version) {
        (Some(model), Some(ver)) => format!("Agent ({model} v{ver})"),
        (Some(model), None) => format!("Agent ({model})"),
        (None, Some(ver)) => format!("Agent (v{ver})"),
        (None, None) => "Agent".to_string(),
    };
    lines.push(Line::from(Span::styled(header, Theme::section_header())));

    // Tokens
    if metrics.total_input_tokens.is_some() || metrics.total_output_tokens.is_some() {
        let input = metrics
            .total_input_tokens
            .map(format_tokens)
            .unwrap_or_else(|| "-".to_string());
        let output = metrics
            .total_output_tokens
            .map(format_tokens)
            .unwrap_or_else(|| "-".to_string());
        lines.push(Line::from(vec![
            Span::styled("Tokens:  ", Theme::label()),
            Span::styled(
                format!("{input} in / {output} out"),
                Style::default().fg(Theme::text_primary()),
            ),
        ]));
    }

    // Context window gauge
    if let Some(pct) = metrics.used_percentage {
        let gauge = render_gauge_lines("Context", pct as f32, None, inner_width);
        lines.extend(gauge);
    }

    // Lines changed
    if metrics.total_lines_added.is_some() || metrics.total_lines_removed.is_some() {
        let added = metrics.total_lines_added.unwrap_or(0);
        let removed = metrics.total_lines_removed.unwrap_or(0);
        lines.push(Line::from(vec![
            Span::styled("Lines:   ", Theme::label()),
            Span::styled(
                format!("+{added}"),
                Style::default().fg(Theme::status_busy()),
            ),
            Span::styled(" / ", Style::default().fg(Theme::text_muted())),
            Span::styled(
                format!("-{removed}"),
                Style::default().fg(Theme::status_error()),
            ),
        ]));
    }

    // Cache stats (only when non-zero)
    let cache_read = metrics.cache_read_input_tokens.unwrap_or(0);
    let cache_create = metrics.cache_creation_input_tokens.unwrap_or(0);
    if cache_read > 0 || cache_create > 0 {
        lines.push(Line::from(vec![
            Span::styled("Cache:   ", Theme::label()),
            Span::styled(
                format!(
                    "{} read / {} created",
                    format_tokens(cache_read),
                    format_tokens(cache_create)
                ),
                Style::default().fg(Theme::text_primary()),
            ),
        ]));
    }
}

/// Append the repos section showing all repo paths for a session.
fn append_repos_section<'a>(lines: &mut Vec<Line<'a>>, info: &'a SessionInfo) {
    // Collect repo names: first worktree repo, then additional_dirs.
    let first_repo = info
        .worktrees
        .first()
        .map(|wt| &wt.repo_path)
        .or(info.cwd.as_ref())
        .and_then(|p| p.file_name())
        .and_then(|f| f.to_str());

    let branch = info.worktrees.first().map(|wt| wt.branch.as_str());

    // Nothing to show if there's no repo info at all.
    if first_repo.is_none() && branch.is_none() {
        return;
    }

    let primary = match (first_repo, branch) {
        (Some(repo), Some(br)) => format!("{repo}/{br}"),
        (Some(repo), None) => repo.to_string(),
        (None, Some(br)) => br.to_string(),
        (None, None) => return,
    };

    let extra_names: Vec<&str> = info
        .additional_dirs
        .iter()
        .filter_map(|d| d.file_name().and_then(|f| f.to_str()))
        .collect();

    if extra_names.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Repos: ", Theme::label()),
            Span::styled(primary, Style::default().fg(Theme::branch_name())),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled("Repos: ", Theme::label()),
            Span::styled(primary, Style::default().fg(Theme::branch_name())),
        ]));
        for name in &extra_names {
            lines.push(Line::from(vec![
                Span::styled("       ", Theme::label()),
                Span::styled(*name, Style::default().fg(Theme::branch_name())),
            ]));
        }
    }
}

fn separator(width: usize) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width),
        Style::default().fg(Theme::border_unfocused()),
    ))
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
        Span::styled(label.to_string(), Style::default().fg(Theme::text_muted())),
        Span::raw(" ".repeat(padding)),
        Span::styled(right_text, Style::default().fg(Theme::text_primary())),
    ]);

    // Line 2: bar scaled to width - 2 (for [ and ])
    let bar_width = width.saturating_sub(2);
    let filled = ((clamped / 100.0) * bar_width as f32).round() as usize;
    let empty = bar_width.saturating_sub(filled);
    let bar_line = Line::from(vec![
        Span::styled("[", Style::default().fg(Theme::text_muted())),
        Span::styled("█".repeat(filled), Style::default().fg(Theme::accent())),
        Span::styled("░".repeat(empty), Style::default().fg(Theme::text_muted())),
        Span::styled("]", Style::default().fg(Theme::text_muted())),
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

/// Format a token count with human-readable suffixes (e.g., "15.2k", "1.2M").
fn format_tokens(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
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
    fn format_bytes_megabytes() {
        assert_eq!(format_bytes(524_288_000), "500.0 MB");
    }

    #[test]
    fn format_bytes_zero() {
        assert_eq!(format_bytes(0), "0.0 KB");
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

    // ── separator tests ──

    #[test]
    fn separator_width_matches() {
        let line = separator(16);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.chars().count(), 16);
        assert!(text.chars().all(|c| c == '─'));
    }

    // ── format_tokens tests ──

    #[test]
    fn format_tokens_small() {
        assert_eq!(format_tokens(42), "42");
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn format_tokens_thousands() {
        assert_eq!(format_tokens(1_000), "1.0k");
        assert_eq!(format_tokens(15_200), "15.2k");
        assert_eq!(format_tokens(999_999), "1000.0k");
    }

    #[test]
    fn format_tokens_millions() {
        assert_eq!(format_tokens(1_000_000), "1.0M");
        assert_eq!(format_tokens(2_500_000), "2.5M");
    }
}
