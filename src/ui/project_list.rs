use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};

use super::theme::Theme;
use super::FocusLevel;
use super::{admin_block, focus_block};
use crate::session::SessionInfo;

pub struct ProjectEntry<'a> {
    pub name: &'a str,
    pub is_admin: bool,
    pub repo_count: usize,
    pub repo_short: Option<&'a str>,
    pub role_count: usize,
    pub busy_count: usize,
    pub waiting_count: usize,
    pub error_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftPanelFocus {
    Projects,
    Sessions,
}

pub struct LeftPanelState<'a> {
    pub projects: &'a [ProjectEntry<'a>],
    pub active_project: usize,
    pub sessions: &'a [&'a SessionInfo],
    pub active_session: usize,
    /// Elapsed millis since last output, parallel to `sessions`.
    pub session_elapsed_ms: &'a [u64],
    pub focus: LeftPanelFocus,
    pub panel_focused: bool,
    /// Focus level for the project sub-section.
    pub project_focus: FocusLevel,
    /// Focus level for the session sub-section.
    pub session_focus: FocusLevel,
}

pub fn render_left_panel(frame: &mut Frame, area: Rect, state: &LeftPanelState<'_>) {
    // Partition projects into regular and admin groups, keeping original indices
    let regular: Vec<(usize, &ProjectEntry<'_>)> = state
        .projects
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.is_admin)
        .collect();
    let admin: Vec<(usize, &ProjectEntry<'_>)> = state
        .projects
        .iter()
        .enumerate()
        .filter(|(_, p)| p.is_admin)
        .collect();

    // 2 lines per regular project, 1 line per admin entry, +2 for borders per section
    let regular_content = regular.len() as u16 * 2;
    let regular_height = regular_content + 2; // borders

    let has_admin = !admin.is_empty();
    let admin_content = admin.len() as u16; // admin entries are single-line
    let admin_height = if has_admin {
        admin_content + 2 // borders
    } else {
        0
    };

    let mut constraints = vec![Constraint::Length(regular_height)];
    if has_admin {
        constraints.push(Constraint::Length(admin_height));
    }
    constraints.push(Constraint::Min(4)); // sessions

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    // Render regular projects section
    render_project_section(
        frame,
        chunks[0],
        &regular,
        state.active_project,
        state.project_focus,
    );

    let session_chunk_idx = if has_admin {
        // Render admin section
        render_admin_section(
            frame,
            chunks[1],
            &admin,
            state.active_project,
            state.project_focus,
        );
        2
    } else {
        1
    };

    render_session_section(
        frame,
        chunks[session_chunk_idx],
        state.sessions,
        state.active_session,
        state.session_elapsed_ms,
        state.session_focus,
    );
}

/// Build status dot spans for a project's aggregate session statuses.
fn status_dots<'a>(project: &ProjectEntry<'a>) -> Vec<Span<'a>> {
    let mut dots = Vec::new();
    for _ in 0..project.busy_count {
        dots.push(Span::styled("●", Style::default().fg(Theme::STATUS_BUSY)));
    }
    for _ in 0..project.waiting_count {
        dots.push(Span::styled(
            "◉",
            Style::default().fg(Theme::STATUS_WAITING),
        ));
    }
    for _ in 0..project.error_count {
        dots.push(Span::styled("✗", Style::default().fg(Theme::STATUS_ERROR)));
    }
    dots
}

/// Build the metadata line (line 2) for a regular project entry.
fn project_meta_line<'a>(project: &ProjectEntry<'a>) -> Line<'a> {
    let repo_text = if project.repo_count == 1 {
        project.repo_short.unwrap_or("1 repo").to_string()
    } else if project.repo_count > 1 {
        format!("{} repos", project.repo_count)
    } else {
        "no repos".to_string()
    };

    let role_text = if project.role_count == 1 {
        "1 role".to_string()
    } else {
        format!("{} roles", project.role_count)
    };

    Line::from(vec![Span::styled(
        format!("    {repo_text} · {role_text}"),
        Theme::project_meta(),
    )])
}

