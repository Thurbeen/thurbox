use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};

use super::theme::Theme;
use super::{focus_block, status_color, FocusLevel};
use crate::session::SessionInfo;

/// Per-field fuzzy match positions for a session entry.
#[derive(Clone)]
pub struct SessionMatch {
    pub name: Vec<usize>,
    pub role: Vec<usize>,
    pub branch: Vec<usize>,
    pub cwd: Vec<usize>,
    pub status: Vec<usize>,
}

impl SessionMatch {
    /// Build a `SessionMatch` from fuzzy match results, returning `None` if nothing matched.
    pub fn from_matches(
        name: Option<Vec<usize>>,
        role: Option<Vec<usize>>,
        branch: Option<Vec<usize>>,
        cwd: Option<Vec<usize>>,
        status: Option<Vec<usize>>,
    ) -> Option<Self> {
        if name.is_some() || role.is_some() || branch.is_some() || cwd.is_some() || status.is_some()
        {
            Some(Self {
                name: name.unwrap_or_default(),
                role: role.unwrap_or_default(),
                branch: branch.unwrap_or_default(),
                cwd: cwd.unwrap_or_default(),
                status: status.unwrap_or_default(),
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

/// Display-ordered view of the session list with admin sessions pinned to the
/// top. All fields are parallel arrays aligned to the rendered order.
pub struct OrderedSessions<'a> {
    pub sessions: Vec<&'a SessionInfo>,
    pub elapsed_ms: Vec<u64>,
    pub match_positions: Vec<Option<SessionMatch>>,
    pub active_index: usize,
    /// Index of the first non-admin row, or `None` if no non-admin sessions.
    pub first_non_admin_index: Option<usize>,
}

impl<'a> OrderedSessions<'a> {
    /// Reorder the parallel arrays so admin sessions come first (stable), and
    /// remap `active_index` and `match_positions` to follow the new order.
    pub fn new(
        sessions: &[&'a SessionInfo],
        elapsed_ms: &[u64],
        match_positions: &[Option<SessionMatch>],
        active_index: usize,
    ) -> Self {
        let n = sessions.len();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| !sessions[i].is_admin);

        let first_non_admin_index = order.iter().position(|&i| !sessions[i].is_admin);
        let ordered_sessions = order.iter().map(|&i| sessions[i]).collect();
        let ordered_elapsed = order.iter().map(|&i| elapsed_ms[i]).collect();
        let ordered_matches = order
            .iter()
            .map(|&i| match_positions.get(i).cloned().flatten())
            .collect();
        let new_active = order.iter().position(|&i| i == active_index).unwrap_or(0);

        Self {
            sessions: ordered_sessions,
            elapsed_ms: ordered_elapsed,
            match_positions: ordered_matches,
            active_index: new_active,
            first_non_admin_index,
        }
    }
}

pub struct LeftPanelState<'a> {
    pub sessions: &'a [&'a SessionInfo],
    pub active_session: usize,
    /// Elapsed millis since last output, parallel to `sessions`.
    pub session_elapsed_ms: &'a [u64],
    /// Focus level for the session list.
    pub session_focus: FocusLevel,
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
    /// Whether a session search is active (non-empty session_match_positions).
    pub session_search_active: bool,
    /// Number of sessions matching the current search query.
    pub match_count: usize,
    /// Total number of sessions (for search count display).
    pub total_count: usize,
    /// Index of the first non-admin session in the ordered list. When `Some`
    /// and > 0, a subtle divider is rendered above that row to separate admin
    /// sessions (pinned at the top) from the rest.
    pub first_non_admin_index: Option<usize>,
}

pub fn render_left_panel(frame: &mut Frame, area: Rect, state: &mut LeftPanelState<'_>) {
    let search_visible = state.search_active || !state.search_query.is_empty();

    let constraints = if search_visible {
        vec![
            Constraint::Min(0),    // sessions
            Constraint::Length(3), // search bar
        ]
    } else {
        vec![Constraint::Min(0)] // sessions only
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let session_area = chunks[0];
    let search_area = if search_visible {
        Some(chunks[1])
    } else {
        None
    };

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
        state.search_query,
        state.first_non_admin_index,
    );

    if let Some(area) = search_area {
        render_search_bar(
            frame,
            area,
            state.search_query,
            state.search_active,
            state.search_cursor,
            state.match_count,
            state.total_count,
        );
    }
}

/// Overlay scroll indicators ("^" N" / "v N") on the block borders when items
/// are clipped above or below. Renders right-aligned on the top/bottom border
/// lines, consuming no content space.
pub(super) fn render_scroll_indicators(
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
        let text = format!("\u{25b2} {items_above} ");
        let text_len = text.chars().count() as u16;
        let x = block_area
            .x
            .saturating_add(block_area.width.saturating_sub(text_len + 1));
        let area = Rect::new(x, block_area.y, text_len, 1);
        frame.render_widget(Paragraph::new(text).style(indicator_style), area);
    }

    if items_below > 0 {
        let text = format!("\u{25bc} {items_below} ");
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
fn render_search_bar(
    frame: &mut Frame,
    area: Rect,
    query: &str,
    is_active: bool,
    cursor: usize,
    match_count: usize,
    total_count: usize,
) {
    use ratatui::widgets::{Block, Borders};

    let style = if is_active {
        Style::default().fg(Theme::SEARCH_BAR)
    } else {
        Style::default().fg(Theme::TEXT_MUTED)
    };

    let title = if !query.is_empty() {
        format!(" Search ({match_count}/{total_count}) ")
    } else {
        " Search ".to_string()
    };

    let block = Block::default()
        .title(Line::from(Span::styled(title, style)))
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
    search_query: &str,
    first_non_admin_index: Option<usize>,
) {
    let mut block = focus_block(" Sessions ", level);

    if !sessions.is_empty() {
        let dots: Vec<Span> = sessions
            .iter()
            .map(|info| {
                Span::styled(
                    info.status.icon(),
                    Style::default().fg(status_color(info.status)),
                )
            })
            .collect();
        block = block.title_top(Line::from(dots).right_aligned());
    }

    if sessions.is_empty() {
        let text = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "No sessions yet",
                Style::default().fg(Theme::TEXT_MUTED),
            )),
            Line::from(Span::styled(
                "Press Ctrl+N to create one",
                Style::default().fg(Theme::TEXT_MUTED),
            )),
        ])
        .block(block)
        .alignment(ratatui::layout::Alignment::Center);
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
            let prefix = if is_active { "\u{25b8}" } else { " " };
            let is_admin = info.is_admin;

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

