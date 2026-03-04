use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};

use super::focus_block;
use super::theme::Theme;
use super::FocusLevel;
use crate::session::SessionInfo;

/// Per-field fuzzy match positions for a session entry.
pub struct SessionMatch {
    pub name: Vec<usize>,
    pub role: Vec<usize>,
    pub branch: Vec<usize>,
}

impl SessionMatch {
    /// Build a `SessionMatch` from fuzzy match results, returning `None` if nothing matched.
    pub fn from_matches(
        name: Option<Vec<usize>>,
        role: Option<Vec<usize>>,
        branch: Option<Vec<usize>>,
    ) -> Option<Self> {
        if name.is_some() || role.is_some() || branch.is_some() {
            Some(Self {
                name: name.unwrap_or_default(),
                role: role.unwrap_or_default(),
                branch: branch.unwrap_or_default(),
            })
        } else {
            None
        }
    }

    /// Extract non-empty positions for a field, suitable for `append_name_spans`.
    fn positions<'a>(&self, field: &'a [usize]) -> Option<&'a [usize]> {
        if field.is_empty() {
            None
        } else {
            Some(field)
        }
    }
}

pub struct ProjectEntry<'a> {
    pub name: &'a str,
    pub is_admin: bool,
    pub repo_count: usize,
    pub repo_short: Option<&'a str>,
    pub role_count: usize,
    pub busy_count: usize,
    pub waiting_count: usize,
    pub error_count: usize,
    /// Fuzzy match byte positions (None = no match / dim, Some = highlight).
    pub match_positions: Option<&'a [usize]>,
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
    /// Persistent list state for the project section.
    pub project_list_state: &'a mut ListState,
    /// Persistent list state for the session section.
    pub session_list_state: &'a mut ListState,
    /// Active search query (empty = no search).
    pub search_query: &'a str,
    /// Whether the search input is actively receiving keystrokes.
    pub search_active: bool,
    /// Cursor position within the search query.
    pub search_cursor: usize,
    /// Per-session fuzzy match positions (parallel to sessions slice).
    pub session_match_positions: &'a [Option<SessionMatch>],
    /// Whether the search targets the project section (else sessions).
    pub search_targets_projects: bool,
    /// Whether a project search is active (non-empty project_match_positions).
    pub project_search_active: bool,
    /// Whether a session search is active (non-empty session_match_positions).
    pub session_search_active: bool,
}

