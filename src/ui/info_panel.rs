use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use super::theme::Theme;
use crate::session::{AgentMetrics, SessionInfo};

/// View-only entry for upcoming automations shown in the info panel.
pub struct AutomationEntry {
    pub label: String,
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
    automations: &[AutomationEntry],
    usage: Option<&crate::session::AgentUsage>,
    parent_name: Option<&str>,
) {
    let block = Block::default()
        .title(" Info ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::border_unfocused()));

    let inner_width = area.width.saturating_sub(2) as usize;

    let mut lines = Vec::new();

    append_session_section(&mut lines, info, parent_name);
    append_repos_section(&mut lines, info);

    if let Some(ref git) = info.git_stats {
        append_git_section(&mut lines, git);
    }

    if let Some(m) = metrics {
        append_session_resources(&mut lines, m, inner_width);
    }

    if let Some(ref metrics) = info.agent_metrics {
        append_agent_section(&mut lines, metrics, inner_width);
    }

    if let Some(u) = usage {
        if !u.is_empty() {
            append_usage_section(&mut lines, u, inner_width);
        }
    }

    if let Some(m) = metrics {
        append_system_section(&mut lines, m, inner_width);
    }

    append_automations_section(&mut lines, automations, inner_width);

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

/// Append the session header rows: name, status, agent, and the optional
/// parent / host / activity / signal rows.
fn append_session_section<'a>(
    lines: &mut Vec<Line<'a>>,
    info: &'a SessionInfo,
    parent_name: Option<&str>,
) {
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
    // Parent session (lead/worker linkage); omitted for top-level sessions.
    if let Some(parent) = parent_name {
        lines.push(Line::from(vec![
            Span::styled("Parent: ", Theme::label()),
            Span::styled(
                parent.to_string(),
                Style::default().fg(Theme::text_secondary()),
            ),
        ]));
    }
    // Remote host (ssh:<host>); omitted entirely for local sessions.
    if let Some(host) = info.remote_host.as_deref() {
        lines.push(Line::from(vec![
            Span::styled("Host:  ", Theme::label()),
            Span::styled(
                format!("\u{2601} {host}"),
                Style::default().fg(Theme::accent()),
            ),
        ]));
    }
    // Live activity from the agent-emitted OSC terminal title.
    if let Some(activity) = info.agent_activity.as_deref() {
        lines.push(Line::from(vec![
            Span::styled("Activity: ", Theme::label()),
            Span::styled(activity, Style::default().fg(Theme::text_secondary())),
        ]));
    }
    // Latest signal the agent pushed (OSC 9/777 notification). Highlighted while
    // the session is blocked, muted otherwise.
    if let Some(signal) = info.notification.as_deref() {
        let value_style = if info.status == crate::session::SessionStatus::Blocked {
            Style::default().fg(super::status_color(info.status))
        } else {
            Style::default().fg(Theme::text_muted())
        };
        lines.push(Line::from(vec![
            Span::styled("Signal:   ", Theme::label()),
            Span::styled(signal, value_style),
        ]));
    }
}

/// Append the active session's CPU gauge + RAM line (only when non-zero).
fn append_session_resources(lines: &mut Vec<Line<'_>>, m: &SystemMetrics, inner_width: usize) {
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

/// Append the System Resources section (global CPU/RAM gauges).
fn append_system_section(lines: &mut Vec<Line<'_>>, m: &SystemMetrics, inner_width: usize) {
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

/// Append the upcoming-automations section (skipped when there are none).
fn append_automations_section<'a>(
    lines: &mut Vec<Line<'a>>,
    automations: &'a [AutomationEntry],
    inner_width: usize,
) {
    if automations.is_empty() {
        return;
    }
    lines.push(separator(inner_width));
    lines.push(Line::from(Span::styled(
        format!("Automations ({})", automations.len()),
        Theme::section_header(),
    )));
    for entry in automations {
        lines.push(Line::from(vec![
            Span::styled(&entry.countdown, Style::default().fg(Theme::accent())),
            Span::styled("  ", Style::default()),
            Span::styled(&entry.label, Style::default().fg(Theme::text_secondary())),
        ]));
    }
}

/// Build the Agent section header from the model name + CLI version.
fn agent_section_header(metrics: &AgentMetrics) -> String {
    match (&metrics.model_display_name, &metrics.cli_version) {
        (Some(model), Some(ver)) => format!("Agent ({model} v{ver})"),
        (Some(model), None) => format!("Agent ({model})"),
        (None, Some(ver)) => format!("Agent (v{ver})"),
        (None, None) => "Agent".to_string(),
    }
}

/// Append the elapsed-time row, with API time as a muted aside when present.
fn append_agent_time(lines: &mut Vec<Line<'_>>, metrics: &AgentMetrics) {
    let Some(ms) = metrics.total_duration_ms else {
        return;
    };
    let mut spans = vec![
        Span::styled("Time:    ", Theme::label()),
        Span::styled(
            format_duration(ms),
            Style::default().fg(Theme::text_primary()),
        ),
    ];
    if let Some(api_ms) = metrics.total_api_duration_ms.filter(|&v| v > 0) {
        spans.push(Span::styled(
            format!("  (api {})", format_duration(api_ms)),
            Style::default().fg(Theme::text_muted()),
        ));
    }
    lines.push(Line::from(spans));
}

/// Append the tokens row (input / output), skipped when neither is reported.
fn append_agent_tokens(lines: &mut Vec<Line<'_>>, metrics: &AgentMetrics) {
    if metrics.total_input_tokens.is_none() && metrics.total_output_tokens.is_none() {
        return;
    }
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

/// Append the context-window gauge. When the window size is known, show the
/// gauge as used/total tokens rather than a bare percentage.
fn append_agent_context(lines: &mut Vec<Line<'_>>, metrics: &AgentMetrics, inner_width: usize) {
    if let Some(pct) = metrics.used_percentage {
        let suffix = metrics.context_window_size.map(|size| {
            let used = (size as f64 * pct as f64 / 100.0).round() as u64;
            format!("{}/{}", format_tokens(used), format_tokens(size))
        });
        let gauge = render_gauge_lines("Context", pct as f32, suffix, inner_width);
        lines.extend(gauge);
    }
}

/// Append the lines-changed row, skipped when neither is reported.
fn append_agent_lines_changed(lines: &mut Vec<Line<'_>>, metrics: &AgentMetrics) {
    if metrics.total_lines_added.is_none() && metrics.total_lines_removed.is_none() {
        return;
    }
    let added = metrics.total_lines_added.unwrap_or(0);
    let removed = metrics.total_lines_removed.unwrap_or(0);
    lines.push(Line::from(vec![
        Span::styled("Lines:   ", Theme::label()),
        Span::styled(
            format!("+{added}"),
            Style::default().fg(Theme::tool_allowed()),
        ),
        Span::styled(" / ", Style::default().fg(Theme::text_muted())),
        Span::styled(
            format!("-{removed}"),
            Style::default().fg(Theme::status_error()),
        ),
    ]));
}

/// Append the cache-stats row (only when non-zero).
fn append_agent_cache(lines: &mut Vec<Line<'_>>, metrics: &AgentMetrics) {
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

/// Append the Agent section showing Claude CLI metrics.
fn append_agent_section(lines: &mut Vec<Line<'_>>, metrics: &AgentMetrics, inner_width: usize) {
    lines.push(separator(inner_width));
    lines.push(Line::from(Span::styled(
        agent_section_header(metrics),
        Theme::section_header(),
    )));

    // Cost (headline number, only when the agent reports a non-zero spend).
    if let Some(cost) = metrics.total_cost_usd {
        if cost > 0.0 {
            lines.push(Line::from(vec![
                Span::styled("Cost:    ", Theme::label()),
                Span::styled(format_cost(cost), Style::default().fg(Theme::accent())),
            ]));
        }
    }

    append_agent_time(lines, metrics);
    append_agent_tokens(lines, metrics);
    append_agent_context(lines, metrics, inner_width);
    append_agent_lines_changed(lines, metrics);
    append_agent_cache(lines, metrics);
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

/// Append the git change-summary lines (uncommitted diff + sync state).
fn append_git_section(lines: &mut Vec<Line<'_>>, git: &crate::session::GitStats) {
    // Changes line: only when there is something uncommitted.
    if git.files_changed > 0 || git.dirty {
        let files = if git.files_changed == 1 {
            "1 file".to_string()
        } else {
            format!("{} files", git.files_changed)
        };
        let mut spans = vec![
            Span::styled("Changes: ", Theme::label()),
            Span::styled(files, Style::default().fg(Theme::text_primary())),
            Span::raw(" "),
            Span::styled(
                format!("+{}", git.insertions),
                Style::default().fg(Theme::tool_allowed()),
            ),
            Span::styled(" / ", Style::default().fg(Theme::text_muted())),
            Span::styled(
                format!("-{}", git.deletions),
                Style::default().fg(Theme::status_error()),
            ),
        ];
        if git.dirty && git.files_changed == 0 {
            spans.push(Span::styled(
                " dirty",
                Style::default().fg(Theme::text_muted()),
            ));
        }
        lines.push(Line::from(spans));
    }

    // Sync line: only when ahead of / behind the base branch.
    if git.ahead > 0 || git.behind > 0 {
        lines.push(Line::from(vec![
            Span::styled("Sync:    ", Theme::label()),
            Span::styled(
                format!("↑{} ↓{}", git.ahead, git.behind),
                Style::default().fg(Theme::text_muted()),
            ),
        ]));
    }
}

/// Current Unix time in seconds (for usage reset countdowns).
fn epoch_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Format a seconds countdown compactly: `"now"`, `"5m"`, `"1h 12m"`, `"4d 3h"`.
fn format_countdown_secs(secs: u64) -> String {
    if secs == 0 {
        return "now".to_string();
    }
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else if mins > 0 {
        format!("{mins}m")
    } else {
        "<1m".to_string()
    }
}

/// Append the account-level usage / rate-limit section (the `/usage`
/// equivalent): one gauge per window with used-% and a reset countdown.
fn append_usage_section(
    lines: &mut Vec<Line<'_>>,
    usage: &crate::session::AgentUsage,
    width: usize,
) {
    lines.push(separator(width));
    let header = match &usage.plan {
        Some(plan) => format!("Usage ({plan})"),
        None => "Usage".to_string(),
    };
    lines.push(Line::from(Span::styled(header, Theme::section_header())));

    if usage.windows.is_empty() {
        if let Some(note) = &usage.note {
            lines.push(Line::from(Span::styled(
                note.clone(),
                Style::default().fg(Theme::text_muted()),
            )));
        }
        return;
    }

    let now = epoch_now_secs();
    for w in &usage.windows {
        let suffix = match w.resets_at {
            Some(reset) => format!(
                "{:.0}%  {}",
                w.used_percent,
                format_countdown_secs(reset.saturating_sub(now))
            ),
            None => format!("{:.0}%", w.used_percent),
        };
        let gauge = render_gauge_lines(&w.label, w.used_percent, Some(suffix), width);
        lines.extend(gauge);
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

/// Format a USD cost like `$0.0423` (4 dp below $1, 2 dp at/above $1).
fn format_cost(usd: f64) -> String {
    if usd >= 1.0 {
        format!("${usd:.2}")
    } else {
        format!("${usd:.4}")
    }
}

/// Format a millisecond duration as a compact human string:
/// `"820ms"`, `"45s"`, `"1m 23s"`, `"2h 05m"`.
fn format_duration(ms: u64) -> String {
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    let total_secs = ms / 1_000;
    let secs = total_secs % 60;
    let mins = (total_secs / 60) % 60;
    let hours = total_secs / 3_600;
    if hours > 0 {
        format!("{hours}h {mins:02}m")
    } else if mins > 0 {
        format!("{mins}m {secs:02}s")
    } else {
        format!("{secs}s")
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
        // bar_width = 20 - 2 = 18
        assert!(bar.contains(&"░".repeat(18)));
        assert!(!bar.contains('█'));
    }

    #[test]
    fn render_gauge_lines_hundred_percent() {
        let lines = render_gauge_lines("CPU", 100.0, None, 20);
        let header: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(header.contains("100%"));
        let bar: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        // bar_width = 18
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
        let bar_content_len = bar.chars().count();
        // [ + bar_width + ] == inner_width
        assert_eq!(bar_content_len, inner_width);
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

    // ── format_cost tests ──

    #[test]
    fn format_cost_sub_dollar_uses_four_dp() {
        assert_eq!(format_cost(0.0423), "$0.0423");
        assert_eq!(format_cost(0.0), "$0.0000");
    }

    #[test]
    fn format_cost_dollar_and_above_uses_two_dp() {
        assert_eq!(format_cost(1.0), "$1.00");
        assert_eq!(format_cost(12.345), "$12.35");
    }

    // ── format_duration tests ──

    #[test]
    fn format_duration_millis() {
        assert_eq!(format_duration(0), "0ms");
        assert_eq!(format_duration(820), "820ms");
    }

    #[test]
    fn format_duration_seconds() {
        assert_eq!(format_duration(1_000), "1s");
        assert_eq!(format_duration(45_000), "45s");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(format_duration(83_000), "1m 23s");
        assert_eq!(format_duration(600_000), "10m 00s");
    }

    #[test]
    fn format_duration_hours() {
        assert_eq!(format_duration(3_600_000), "1h 00m");
        assert_eq!(format_duration(7_500_000), "2h 05m");
    }

    // ── append_git_section tests ──

    fn render_git(git: &crate::session::GitStats) -> String {
        let mut lines: Vec<Line> = Vec::new();
        append_git_section(&mut lines, git);
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn git_section_shows_changes_and_sync() {
        let git = crate::session::GitStats {
            files_changed: 3,
            insertions: 120,
            deletions: 8,
            dirty: true,
            ahead: 2,
            behind: 0,
        };
        let out = render_git(&git);
        assert!(out.contains("Changes:"));
        assert!(out.contains("3 files"));
        assert!(out.contains("+120"));
        assert!(out.contains("-8"));
        assert!(out.contains("Sync:"));
        assert!(out.contains("↑2 ↓0"));
    }

    #[test]
    fn git_section_singular_file_and_no_sync() {
        let git = crate::session::GitStats {
            files_changed: 1,
            insertions: 5,
            deletions: 0,
            dirty: true,
            ahead: 0,
            behind: 0,
        };
        let out = render_git(&git);
        assert!(out.contains("1 file"));
        assert!(!out.contains("files"));
        assert!(!out.contains("Sync:"));
    }

    #[test]
    fn git_section_clean_repo_is_empty() {
        let git = crate::session::GitStats::default();
        assert_eq!(render_git(&git), "");
    }

    // ── usage section tests ──

    #[test]
    fn format_countdown_variants() {
        assert_eq!(format_countdown_secs(0), "now");
        assert_eq!(format_countdown_secs(30), "<1m");
        assert_eq!(format_countdown_secs(5 * 60), "5m");
        assert_eq!(format_countdown_secs(3600 + 12 * 60), "1h 12m");
        assert_eq!(format_countdown_secs(4 * 86400 + 3 * 3600), "4d 3h");
    }

    fn render_usage(u: &crate::session::AgentUsage) -> String {
        let mut lines: Vec<Line> = Vec::new();
        append_usage_section(&mut lines, u, 24);
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn usage_section_renders_plan_and_windows() {
        let u = crate::session::AgentUsage {
            windows: vec![
                crate::session::UsageWindow {
                    label: "5h".into(),
                    used_percent: 42.0,
                    resets_at: None,
                },
                crate::session::UsageWindow {
                    label: "Week".into(),
                    used_percent: 18.0,
                    resets_at: None,
                },
            ],
            plan: Some("max".into()),
            note: None,
        };
        let out = render_usage(&u);
        assert!(out.contains("Usage (max)"));
        assert!(out.contains("5h"));
        assert!(out.contains("42%"));
        assert!(out.contains("Week"));
        assert!(out.contains("18%"));
    }

    #[test]
    fn usage_section_renders_note_when_no_windows() {
        let u = crate::session::AgentUsage {
            windows: vec![],
            plan: None,
            note: Some("not logged in".into()),
        };
        let out = render_usage(&u);
        assert!(out.contains("Usage"));
        assert!(out.contains("not logged in"));
    }
}