            // Build prefix with optional admin badge
            let prefix_str = if is_admin {
                format!("{prefix} \u{2699} {} ", info.status.icon())
            } else {
                format!("{prefix} {} ", info.status.icon())
            };
            let prefix_width = prefix_str.chars().count();
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

            let prefix_style = if is_admin && !is_dimmed {
                Style::default().fg(Theme::ADMIN_BADGE)
            } else {
                status_style
            };

            let mut line1_spans = vec![Span::styled(prefix_str, prefix_style)];
            append_name_spans(
                &mut line1_spans,
                &info.name,
                session_match.and_then(|m| m.positions(&m.name)),
                name_style,
            );

            line1_spans.push(Span::raw(" ".repeat(gap)));
            line1_spans.push(Span::styled(status_text, status_style));
            let line1 = Line::from(line1_spans);

            // Line 2: provisioning step or role [+ tag]
            // Line 3 (optional): repo/branch text
            let mut item_lines = vec![line1];

            if is_dimmed {
                let dimmed = Style::default().fg(Theme::TEXT_MUTED);
                item_lines.push(Line::from(vec![Span::styled(
                    format!("    {}", info.role),
                    dimmed,
                )]));
                let entries = build_repo_entries(info);
                if !entries.is_empty() {
                    let text = format_repo_entries_plain(&entries);
                    if !text.is_empty() {
                        item_lines.push(Line::from(vec![Span::styled(
                            format!("    {text}"),
                            dimmed,
                        )]));
                    }
                }
            } else if info.status == crate::session::SessionStatus::Provisioning {
                let step_text = info.provisioning_step.as_deref().unwrap_or("Starting...");
                item_lines.push(Line::from(vec![
                    Span::styled("    \u{27f3} ", Style::default().fg(Theme::ACCENT)),
                    Span::styled(step_text, Style::default().fg(Theme::ACCENT)),
                ]));
            } else {
                let role_style = Style::default().fg(Theme::ROLE_NAME);
                let mut line2_spans = vec![Span::raw("    ")];
                append_name_spans(
                    &mut line2_spans,
                    &info.role,
                    session_match.and_then(|m| m.positions(&m.role)),
                    role_style,
                );
                if info.vm_id.is_some() {
                    line2_spans.push(Span::styled(
                        " \u{00b7} ",
                        Style::default().fg(Theme::TEXT_MUTED),
                    ));
                    line2_spans.push(Span::styled("VM", Style::default().fg(Theme::ACCENT)));
                } else if info.container_id.is_some() {
                    line2_spans.push(Span::styled(
                        " \u{00b7} ",
                        Style::default().fg(Theme::TEXT_MUTED),
                    ));
                    line2_spans.push(Span::styled(
                        "Container",
                        Style::default().fg(Theme::ACCENT),
                    ));
                }
                item_lines.push(Line::from(line2_spans));

                let entries = build_repo_entries(info);
                if !entries.is_empty() {
                    let repo_style = Style::default().fg(Theme::TEXT_PRIMARY);
                    let branch_style = Style::default().fg(Theme::BRANCH_NAME);
                    let muted = Style::default().fg(Theme::TEXT_MUTED);
                    let mut line3_spans: Vec<Span<'static>> = vec![Span::raw("    ")];

                    // Build the plain text for search matching.
                    let plain = format_repo_entries_plain(&entries);
                    let search_positions = if !search_query.is_empty() {
                        crate::fuzzy::fuzzy_match(search_query, &plain).map(|m| m.positions)
                    } else {
                        None
                    };

                    if let Some(ref positions) = search_positions {
                        if !positions.is_empty() {
                            line3_spans.extend(
                                build_highlighted_spans(&plain, positions, repo_style)
                                    .into_iter()
                                    .map(|s| Span::styled(s.content.into_owned(), s.style)),
                            );
                        } else {
                            line3_spans.push(Span::styled(plain, repo_style));
                        }
                    } else {
                        // No search — render with colored branches.
                        for (i, entry) in entries.iter().enumerate() {
                            if i > 0 {
                                line3_spans.push(Span::styled(", ", muted));
                            }
                            line3_spans.push(Span::styled(entry.name.clone(), repo_style));
                            if let Some(ref br) = entry.branch {
                                line3_spans.push(Span::styled("(", branch_style));
                                line3_spans.push(Span::styled(br.clone(), branch_style));
                                line3_spans.push(Span::styled(")", branch_style));
                            }
                        }
                    }

                    item_lines.push(Line::from(line3_spans));
                }
            }