pub fn render_left_panel(frame: &mut Frame, area: Rect, state: &mut LeftPanelState<'_>) {
    // All projects in a single list (admin is first, already guaranteed by app)
    let all_projects: Vec<(usize, &ProjectEntry<'_>)> = state.projects.iter().enumerate().collect();

    let search_visible = state.search_active || !state.search_query.is_empty();
    let search_below_projects = search_visible && state.search_targets_projects;
    let search_below_sessions = search_visible && !search_below_projects;

    let constraints = if search_below_projects {
        vec![
            Constraint::Percentage(33), // projects
            Constraint::Length(3),      // search bar (below projects)
            Constraint::Percentage(67), // sessions
        ]
    } else if search_below_sessions {
        vec![
            Constraint::Percentage(33), // projects
            Constraint::Percentage(67), // sessions
            Constraint::Length(3),      // search bar (below sessions)
        ]
    } else {
        vec![
            Constraint::Percentage(33), // projects
            Constraint::Percentage(67), // sessions
        ]
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let (project_area, search_area, session_area) = if search_below_projects {
        (chunks[0], Some(chunks[1]), chunks[2])
    } else if search_below_sessions {
        (chunks[0], Some(chunks[2]), chunks[1])
    } else {
        (chunks[0], None, chunks[1])
    };

    render_project_section(
        frame,
        project_area,
        &all_projects,
        state.active_project,
        state.project_focus,
        state.project_list_state,
        state.project_search_active,
    );

    render_session_section(
        frame,
        session_area,
        state.sessions,
        state.active_session,
        state.session_elapsed_ms,
        state.session_focus,
        state.session_list_state,
        state.session_match_positions,
        state.session_search_active,
    );

    if let Some(area) = search_area {
        render_search_bar(
            frame,
            area,
            state.search_query,
            state.search_active,
            state.search_cursor,
        );
    }
}

/// Overlay scroll indicators ("▲ N" / "▼ N") on the block borders when items
/// are clipped above or below. Renders right-aligned on the top/bottom border
/// lines, consuming no content space.
fn render_scroll_indicators(
    frame: &mut Frame,
    block_area: Rect,
    total_items: usize,
    list_state: &ListState,
    item_height: u16,
) {
    let offset = list_state.offset();
    // Inner height = block_area.height - 2 (top + bottom border)
    let inner_height = block_area.height.saturating_sub(2);
    let visible_count = if item_height > 0 {
        (inner_height / item_height) as usize
    } else {
        0
    };

    let items_above = offset;
    let items_below = total_items.saturating_sub(offset + visible_count);

    let indicator_style = Style::default().fg(Theme::TEXT_MUTED);

    if items_above > 0 {
        let text = format!("▲ {items_above} ");
        let text_len = text.chars().count() as u16;
        let x = block_area
            .x
            .saturating_add(block_area.width.saturating_sub(text_len + 1));
        let area = Rect::new(x, block_area.y, text_len, 1);
        frame.render_widget(Paragraph::new(text).style(indicator_style), area);
    }

    if items_below > 0 {
        let text = format!("▼ {items_below} ");
        let text_len = text.chars().count() as u16;
        let x = block_area
            .x
            .saturating_add(block_area.width.saturating_sub(text_len + 1));
        let y = block_area
            .y
            .saturating_add(block_area.height.saturating_sub(1));
        let area = Rect::new(x, y, text_len, 1);
        frame.render_widget(Paragraph::new(text).style(indicator_style), area);
    }
}

/// Render a bordered search bar block.
fn render_search_bar(frame: &mut Frame, area: Rect, query: &str, is_active: bool, cursor: usize) {
    let style = if is_active {
        Style::default().fg(Theme::ACCENT)
    } else {
        Style::default().fg(Theme::TEXT_MUTED)
    };

    let block = Block::default()
        .title(Line::from(Span::styled(" Search ", style)))
        .borders(Borders::ALL)
        .border_style(style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let max_width = inner.width as usize;
    if max_width == 0 || inner.height == 0 {
        return;
    }

    let prefix = "/ ";
    let display_query = if query.len() + prefix.len() > max_width {
        &query[query.len().saturating_sub(max_width - prefix.len())..]
    } else {
        query
    };

    let (before, after) = if cursor <= display_query.chars().count() {
        let byte_pos = display_query
            .char_indices()
            .nth(cursor)
            .map(|(i, _)| i)
            .unwrap_or(display_query.len());
        (&display_query[..byte_pos], &display_query[byte_pos..])
    } else {
        (display_query, "")
    };

    let mut spans = vec![Span::styled(prefix, style), Span::styled(before, style)];

    if is_active {
        let first_char_len = after.chars().next().map_or(0, |c| c.len_utf8());
        let cursor_char = if first_char_len == 0 {
            " "
        } else {
            &after[..first_char_len]
        };
        spans.push(Span::styled(cursor_char, Theme::cursor()));
        let rest = &after[first_char_len..];
        if !rest.is_empty() {
            spans.push(Span::styled(rest, style));
        }
    } else {
        spans.push(Span::styled(after, style));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
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
fn project_meta_line<'a>(project: &ProjectEntry<'a>, style: Style) -> Line<'a> {
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
        style,
    )])
}

/// Build spans for a name with fuzzy-matched characters highlighted.
fn build_highlighted_spans<'a>(
    name: &'a str,
    positions: &[usize],
    base_style: Style,
) -> Vec<Span<'a>> {
    let highlight_style = base_style
        .fg(Theme::ACCENT)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);

    let mut spans = Vec::new();
    let mut last_end = 0;
    for &byte_pos in positions {
        if byte_pos > name.len() {
            break;
        }
        if let Some(ch) = name[byte_pos..].chars().next() {
            let char_len = ch.len_utf8();
            if byte_pos > last_end {
                spans.push(Span::styled(&name[last_end..byte_pos], base_style));
            }
            spans.push(Span::styled(
                &name[byte_pos..byte_pos + char_len],
                highlight_style,
            ));
            last_end = byte_pos + char_len;
        }
    }
    if last_end < name.len() {
        spans.push(Span::styled(&name[last_end..], base_style));
    }
    spans
}

/// Append name spans to `out`, using fuzzy-highlight positions when available.
fn append_name_spans<'a>(
    out: &mut Vec<Span<'a>>,
    name: &'a str,
    match_positions: Option<&[usize]>,
    style: Style,
) {
    match match_positions {
        Some(positions) if !positions.is_empty() => {
            out.extend(build_highlighted_spans(name, positions, style));
        }
        _ => out.push(Span::styled(name, style)),
    }
}