fn render_project_section(
    frame: &mut Frame,
    area: Rect,
    projects: &[(usize, &ProjectEntry<'_>)],
    active_index: usize,
    level: FocusLevel,
) {
    let block = focus_block(" Projects ", level);

    let items: Vec<ListItem> = projects
        .iter()
        .map(|&(orig_idx, project)| {
            let is_active = orig_idx == active_index;
            let name_style = if is_active {
                Style::default()
                    .fg(Theme::ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Theme::TEXT_PRIMARY)
            };

            let indicator = if is_active { "▸" } else { " " };

            // Line 1: indicator + name + status dots
            let mut line1_spans = vec![
                Span::styled(format!("{indicator} "), name_style),
                Span::styled(project.name, name_style),
                Span::raw("  "),
            ];
            line1_spans.extend(status_dots(project));

            let line1 = Line::from(line1_spans);
            let line2 = project_meta_line(project);

            ListItem::new(vec![line1, line2])
        })
        .collect();

    // Find which index within the regular list is active
    let list_active = projects
        .iter()
        .position(|&(orig_idx, _)| orig_idx == active_index);

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    let mut list_state = ListState::default();
    list_state.select(list_active);
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_admin_section(
    frame: &mut Frame,
    area: Rect,
    projects: &[(usize, &ProjectEntry<'_>)],
    active_index: usize,
    level: FocusLevel,
) {
    let block = admin_block(" Admin ", level);

    let items: Vec<ListItem> = projects
        .iter()
        .map(|&(orig_idx, project)| {
            let is_active = orig_idx == active_index;
            let name_style = if is_active {
                Style::default()
                    .fg(Theme::ADMIN_BADGE)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Theme::ADMIN_BADGE)
            };

            let indicator = if is_active { "▸" } else { " " };

            let mut line_spans = vec![
                Span::styled(format!("{indicator} ⚙ "), name_style),
                Span::styled(project.name, name_style),
                Span::raw("  "),
            ];
            line_spans.extend(status_dots(project));

            ListItem::new(Line::from(line_spans))
        })
        .collect();

    let list_active = projects
        .iter()
        .position(|&(orig_idx, _)| orig_idx == active_index);

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    let mut list_state = ListState::default();
    list_state.select(list_active);
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_session_section(
    frame: &mut Frame,
    area: Rect,
    sessions: &[&SessionInfo],
    active_index: usize,
    elapsed_ms: &[u64],
    level: FocusLevel,
) {
    let block = focus_block(" Sessions ", level);

    if sessions.is_empty() {
        let text = Paragraph::new("Ctrl+N to create session")
            .block(block)
            .style(Style::default().fg(Theme::TEXT_MUTED));
        frame.render_widget(text, area);
        return;
    }

    // Available width inside the block (subtract 2 for borders)
    let inner_width = area.width.saturating_sub(2) as usize;

    let items: Vec<ListItem> = sessions
        .iter()
        .enumerate()
        .map(|(i, info)| {
            let is_active = i == active_index;
            let prefix = if is_active { "▸" } else { " " };

            let status_text = format_status_with_elapsed(info.status, elapsed_ms.get(i).copied());
            let name_style = if is_active {
                Theme::selected_item()
            } else {
                Theme::normal_item()
            };

            // "▸ ● " prefix is 4 chars wide (indicator + space + icon + space)
            let prefix_width = 4;
            let name_len = info.name.chars().count();
            let status_len = status_text.chars().count();
            let used = prefix_width + name_len + status_len;
            let gap = if used < inner_width {
                inner_width - used
            } else {
                1
            };

            let status_style = Style::default().fg(super::status_color(info.status));
            let line1 = Line::from(vec![
                Span::styled(format!("{prefix} {} ", info.status.icon()), status_style),
                Span::styled(&info.name, name_style),
                Span::raw(" ".repeat(gap)),
                Span::styled(status_text, status_style),
            ]);

            // Line 2: indented role name + optional · branch
            let mut line2_spans = vec![Span::styled(
                format!("    {}", info.role),
                Style::default().fg(Theme::ROLE_NAME),
            )];
            if let Some(wt) = info.worktrees.first() {
                line2_spans.push(Span::styled(" · ", Style::default().fg(Theme::TEXT_MUTED)));
                line2_spans.push(Span::styled(
                    &wt.branch,
                    Style::default().fg(Theme::BRANCH_NAME),
                ));
            }
            let line2 = Line::from(line2_spans);

            ListItem::new(vec![line1, line2])
        })
        .collect();

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    let mut state = ListState::default();
    state.select(Some(active_index));
    frame.render_stateful_widget(list, area, &mut state);
}

/// Format status text with elapsed time for Waiting/Idle sessions.
fn format_status_with_elapsed(
    status: crate::session::SessionStatus,
    elapsed_ms: Option<u64>,
) -> String {
    use crate::session::SessionStatus;
    match (status, elapsed_ms) {
        (SessionStatus::Waiting | SessionStatus::Idle, Some(ms)) if ms >= 60_000 => {
            let mins = ms / 60_000;
            format!("{status} {mins}m")
        }
        (SessionStatus::Waiting | SessionStatus::Idle, Some(ms)) if ms >= 10_000 => {
            let secs = ms / 1_000;
            format!("{status} {secs}s")
        }
        _ => format!("{status}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionStatus;

    fn test_entry<'a>(
        name: &'a str,
        busy: usize,
        waiting: usize,
        error: usize,
        repo_count: usize,
        repo_short: Option<&'a str>,
        role_count: usize,
    ) -> ProjectEntry<'a> {
        ProjectEntry {
            name,
            is_admin: false,
            repo_count,
            repo_short,
            role_count,
            busy_count: busy,
            waiting_count: waiting,
            error_count: error,
        }
    }

    // --- status_dots ---

    #[test]
    fn status_dots_empty_for_no_sessions() {
        let entry = test_entry("P", 0, 0, 0, 0, None, 0);
        assert!(status_dots(&entry).is_empty());
    }

    #[test]
    fn status_dots_counts_match_input() {
        let entry = test_entry("P", 2, 1, 3, 0, None, 0);
        let dots = status_dots(&entry);
        assert_eq!(dots.len(), 6); // 2 busy + 1 waiting + 3 error
    }

    #[test]
    fn status_dots_ordering_is_busy_waiting_error() {
        let entry = test_entry("P", 1, 1, 1, 0, None, 0);
        let dots = status_dots(&entry);
        assert_eq!(dots.len(), 3);
        // Busy dot uses ●
        assert_eq!(dots[0].content, "●");
        // Waiting dot uses ◉
        assert_eq!(dots[1].content, "◉");
        // Error dot uses ✗
        assert_eq!(dots[2].content, "✗");
    }

    // --- project_meta_line ---

    #[test]
    fn meta_line_single_repo_shows_name() {
        let entry = test_entry("P", 0, 0, 0, 1, Some("myrepo"), 2);
        let line = project_meta_line(&entry);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("myrepo"));
        assert!(text.contains("2 roles"));
    }

    #[test]
    fn meta_line_multiple_repos_shows_count() {
        let entry = test_entry("P", 0, 0, 0, 3, None, 1);
        let line = project_meta_line(&entry);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("3 repos"));
        assert!(text.contains("1 role"));
    }

    #[test]
    fn meta_line_no_repos() {
        let entry = test_entry("P", 0, 0, 0, 0, None, 0);
        let line = project_meta_line(&entry);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("no repos"));
        assert!(text.contains("0 roles"));
    }

    #[test]
    fn meta_line_single_repo_without_short_name() {
        let entry = test_entry("P", 0, 0, 0, 1, None, 1);
        let line = project_meta_line(&entry);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("1 repo"));
        assert!(text.contains("1 role"));
    }

    // --- format_status_with_elapsed ---

    #[test]
    fn elapsed_minutes_shown_above_60s() {
        let text = format_status_with_elapsed(SessionStatus::Waiting, Some(120_000));
        assert_eq!(text, "Waiting 2m");
    }

    #[test]
    fn elapsed_seconds_shown_between_10s_and_60s() {
        let text = format_status_with_elapsed(SessionStatus::Idle, Some(30_000));
        assert_eq!(text, "Idle 30s");
    }

    #[test]
    fn elapsed_not_shown_below_10s() {
        let text = format_status_with_elapsed(SessionStatus::Waiting, Some(5_000));
        assert_eq!(text, "Waiting");
    }

    #[test]
    fn elapsed_not_shown_for_busy() {
        let text = format_status_with_elapsed(SessionStatus::Busy, Some(120_000));
        assert_eq!(text, "Busy");
    }

    #[test]
    fn elapsed_none_shows_plain_status() {
        let text = format_status_with_elapsed(SessionStatus::Error, None);
        assert_eq!(text, "Error");
    }
}