            // Prepend a subtle divider above the first non-admin session when
            // admin sessions are pinned above it.
            if first_non_admin_index == Some(i) && i > 0 {
                let divider_width = inner_width.max(1);
                let divider = Line::from(Span::styled(
                    "\u{2500}".repeat(divider_width),
                    Style::default().fg(Theme::TEXT_MUTED),
                ));
                item_lines.insert(0, divider);
            }

            ListItem::new(item_lines)
        })
        .collect();

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    list_state.select(Some(active_index));
    frame.render_stateful_widget(list, area, list_state);

    // Most sessions are 3 lines (name + role + repo); use 3 as conservative estimate.
    render_scroll_indicators(frame, area, sessions.len(), list_state, 3);
}

/// A single repo entry with an optional branch (for worktree repos).
struct RepoEntry {
    name: String,
    branch: Option<String>,
}

/// Build a list of repo entries for line 3 of a session entry.
///
/// Uses pre-resolved `repo_display_names` (from git remote or dir name).
/// Worktree repos (indices 0..worktrees.len()) get their branch name.
fn build_repo_entries(info: &SessionInfo) -> Vec<RepoEntry> {
    info.repo_display_names
        .iter()
        .enumerate()
        .map(|(i, name)| RepoEntry {
            name: name.clone(),
            branch: info.worktrees.get(i).map(|wt| wt.branch.clone()),
        })
        .collect()
}