fn render_project_section(
    frame: &mut Frame,
    area: Rect,
    projects: &[(usize, &ProjectEntry<'_>)],
    active_index: usize,
    level: FocusLevel,
    list_state: &mut ListState,
    search_active: bool,
) {
    let block = focus_block(" Projects ", level);
    let inner_width = area.width.saturating_sub(2) as usize;

    let items: Vec<ListItem> = projects
        .iter()
        .map(|&(orig_idx, project)| {
            let is_active = orig_idx == active_index;
            let indicator = if is_active { "▸" } else { " " };

            let is_dimmed = search_active && project.match_positions.is_none();

            let (prefix, name_color) = if is_dimmed {
                (
                    if project.is_admin {
                        format!("{indicator} ⚙ ")
                    } else {
                        format!("{indicator} ")
                    },
                    Theme::TEXT_MUTED,
                )
            } else if project.is_admin {
                (format!("{indicator} ⚙ "), Theme::ADMIN_BADGE)
            } else {
                (
                    format!("{indicator} "),
                    if is_active {
                        Theme::ACCENT
                    } else {
                        Theme::TEXT_PRIMARY
                    },
                )
            };

            let name_style = if is_dimmed {
                Style::default().fg(Theme::TEXT_MUTED)
            } else if is_active {
                Style::default().fg(name_color).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(name_color)
            };

            let mut line1_spans = vec![Span::styled(prefix, name_style)];
            append_name_spans(
                &mut line1_spans,
                project.name,
                project.match_positions,
                name_style,
            );
            line1_spans.push(Span::raw("  "));
            if !is_dimmed {
                line1_spans.extend(status_dots(project));
            }
            let line1 = Line::from(line1_spans);

            let meta_style = if is_dimmed {
                Style::default().fg(Theme::TEXT_MUTED)
            } else {
                Theme::project_meta()
            };

            let lines = if project.is_admin {
                let line2 = Line::from(Span::styled("    admin", meta_style));
                let separator = Line::from(Span::styled(
                    "─ ".repeat(inner_width / 2),
                    Style::default().fg(Theme::BORDER_UNFOCUSED),
                ));
                vec![line1, line2, separator]
            } else {
                vec![line1, project_meta_line(project, meta_style)]
            };

            ListItem::new(lines)
        })
        .collect();

    // Find which index within the list is active
    let list_active = projects
        .iter()
        .position(|&(orig_idx, _)| orig_idx == active_index);

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    list_state.select(list_active);
    frame.render_stateful_widget(list, area, list_state);

    // Admin entries are 3 lines, regular are 2. Use 2 as the base height
    // since most items are regular projects.
    render_scroll_indicators(frame, area, projects.len(), list_state, 2);
}

#[allow(clippy::too_many_arguments)]
fn render_session_section(
    frame: &mut Frame,
    area: Rect,
    sessions: &[&SessionInfo],
    active_index: usize,
    elapsed_ms: &[u64],
    level: FocusLevel,
    list_state: &mut ListState,
    match_positions: &[Option<SessionMatch>],
    search_active: bool,
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

            // Determine if this session is dimmed (search active + no match).
            let session_match = match_positions.get(i).and_then(|m| m.as_ref());
            let is_dimmed = search_active && session_match.is_none();

            let status_text = format_status_with_elapsed(info.status, elapsed_ms.get(i).copied());
            let name_style = if is_dimmed {
                Style::default().fg(Theme::TEXT_MUTED)
            } else if is_active {
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

            let status_style = if is_dimmed {
                Style::default().fg(Theme::TEXT_MUTED)
            } else {
                Style::default().fg(super::status_color(info.status))
            };

            let mut line1_spans = vec![Span::styled(
                format!("{prefix} {} ", info.status.icon()),
                status_style,
            )];
            append_name_spans(
                &mut line1_spans,
                &info.name,
                session_match.and_then(|m| m.positions(&m.name)),
                name_style,
            );

            line1_spans.push(Span::raw(" ".repeat(gap)));
            line1_spans.push(Span::styled(status_text, status_style));
            let line1 = Line::from(line1_spans);

            // Line 2: provisioning step (if provisioning) or role · branch/VM
            let line2 = if is_dimmed {
                let mut line2_spans = vec![Span::styled(
                    format!("    {}", info.role),
                    Style::default().fg(Theme::TEXT_MUTED),
                )];
                if let Some(wt) = info.worktrees.first() {
                    line2_spans.push(Span::styled(
                        format!(" · {}", wt.branch),
                        Style::default().fg(Theme::TEXT_MUTED),
                    ));
                }
                Line::from(line2_spans)
            } else if info.status == crate::session::SessionStatus::Provisioning {
                let step_text = info.provisioning_step.as_deref().unwrap_or("Starting...");
                Line::from(vec![
                    Span::styled("    ⟳ ", Style::default().fg(Theme::ACCENT)),
                    Span::styled(step_text, Style::default().fg(Theme::ACCENT)),
                ])
            } else {
                let role_style = Style::default().fg(Theme::ROLE_NAME);
                let mut line2_spans = vec![Span::raw("    ")];
                append_name_spans(
                    &mut line2_spans,
                    &info.role,
                    session_match.and_then(|m| m.positions(&m.role)),
                    role_style,
                );
                if let Some(wt) = info.worktrees.first() {
                    line2_spans.push(Span::styled(" · ", Style::default().fg(Theme::TEXT_MUTED)));
                    let branch_style = Style::default().fg(Theme::BRANCH_NAME);
                    append_name_spans(
                        &mut line2_spans,
                        &wt.branch,
                        session_match.and_then(|m| m.positions(&m.branch)),
                        branch_style,
                    );
                }
                if info.vm_id.is_some() {
                    line2_spans.push(Span::styled(" · ", Style::default().fg(Theme::TEXT_MUTED)));
                    line2_spans.push(Span::styled("VM", Style::default().fg(Theme::ACCENT)));
                } else if info.container_id.is_some() {
                    line2_spans.push(Span::styled(" · ", Style::default().fg(Theme::TEXT_MUTED)));
                    line2_spans.push(Span::styled(
                        "Container",
                        Style::default().fg(Theme::ACCENT),
                    ));
                }
                Line::from(line2_spans)
            };

            ListItem::new(vec![line1, line2])
        })
        .collect();

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    list_state.select(Some(active_index));
    frame.render_stateful_widget(list, area, list_state);

    // Sessions are 2 lines per item.
    render_scroll_indicators(frame, area, sessions.len(), list_state, 2);
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
            match_positions: None,
        }
    }

    fn admin_entry<'a>(
        name: &'a str,
        busy: usize,
        waiting: usize,
        error: usize,
    ) -> ProjectEntry<'a> {
        ProjectEntry {
            name,
            is_admin: true,
            repo_count: 0,
            repo_short: None,
            role_count: 0,
            busy_count: busy,
            waiting_count: waiting,
            error_count: error,
            match_positions: None,
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

    #[test]
    fn status_dots_work_for_admin_entries() {
        let entry = admin_entry("Admin", 1, 0, 1);
        let dots = status_dots(&entry);
        assert_eq!(dots.len(), 2);
        assert_eq!(dots[0].content, "●");
        assert_eq!(dots[1].content, "✗");
    }

    // --- project_meta_line ---

    #[test]
    fn meta_line_single_repo_shows_name() {
        let entry = test_entry("P", 0, 0, 0, 1, Some("myrepo"), 2);
        let line = project_meta_line(&entry, Theme::project_meta());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("myrepo"));
        assert!(text.contains("2 roles"));
    }

    #[test]
    fn meta_line_multiple_repos_shows_count() {
        let entry = test_entry("P", 0, 0, 0, 3, None, 1);
        let line = project_meta_line(&entry, Theme::project_meta());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("3 repos"));
        assert!(text.contains("1 role"));
    }

    #[test]
    fn meta_line_no_repos() {
        let entry = test_entry("P", 0, 0, 0, 0, None, 0);
        let line = project_meta_line(&entry, Theme::project_meta());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("no repos"));
        assert!(text.contains("0 roles"));
    }

    #[test]
    fn meta_line_single_repo_without_short_name() {
        let entry = test_entry("P", 0, 0, 0, 1, None, 1);
        let line = project_meta_line(&entry, Theme::project_meta());
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

    // --- build_highlighted_spans ---

    #[test]
    fn highlighted_spans_basic() {
        let style = Style::default().fg(Theme::TEXT_PRIMARY);
        let spans = build_highlighted_spans("foo-bar", &[0, 4], style);
        // Should produce: "f" (highlighted), "oo-" (normal), "b" (highlighted), "ar" (normal)
        assert_eq!(spans.len(), 4);
        assert_eq!(spans[0].content, "f");
        assert_eq!(spans[1].content, "oo-");
        assert_eq!(spans[2].content, "b");
        assert_eq!(spans[3].content, "ar");
    }

    #[test]
    fn highlighted_spans_empty_positions() {
        let style = Style::default().fg(Theme::TEXT_PRIMARY);
        let spans = build_highlighted_spans("hello", &[], style);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello");
    }

    #[test]
    fn highlighted_spans_all_chars() {
        let style = Style::default().fg(Theme::TEXT_PRIMARY);
        let spans = build_highlighted_spans("abc", &[0, 1, 2], style);
        // All chars highlighted, no normal spans between them.
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "a");
        assert_eq!(spans[1].content, "b");
        assert_eq!(spans[2].content, "c");
    }

    // --- append_name_spans ---

    #[test]
    fn append_name_spans_no_match_positions() {
        let style = Style::default().fg(Theme::TEXT_PRIMARY);
        let mut spans = Vec::new();
        append_name_spans(&mut spans, "hello", None, style);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello");
    }

    #[test]
    fn append_name_spans_empty_positions() {
        let style = Style::default().fg(Theme::TEXT_PRIMARY);
        let mut spans = Vec::new();
        append_name_spans(&mut spans, "hello", Some(&[]), style);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello");
    }

    #[test]
    fn append_name_spans_with_highlights() {
        let style = Style::default().fg(Theme::TEXT_PRIMARY);
        let mut spans = vec![Span::raw("prefix ")];
        append_name_spans(&mut spans, "foo-bar", Some(&[0, 4]), style);
        // prefix + "f" (highlighted) + "oo-" (normal) + "b" (highlighted) + "ar" (normal)
        assert_eq!(spans.len(), 5);
        assert_eq!(spans[0].content, "prefix ");
        assert_eq!(spans[1].content, "f");
        assert_eq!(spans[2].content, "oo-");
        assert_eq!(spans[3].content, "b");
        assert_eq!(spans[4].content, "ar");
    }

    // --- SessionMatch ---

    #[test]
    fn session_match_from_matches_all_none_returns_none() {
        assert!(SessionMatch::from_matches(None, None, None).is_none());
    }

    #[test]
    fn session_match_from_matches_name_only() {
        let m = SessionMatch::from_matches(Some(vec![0, 1]), None, None).unwrap();
        assert_eq!(m.name, vec![0, 1]);
        assert!(m.role.is_empty());
        assert!(m.branch.is_empty());
    }

    #[test]
    fn session_match_from_matches_role_only() {
        let m = SessionMatch::from_matches(None, Some(vec![2]), None).unwrap();
        assert!(m.name.is_empty());
        assert_eq!(m.role, vec![2]);
        assert!(m.branch.is_empty());
    }

    #[test]
    fn session_match_from_matches_branch_only() {
        let m = SessionMatch::from_matches(None, None, Some(vec![3, 5])).unwrap();
        assert!(m.name.is_empty());
        assert!(m.role.is_empty());
        assert_eq!(m.branch, vec![3, 5]);
    }

    #[test]
    fn session_match_from_matches_all_fields() {
        let m = SessionMatch::from_matches(Some(vec![0]), Some(vec![1]), Some(vec![2])).unwrap();
        assert_eq!(m.name, vec![0]);
        assert_eq!(m.role, vec![1]);
        assert_eq!(m.branch, vec![2]);
    }

    #[test]
    fn session_match_positions_empty_returns_none() {
        let m = SessionMatch::from_matches(Some(vec![0]), None, None).unwrap();
        assert!(m.positions(&m.role).is_none());
        assert!(m.positions(&m.branch).is_none());
    }

    #[test]
    fn session_match_positions_non_empty_returns_some() {
        let m = SessionMatch::from_matches(Some(vec![0, 4]), None, None).unwrap();
        assert_eq!(m.positions(&m.name), Some(&[0, 4][..]));
    }

    // --- project_meta_line with custom style ---

    #[test]
    fn meta_line_uses_provided_style() {
        let entry = test_entry("P", 0, 0, 0, 1, Some("myrepo"), 1);
        let muted = Style::default().fg(Theme::TEXT_MUTED);
        let line = project_meta_line(&entry, muted);
        assert_eq!(line.spans[0].style, muted);
    }
}