/// Format repo entries as a plain string for search matching and dimmed display.
///
/// Example: `"thurbox(feat-search), shared-lib"`
fn format_repo_entries_plain(entries: &[RepoEntry]) -> String {
    let mut out = String::new();
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&entry.name);
        if let Some(ref br) = entry.branch {
            out.push('(');
            out.push_str(br);
            out.push(')');
        }
    }
    out
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
        assert!(SessionMatch::from_matches(None, None, None, None, None).is_none());
    }

    #[test]
    fn session_match_from_matches_name_only() {
        let m = SessionMatch::from_matches(Some(vec![0, 1]), None, None, None, None).unwrap();
        assert_eq!(m.name, vec![0, 1]);
        assert!(m.role.is_empty());
        assert!(m.branch.is_empty());
        assert!(m.cwd.is_empty());
        assert!(m.status.is_empty());
    }

    #[test]
    fn session_match_from_matches_role_only() {
        let m = SessionMatch::from_matches(None, Some(vec![2]), None, None, None).unwrap();
        assert!(m.name.is_empty());
        assert_eq!(m.role, vec![2]);
        assert!(m.branch.is_empty());
    }

    #[test]
    fn session_match_from_matches_branch_only() {
        let m = SessionMatch::from_matches(None, None, Some(vec![3, 5]), None, None).unwrap();
        assert!(m.name.is_empty());
        assert!(m.role.is_empty());
        assert_eq!(m.branch, vec![3, 5]);
    }

    #[test]
    fn session_match_from_matches_all_fields() {
        let m = SessionMatch::from_matches(
            Some(vec![0]),
            Some(vec![1]),
            Some(vec![2]),
            Some(vec![3]),
            Some(vec![4]),
        )
        .unwrap();
        assert_eq!(m.name, vec![0]);
        assert_eq!(m.role, vec![1]);
        assert_eq!(m.branch, vec![2]);
        assert_eq!(m.cwd, vec![3]);
        assert_eq!(m.status, vec![4]);
    }

    #[test]
    fn session_match_from_matches_cwd_only() {
        let m = SessionMatch::from_matches(None, None, None, Some(vec![0, 3]), None).unwrap();
        assert!(m.name.is_empty());
        assert_eq!(m.cwd, vec![0, 3]);
    }

    #[test]
    fn session_match_from_matches_status_only() {
        let m = SessionMatch::from_matches(None, None, None, None, Some(vec![1])).unwrap();
        assert!(m.name.is_empty());
        assert_eq!(m.status, vec![1]);
    }

    #[test]
    fn session_match_positions_empty_returns_none() {
        let m = SessionMatch::from_matches(Some(vec![0]), None, None, None, None).unwrap();
        assert!(m.positions(&m.role).is_none());
        assert!(m.positions(&m.branch).is_none());
    }

    #[test]
    fn session_match_positions_non_empty_returns_some() {
        let m = SessionMatch::from_matches(Some(vec![0, 4]), None, None, None, None).unwrap();
        assert_eq!(m.positions(&m.name), Some(&[0, 4][..]));
    }

    // --- build_repo_entries / format_repo_entries_plain ---

    #[test]
    fn repo_entries_single_repo() {
        let mut info = SessionInfo::new("test".to_string());
        info.repo_display_names = vec!["thurbox".to_string()];
        let entries = build_repo_entries(&info);
        assert_eq!(format_repo_entries_plain(&entries), "thurbox");
    }

    #[test]
    fn repo_entries_worktree_shows_branch_on_first_repo() {
        let mut info = SessionInfo::new("test".to_string());
        info.repo_display_names = vec!["thurbox".to_string()];
        info.worktrees.push(crate::session::WorktreeInfo {
            repo_path: std::path::PathBuf::from("/home/user/repos/thurbox"),
            worktree_path: std::path::PathBuf::from("/tmp/wt/feat"),
            branch: "feat-search".to_string(),
        });
        let entries = build_repo_entries(&info);
        assert_eq!(format_repo_entries_plain(&entries), "thurbox(feat-search)");
    }

    #[test]
    fn repo_entries_mixed_worktree_and_normal() {
        let mut info = SessionInfo::new("test".to_string());
        info.repo_display_names = vec!["thurbox".to_string(), "shared-lib".to_string()];
        info.worktrees.push(crate::session::WorktreeInfo {
            repo_path: std::path::PathBuf::from("/home/user/repos/thurbox"),
            worktree_path: std::path::PathBuf::from("/tmp/wt/feat"),
            branch: "feat-search".to_string(),
        });
        let entries = build_repo_entries(&info);
        assert_eq!(
            format_repo_entries_plain(&entries),
            "thurbox(feat-search), shared-lib"
        );
    }

    #[test]
    fn repo_entries_multiple_normal_repos() {
        let mut info = SessionInfo::new("test".to_string());
        info.repo_display_names = vec!["main-app".to_string(), "shared-lib".to_string()];
        let entries = build_repo_entries(&info);
        assert_eq!(format_repo_entries_plain(&entries), "main-app, shared-lib");
    }

    #[test]
    fn repo_entries_multiple_worktrees() {
        let mut info = SessionInfo::new("test".to_string());
        info.repo_display_names = vec!["thurbox".to_string(), "api-server".to_string()];
        info.worktrees.push(crate::session::WorktreeInfo {
            repo_path: std::path::PathBuf::from("/repos/thurbox"),
            worktree_path: std::path::PathBuf::from("/tmp/wt1/feat"),
            branch: "feat".to_string(),
        });
        info.worktrees.push(crate::session::WorktreeInfo {
            repo_path: std::path::PathBuf::from("/repos/api-server"),
            worktree_path: std::path::PathBuf::from("/tmp/wt2/feat"),
            branch: "feat".to_string(),
        });
        let entries = build_repo_entries(&info);
        assert_eq!(
            format_repo_entries_plain(&entries),
            "thurbox(feat), api-server(feat)"
        );
    }

    #[test]
    fn repo_entries_multi_worktree_plus_normal() {
        let mut info = SessionInfo::new("test".to_string());
        info.repo_display_names = vec![
            "thurbox".to_string(),
            "api-server".to_string(),
            "docs".to_string(),
        ];
        info.worktrees.push(crate::session::WorktreeInfo {
            repo_path: std::path::PathBuf::from("/repos/thurbox"),
            worktree_path: std::path::PathBuf::from("/tmp/wt1/feat"),
            branch: "feat".to_string(),
        });
        info.worktrees.push(crate::session::WorktreeInfo {
            repo_path: std::path::PathBuf::from("/repos/api-server"),
            worktree_path: std::path::PathBuf::from("/tmp/wt2/feat"),
            branch: "feat".to_string(),
        });
        let entries = build_repo_entries(&info);
        assert_eq!(
            format_repo_entries_plain(&entries),
            "thurbox(feat), api-server(feat), docs"
        );
    }

    #[test]
    fn repo_entries_empty_when_no_repos() {
        let info = SessionInfo::new("test".to_string());
        let entries = build_repo_entries(&info);
        assert!(entries.is_empty());
        assert_eq!(format_repo_entries_plain(&entries), "");
    }

    // --- OrderedSessions ---

    fn info(name: &str, admin: bool) -> SessionInfo {
        let mut s = SessionInfo::new(name.to_string());
        s.is_admin = admin;
        s
    }

    #[test]
    fn ordered_sessions_pins_admins_first_stable() {
        let a = info("admin-a", true);
        let n1 = info("normal-1", false);
        let b = info("admin-b", true);
        let n2 = info("normal-2", false);
        let sessions = vec![&n1, &a, &n2, &b];
        let elapsed = vec![10, 20, 30, 40];
        let matches = vec![None, None, None, None];

        let ordered = OrderedSessions::new(&sessions, &elapsed, &matches, 0);

        let names: Vec<_> = ordered.sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["admin-a", "admin-b", "normal-1", "normal-2"]);
        assert_eq!(ordered.elapsed_ms, vec![20, 40, 10, 30]);
        assert_eq!(ordered.first_non_admin_index, Some(2));
    }

    #[test]
    fn ordered_sessions_remaps_active_index() {
        let a = info("admin", true);
        let n1 = info("normal-1", false);
        let n2 = info("normal-2", false);
        // original order: [n1, n2, a], active = n2 (index 1)
        let sessions = vec![&n1, &n2, &a];
        let elapsed = vec![0, 0, 0];
        let matches = vec![None, None, None];

        let ordered = OrderedSessions::new(&sessions, &elapsed, &matches, 1);

        // new order: [a, n1, n2], n2 lives at index 2
        assert_eq!(ordered.active_index, 2);
    }

    #[test]
    fn ordered_sessions_no_admins_has_no_divider_beyond_zero() {
        let n1 = info("n1", false);
        let n2 = info("n2", false);
        let sessions = vec![&n1, &n2];
        let ordered = OrderedSessions::new(&sessions, &[0, 0], &[None, None], 0);
        // Divider is gated by `Some(i) && i > 0`; with no admins the index is 0.
        assert_eq!(ordered.first_non_admin_index, Some(0));
    }

    #[test]
    fn ordered_sessions_all_admins_has_no_non_admin_index() {
        let a1 = info("a1", true);
        let a2 = info("a2", true);
        let sessions = vec![&a1, &a2];
        let ordered = OrderedSessions::new(&sessions, &[0, 0], &[None, None], 0);
        assert_eq!(ordered.first_non_admin_index, None);
    }

    #[test]
    fn ordered_sessions_empty_input() {
        let sessions: Vec<&SessionInfo> = vec![];
        let ordered = OrderedSessions::new(&sessions, &[], &[], 0);
        assert!(ordered.sessions.is_empty());
        assert_eq!(ordered.active_index, 0);
        assert_eq!(ordered.first_non_admin_index, None);
    }

    #[test]
    fn ordered_sessions_remaps_match_positions() {
        let a = info("admin", true);
        let n = info("normal", false);
        let sessions = vec![&n, &a];
        let elapsed = vec![0, 0];
        let m_normal = SessionMatch::from_matches(Some(vec![0, 1]), None, None, None, None);
        let m_admin = SessionMatch::from_matches(Some(vec![3]), None, None, None, None);
        let matches = vec![m_normal, m_admin];

        let ordered = OrderedSessions::new(&sessions, &elapsed, &matches, 0);

        // Admin bubbles to front, so its match moves with it.
        assert_eq!(
            ordered.match_positions[0].as_ref().map(|m| m.name.clone()),
            Some(vec![3])
        );
        assert_eq!(
            ordered.match_positions[1].as_ref().map(|m| m.name.clone()),
            Some(vec![0, 1])
        );
    }
}
