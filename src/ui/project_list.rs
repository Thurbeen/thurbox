use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};

use super::highlight::append_highlighted as append_name_spans;
use super::theme::Theme;
use super::{focus_block, status_color, FocusLevel};
use crate::session::SessionInfo;

/// Per-field fuzzy match positions for a session entry.
#[derive(Clone)]
pub struct SessionMatch {
    pub name: Vec<usize>,
    pub agent: Vec<usize>,
    pub branch: Vec<usize>,
    pub cwd: Vec<usize>,
    pub status: Vec<usize>,
}

impl SessionMatch {
    /// Build a `SessionMatch` from fuzzy match results, returning `None` if nothing matched.
    pub fn from_matches(
        name: Option<Vec<usize>>,
        agent: Option<Vec<usize>>,
        branch: Option<Vec<usize>>,
        cwd: Option<Vec<usize>>,
        status: Option<Vec<usize>>,
    ) -> Option<Self> {
        if name.is_some()
            || agent.is_some()
            || branch.is_some()
            || cwd.is_some()
            || status.is_some()
        {
            Some(Self {
                name: name.unwrap_or_default(),
                agent: agent.unwrap_or_default(),
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

/// Rank a session status for ordering. Lower ranks sort closer to the top:
/// sessions that need you first, then running, then exited, then errored.
///
/// `Busy` and `Waiting` share one **running** rank on purpose. A live agent
/// flickers across the ~1s "recent output" boundary every tick (`Busy` while
/// emitting, `Waiting` in the gaps), so ranking them apart would make active
/// sessions churn up and down the list endlessly. The status *dot* still shows
/// the distinction; only the ordering ignores it.
fn status_rank(status: crate::session::SessionStatus) -> u8 {
    use crate::session::SessionStatus::{Attention, Busy, Error, Idle, Waiting};
    match status {
        Attention => 0,
        Busy | Waiting => 1,
        Idle => 2,
        Error => 3,
    }
}

/// Fallback label/key for a session that spans no repos.
const NO_REPO_GROUP: &str = "(no repo)";

/// The canonical grouping **key** for a session: the *set* of repos it
/// spans (sorted + de-duplicated), so sessions touching the same repos cluster
/// together regardless of selection order (`{infra, webapp}` == `{webapp,
/// infra}`). Multi-repo sessions thus form their own group, distinct from the
/// single-repo groups of their constituent repos. Falls back to a shared
/// `(no repo)` bucket.
///
/// The `\0` join separator can't occur in a repo name, so distinct sets never
/// collide. This is only a map key — never displayed (see [`group_display`]).
fn group_key(info: &SessionInfo) -> String {
    if info.repo_display_names.is_empty() {
        return NO_REPO_GROUP.to_string();
    }
    let mut names: Vec<&str> = info.repo_display_names.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    names.join("\0")
}

/// The header **label** shown for a session's repo group: its repos joined with
/// ` + ` in the session's natural order (primary repo first), de-duplicated.
/// Falls back to `(no repo)`.
fn group_display(info: &SessionInfo) -> String {
    if info.repo_display_names.is_empty() {
        return NO_REPO_GROUP.to_string();
    }
    let mut seen = std::collections::HashSet::new();
    let parts: Vec<&str> = info
        .repo_display_names
        .iter()
        .map(String::as_str)
        .filter(|n| seen.insert(*n))
        .collect();
    parts.join(" + ")
}

/// Canonical render order for the session list, shared by the rendering widget
/// (`OrderedSessions`) and keyboard navigation (`App::render_order_indices`) so
/// the two never drift.
///
/// Ordering (top → bottom):
///   1. Repo groups, each ordered by its most-urgent member (and then by name),
///      so a repo holding an `Attention` session bubbles above a merely-running
///      one. Each group's first row carries the repo header label.
///   2. Within a group: by status rank, then original index for stability.
///
/// The order is intentionally a pure function of *status* and *stable insertion
/// order* — never of live recency. Recency (`millis_since_last_output`) changes
/// every tick for active sessions, so using it as a sort key made `Busy`
/// sessions reorder endlessly. Status changes are discrete and meaningful
/// (→`Attention`, →`Idle`), so the list only re-sorts when something real
/// happens. See `status_rank` for why `Busy`/`Waiting` are not split.
pub struct SessionOrder {
    /// Input indices in render order.
    pub order: Vec<usize>,
    /// Parallel to `order`: `Some(label)` on each group's first row, else `None`.
    pub headers: Vec<Option<String>>,
}

pub fn compute_session_order(sessions: &[&SessionInfo]) -> SessionOrder {
    struct Group {
        label: Option<String>,
        members: Vec<usize>,
    }

    let mut groups: Vec<Group> = Vec::new();
    let mut key_to_idx: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for (i, info) in sessions.iter().enumerate() {
        // Group by the canonical repo-set key; the header shows the
        // natural-order ` + ` join from the first session in the group.
        let gi = *key_to_idx.entry(group_key(info)).or_insert_with(|| {
            groups.push(Group {
                label: Some(group_display(info)),
                members: Vec::new(),
            });
            groups.len() - 1
        });
        groups[gi].members.push(i);
    }

    // Within each group: status rank, then original index (stable — no recency).
    for g in &mut groups {
        g.members
            .sort_by_key(|&i| (status_rank(sessions[i].status), i));
    }

    // Groups: by most-urgent member (min status rank), then label for
    // determinism. No recency term, so active groups don't churn.
    groups.sort_by(|a, b| {
        let key = |g: &Group| {
            let rank = g
                .members
                .iter()
                .map(|&i| status_rank(sessions[i].status))
                .min()
                .unwrap_or(u8::MAX);
            (rank, g.label.clone().unwrap_or_default())
        };
        key(a).cmp(&key(b))
    });

    let mut order = Vec::with_capacity(sessions.len());
    let mut headers = Vec::with_capacity(sessions.len());
    for g in &groups {
        for (j, &i) in g.members.iter().enumerate() {
            order.push(i);
            headers.push(if j == 0 { g.label.clone() } else { None });
        }
    }

    SessionOrder { order, headers }
}

/// Display-ordered view of the session list. All fields are parallel arrays
/// aligned to the rendered order produced by [`compute_session_order`].
pub struct OrderedSessions<'a> {
    pub sessions: Vec<&'a SessionInfo>,
    pub elapsed_ms: Vec<u64>,
    pub match_positions: Vec<Option<SessionMatch>>,
    pub active_index: usize,
    /// Parallel to `sessions`: `Some(label)` on each repo group's first row,
    /// used to render a subtle header above it. `None` elsewhere.
    pub headers: Vec<Option<String>>,
}

impl<'a> OrderedSessions<'a> {
    /// Reorder the parallel arrays into render order, remapping `active_index`
    /// and `match_positions` to follow it.
    pub fn new(
        sessions: &[&'a SessionInfo],
        elapsed_ms: &[u64],
        match_positions: &[Option<SessionMatch>],
        active_index: usize,
    ) -> Self {
        // Ordering depends only on status + stable index, never on `elapsed_ms`
        // (which is still used below for the per-row elapsed display).
        let SessionOrder { order, headers } = compute_session_order(sessions);

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
            headers,
        }
    }
}

pub struct LeftPanelState<'a> {
    pub sessions: &'a [&'a SessionInfo],
    pub active_session: usize,
    /// Whether to highlight the active-session row. `false` hides the selection
    /// entirely (e.g. while the automations pane is focused, where the active
    /// session is irrelevant).
    pub show_selection: bool,
    /// Elapsed millis since last output, parallel to `sessions`.
    pub session_elapsed_ms: &'a [u64],
    /// Focus level for the session list.
    pub session_focus: FocusLevel,
    /// Persistent list state for the session section.
    pub session_list_state: &'a mut ListState,
    /// Per-session fuzzy match positions (parallel to sessions slice).
    pub session_match_positions: &'a [Option<SessionMatch>],
    /// Whether a (global) search is active — non-matching rows are dimmed.
    pub session_search_active: bool,
    /// Parallel to `sessions`: `Some(label)` on each repo group's first row,
    /// rendered as a subtle header above that row. `None` elsewhere.
    pub headers: &'a [Option<String>],
}

pub fn render_left_panel(frame: &mut Frame, area: Rect, state: &mut LeftPanelState<'_>) {
    // The session list fills the whole left panel — search lives in the global
    // `Ctrl+A` strip now, so there's no in-list search bar.
    render_session_section(
        frame,
        area,
        state.sessions,
        state.active_session,
        state.show_selection,
        state.session_elapsed_ms,
        state.session_focus,
        state.session_list_state,
        state.session_match_positions,
        state.session_search_active,
        state.headers,
    );
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
    let visible_count = inner_height
        .checked_div(item_height)
        .map(|n| n as usize)
        .unwrap_or(0);

    let items_above = offset;
    let items_below = total_items.saturating_sub(offset + visible_count);

    draw_scroll_indicators(frame, block_area, items_above, items_below);
}

/// Variable-height variant: uses per-item heights to compute the visible range.
fn render_scroll_indicators_variable(
    frame: &mut Frame,
    block_area: Rect,
    list_state: &ListState,
    heights: &[u16],
) {
    let offset = list_state.offset().min(heights.len());
    let inner_height = block_area.height.saturating_sub(2);
    let visible_count = visible_count_from_heights(heights, offset, inner_height);

    let items_above = offset;
    let items_below = heights.len().saturating_sub(offset + visible_count);

    draw_scroll_indicators(frame, block_area, items_above, items_below);
}

/// Count how many items starting at `offset` fit entirely within `inner_height`.
fn visible_count_from_heights(heights: &[u16], offset: usize, inner_height: u16) -> usize {
    let mut consumed: u16 = 0;
    let mut count = 0usize;
    for &h in heights.iter().skip(offset) {
        let next = consumed.saturating_add(h);
        if next > inner_height {
            break;
        }
        consumed = next;
        count += 1;
    }
    count
}

fn draw_scroll_indicators(
    frame: &mut Frame,
    block_area: Rect,
    items_above: usize,
    items_below: usize,
) {
    let indicator_style = Style::default().fg(Theme::text_muted());

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

#[allow(clippy::too_many_arguments)]
fn render_session_section(
    frame: &mut Frame,
    area: Rect,
    sessions: &[&SessionInfo],
    active_index: usize,
    show_selection: bool,
    elapsed_ms: &[u64],
    level: FocusLevel,
    list_state: &mut ListState,
    match_positions: &[Option<SessionMatch>],
    search_active: bool,
    headers: &[Option<String>],
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
        render_empty_sessions(frame, area, block);
        return;
    }

    // Available width inside the block (subtract 2 for borders)
    let inner_width = area.width.saturating_sub(2) as usize;

    // Header row that each row belongs to, so a group's header highlights
    // whenever *any* of its members is the active row — not just the first.
    let group_of = header_group_of(headers);
    let active_group = show_selection
        .then(|| group_of.get(active_index).copied())
        .flatten();

    let mut item_heights: Vec<u16> = Vec::with_capacity(sessions.len());

    let items: Vec<ListItem> = sessions
        .iter()
        .enumerate()
        .map(|(i, info)| {
            let is_active = i == active_index && show_selection;
            let session_match = match_positions.get(i).and_then(|m| m.as_ref());
            let is_dimmed = search_active && session_match.is_none();

            let mut item_lines = vec![
                build_session_line1(info, session_match, is_active, is_dimmed),
                build_session_line2(info, is_dimmed, elapsed_ms.get(i).copied()),
            ];

            // Prepend a subtle repo-group header above the first session of
            // each group. The header is highlighted when the active row lives
            // anywhere in its group.
            if let Some(Some(label)) = headers.get(i) {
                let selected = active_group == Some(i);
                item_lines.insert(0, group_header_line(label, inner_width, selected));
            }

            item_heights.push(item_lines.len() as u16);
            ListItem::new(item_lines)
        })
        .collect();

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Theme::selection_bg())
            .fg(Theme::selection_fg())
            .add_modifier(Modifier::BOLD),
    );

    list_state.select(show_selection.then_some(active_index));
    frame.render_stateful_widget(list, area, list_state);

    render_scroll_indicators_variable(frame, area, list_state, &item_heights);
}

/// Render the centered "no sessions yet" placeholder inside the given block.
fn render_empty_sessions(frame: &mut Frame, area: Rect, block: ratatui::widgets::Block) {
    let text = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            "No sessions yet",
            Style::default().fg(Theme::text_muted()),
        )),
        Line::from(Span::styled(
            "Press Ctrl+N to create one",
            Style::default().fg(Theme::text_muted()),
        )),
    ])
    .block(block)
    .alignment(ratatui::layout::Alignment::Center);
    frame.render_widget(text, area);
}

/// For each row, the index of the group header row it belongs to. Lets a group's
/// header highlight whenever *any* member row is the active one, not just the
/// group's first row. Rows before the first header map to row 0, which is
/// harmless since those rows carry no header.
fn header_group_of(headers: &[Option<String>]) -> Vec<usize> {
    let mut out = Vec::with_capacity(headers.len());
    let mut current = 0;
    for (i, h) in headers.iter().enumerate() {
        if h.is_some() {
            current = i;
        }
        out.push(current);
    }
    out
}

/// A full-width repo-group header: `── label ──────────`. Muted by default;
/// painted with the selection background when the active row is in its group.
fn group_header_line(label: &str, inner_width: usize, selected: bool) -> Line<'static> {
    let style = if selected {
        Style::default()
            .bg(Theme::selection_bg())
            .fg(Theme::selection_fg())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Theme::text_muted())
    };
    let mut text = format!("\u{2500}\u{2500} {label} ");
    let used = text.chars().count();
    if inner_width > used {
        text.push_str(&"\u{2500}".repeat(inner_width - used));
    }
    Line::from(Span::styled(text, style))
}

/// Resolve the live status text for a session row (the dedicated status line).
/// Priority:
///   1. Attention → the agent's notification message ("Needs attention").
///   2. The agent-reported OSC activity title (richer "insight").
///   3. Timing-based Busy/Waiting with elapsed time.
fn session_status_text(info: &SessionInfo, elapsed: Option<u64>) -> String {
    if info.status == crate::session::SessionStatus::Attention {
        return info
            .notification
            .clone()
            .unwrap_or_else(|| info.status.to_string());
    }
    info.agent_activity
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format_status_with_elapsed(info.status, elapsed))
}

/// Build line 1 of a session entry: `<status-dot> [⑂] <name>`.
///
/// The active row is signalled by the list's highlight background, so no extra
/// pointer glyph is needed. Sessions running in a git worktree get a `⑂` mark
/// between the status dot and the name. The live status itself lives on line 2.
fn build_session_line1<'a>(
    info: &'a SessionInfo,
    session_match: Option<&SessionMatch>,
    is_active: bool,
    is_dimmed: bool,
) -> Line<'a> {
    let name_style = if is_dimmed {
        Style::default().fg(Theme::text_muted())
    } else if is_active {
        Theme::selected_item()
    } else {
        Theme::normal_item()
    };
    let status_style = if is_dimmed {
        Style::default().fg(Theme::text_muted())
    } else {
        Style::default().fg(super::status_color(info.status))
    };

    let mut line1_spans = vec![Span::styled(
        format!(" {} ", info.status.icon()),
        status_style,
    )];

    // Worktree sessions get a dedicated mark, subordinate to the status dot.
    if !info.worktrees.is_empty() {
        let wt_style = if is_dimmed {
            Style::default().fg(Theme::text_muted())
        } else {
            Style::default().fg(Theme::branch_name())
        };
        line1_spans.push(Span::styled("\u{2442} ", wt_style));
    }

    append_name_spans(
        &mut line1_spans,
        &info.name,
        session_match.and_then(|m| m.positions(&m.name)),
        name_style,
    );

    Line::from(line1_spans)
}

/// Build line 2 of a session entry: the dedicated status line `   <status-text>`
/// (e.g. `   Waiting 2m`). No status glyph here — the colored dot on line 1
/// already conveys the state, so repeating it would be redundant.
fn build_session_line2(info: &SessionInfo, is_dimmed: bool, elapsed: Option<u64>) -> Line<'static> {
    let text_style = if is_dimmed {
        Style::default().fg(Theme::text_muted())
    } else {
        Style::default().fg(Theme::text_primary())
    };

    let status_text = session_status_text(info, elapsed);
    Line::from(vec![
        Span::raw("   "),
        Span::styled(status_text, text_style),
    ])
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
    use super::super::highlight::highlighted_spans as build_highlighted_spans;
    use super::*;
    use crate::session::SessionStatus;

    #[test]
    fn attention_sessions_sort_above_normal_ones() {
        use crate::session::SessionInfo;

        let mut busy = SessionInfo::new("busy".into());
        busy.status = SessionStatus::Busy;
        let mut attn = SessionInfo::new("attn".into());
        attn.status = SessionStatus::Attention;

        let sessions = vec![&busy, &attn];
        let elapsed = vec![0u64, 0u64];
        let matches: Vec<Option<SessionMatch>> = vec![None, None];
        // active_index points at the busy session; the attention one still
        // floats to the top and active_index is remapped to follow it.
        let ordered = OrderedSessions::new(&sessions, &elapsed, &matches, 0);
        assert_eq!(ordered.sessions[0].name, "attn");
        assert_eq!(ordered.sessions[1].name, "busy");
        assert_eq!(ordered.active_index, 1);
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
        let style = Style::default().fg(Theme::text_primary());
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
        let style = Style::default().fg(Theme::text_primary());
        let spans = build_highlighted_spans("hello", &[], style);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello");
    }

    #[test]
    fn highlighted_spans_all_chars() {
        let style = Style::default().fg(Theme::text_primary());
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
        let style = Style::default().fg(Theme::text_primary());
        let mut spans = Vec::new();
        append_name_spans(&mut spans, "hello", None, style);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello");
    }

    #[test]
    fn append_name_spans_empty_positions() {
        let style = Style::default().fg(Theme::text_primary());
        let mut spans = Vec::new();
        append_name_spans(&mut spans, "hello", Some(&[]), style);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello");
    }

    #[test]
    fn append_name_spans_with_highlights() {
        let style = Style::default().fg(Theme::text_primary());
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
        assert!(m.agent.is_empty());
        assert!(m.branch.is_empty());
        assert!(m.cwd.is_empty());
        assert!(m.status.is_empty());
    }

    #[test]
    fn session_match_from_matches_role_only() {
        let m = SessionMatch::from_matches(None, Some(vec![2]), None, None, None).unwrap();
        assert!(m.name.is_empty());
        assert_eq!(m.agent, vec![2]);
        assert!(m.branch.is_empty());
    }

    #[test]
    fn session_match_from_matches_branch_only() {
        let m = SessionMatch::from_matches(None, None, Some(vec![3, 5]), None, None).unwrap();
        assert!(m.name.is_empty());
        assert!(m.agent.is_empty());
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
        assert_eq!(m.agent, vec![1]);
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
        assert!(m.positions(&m.agent).is_none());
        assert!(m.positions(&m.branch).is_none());
    }

    #[test]
    fn session_match_positions_non_empty_returns_some() {
        let m = SessionMatch::from_matches(Some(vec![0, 4]), None, None, None, None).unwrap();
        assert_eq!(m.positions(&m.name), Some(&[0, 4][..]));
    }

    // --- OrderedSessions ---

    fn info(name: &str) -> SessionInfo {
        SessionInfo::new(name.to_string())
    }

    #[test]
    fn ordered_sessions_groups_share_header_on_first_row() {
        let n1 = info("n1");
        let n2 = info("n2");
        let sessions = vec![&n1, &n2];
        let ordered = OrderedSessions::new(&sessions, &[0, 0], &[None, None], 0);
        // Single "(no repo)" group: header on row 0, none after.
        assert_eq!(ordered.headers, vec![Some("(no repo)".to_string()), None]);
    }

    #[test]
    fn ordered_sessions_empty_input() {
        let sessions: Vec<&SessionInfo> = vec![];
        let ordered = OrderedSessions::new(&sessions, &[], &[], 0);
        assert!(ordered.sessions.is_empty());
        assert_eq!(ordered.active_index, 0);
        assert!(ordered.headers.is_empty());
    }

    // --- line builders ---

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn line1_shows_worktree_glyph_when_worktree_present() {
        let mut s = info("feature");
        s.worktrees.push(crate::session::WorktreeInfo {
            repo_path: std::path::PathBuf::from("/repos/thurbox"),
            worktree_path: std::path::PathBuf::from("/tmp/wt/feat"),
            branch: "feat".to_string(),
        });
        let line = build_session_line1(&s, None, false, false);
        assert!(line_text(&line).contains('\u{2442}'));
    }

    #[test]
    fn line1_no_worktree_glyph_for_plain_session() {
        let s = info("plain");
        let line = build_session_line1(&s, None, false, false);
        assert!(!line_text(&line).contains('\u{2442}'));
    }

    #[test]
    fn line2_shows_status_text_without_glyph() {
        let mut s = info("busy");
        s.status = SessionStatus::Busy;
        let line = build_session_line2(&s, false, None);
        let text = line_text(&line);
        assert!(text.contains("Busy"));
        // The status dot lives on line 1 only; line 2 must not repeat it.
        assert!(!text.contains(SessionStatus::Busy.icon()));
    }

    #[test]
    fn line2_attention_shows_notification() {
        let mut s = info("attn");
        s.status = SessionStatus::Attention;
        s.notification = Some("Review this diff".to_string());
        let line = build_session_line2(&s, false, None);
        assert!(line_text(&line).contains("Review this diff"));
    }

    // --- compute_session_order (grouping + activity) ---

    fn info_repo(name: &str, repo: &str, status: SessionStatus) -> SessionInfo {
        info_repos(name, &[repo], status)
    }

    fn info_repos(name: &str, repos: &[&str], status: SessionStatus) -> SessionInfo {
        let mut s = SessionInfo::new(name.to_string());
        s.status = status;
        s.repo_display_names = repos.iter().map(|r| r.to_string()).collect();
        s
    }

    fn order_names<'a>(sessions: &[&'a SessionInfo]) -> Vec<&'a str> {
        compute_session_order(sessions)
            .order
            .into_iter()
            .map(|i| sessions[i].name.as_str())
            .collect()
    }

    #[test]
    fn groups_keep_same_repo_sessions_together() {
        let a = info_repo("a", "webapp", SessionStatus::Waiting);
        let b = info_repo("b", "infra", SessionStatus::Waiting);
        let c = info_repo("c", "webapp", SessionStatus::Waiting);
        let sessions = vec![&a, &b, &c];
        // Equal status: groups ordered by label ("infra" < "webapp"),
        // members of each group adjacent.
        let names = order_names(&sessions);
        assert_eq!(names, vec!["b", "a", "c"]);
    }

    #[test]
    fn group_with_more_urgent_member_bubbles_up() {
        // "infra" only has a Waiting session; "webapp" has an Attention one, so
        // the webapp group sorts above infra even though infra sorts first by name.
        let waiting = info_repo("infra-1", "infra", SessionStatus::Waiting);
        let attn = info_repo("web-attn", "webapp", SessionStatus::Attention);
        let busy = info_repo("web-busy", "webapp", SessionStatus::Busy);
        let sessions = vec![&waiting, &attn, &busy];
        let names = order_names(&sessions);
        // webapp group first (has Attention), ordered Attention then running within.
        assert_eq!(names, vec!["web-attn", "web-busy", "infra-1"]);
    }

    #[test]
    fn busy_and_waiting_share_a_rank_and_keep_stable_order() {
        // A live agent flickers Busy↔Waiting every tick; that must not reorder
        // the list. Both rank as "running", so order stays the insertion order.
        let a = info_repo("a", "webapp", SessionStatus::Busy);
        let b = info_repo("b", "webapp", SessionStatus::Waiting);
        let c = info_repo("c", "webapp", SessionStatus::Busy);
        let sessions = vec![&a, &b, &c];
        assert_eq!(order_names(&sessions), vec!["a", "b", "c"]);

        // Flip a's status Busy→Waiting and b's Waiting→Busy: order is unchanged.
        let a2 = info_repo("a", "webapp", SessionStatus::Waiting);
        let b2 = info_repo("b", "webapp", SessionStatus::Busy);
        let flipped = vec![&a2, &b2, &c];
        assert_eq!(order_names(&flipped), vec!["a", "b", "c"]);
    }

    #[test]
    fn headers_label_only_first_row_of_each_group() {
        let a = info_repo("a", "webapp", SessionStatus::Busy);
        let b = info_repo("b", "webapp", SessionStatus::Busy);
        let c = info_repo("c", "infra", SessionStatus::Attention);
        let sessions = vec![&a, &b, &c];
        let SessionOrder { order, headers } = compute_session_order(&sessions);
        let labelled: Vec<(&str, Option<String>)> = order
            .iter()
            .zip(headers.iter())
            .map(|(&i, h)| (sessions[i].name.as_str(), h.clone()))
            .collect();
        // infra (Attention) group first with its header, then webapp group.
        assert_eq!(
            labelled,
            vec![
                ("c", Some("infra".to_string())),
                ("a", Some("webapp".to_string())),
                ("b", None),
            ]
        );
    }

    #[test]
    fn no_repo_sessions_share_one_group() {
        let a = info("a");
        let b = info("b");
        let sessions = vec![&a, &b];
        let SessionOrder { order, headers } = compute_session_order(&sessions);
        assert_eq!(order, vec![0, 1]);
        assert_eq!(headers, vec![Some("(no repo)".to_string()), None]);
    }

    #[test]
    fn multi_repo_session_forms_its_own_composite_group() {
        // A {webapp}, B {webapp, infra}, C {infra}: three distinct groups, the
        // multi-repo session is NOT folded into either single-repo group.
        let a = info_repo("a", "webapp", SessionStatus::Waiting);
        let b = info_repos("b", &["webapp", "infra"], SessionStatus::Waiting);
        let c = info_repo("c", "infra", SessionStatus::Waiting);
        let sessions = vec![&a, &b, &c];

        let SessionOrder { order, headers } = compute_session_order(&sessions);
        let labelled: Vec<(&str, Option<String>)> = order
            .iter()
            .zip(headers.iter())
            .map(|(&i, h)| (sessions[i].name.as_str(), h.clone()))
            .collect();
        // Groups ordered by label: "infra" < "webapp" < "webapp + infra".
        assert_eq!(
            labelled,
            vec![
                ("c", Some("infra".to_string())),
                ("a", Some("webapp".to_string())),
                ("b", Some("webapp + infra".to_string())),
            ]
        );
    }

    #[test]
    fn duplicate_repos_collapse_to_one_group() {
        // A repo set with the same repo twice (e.g. two worktrees of one repo)
        // groups under that single repo, alongside a plain single-repo session.
        let multi = info_repos("multi", &["webapp", "webapp"], SessionStatus::Waiting);
        let single = info_repo("single", "webapp", SessionStatus::Waiting);
        let sessions = vec![&multi, &single];

        let SessionOrder { order, headers } = compute_session_order(&sessions);
        // Same canonical key → one group; header is the de-duplicated display.
        assert_eq!(order, vec![0, 1]);
        assert_eq!(headers, vec![Some("webapp".to_string()), None]);
    }

    #[test]
    fn multi_repo_grouping_is_order_independent() {
        // Same repo *set*, different selection order → one group; the header
        // reflects the first session's natural order.
        let b = info_repos("b", &["webapp", "infra"], SessionStatus::Waiting);
        let d = info_repos("d", &["infra", "webapp"], SessionStatus::Waiting);
        let sessions = vec![&b, &d];

        let SessionOrder { order, headers } = compute_session_order(&sessions);
        assert_eq!(
            order
                .iter()
                .map(|&i| sessions[i].name.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "d"]
        );
        // Single composite group, header from the first member ("b").
        assert_eq!(headers, vec![Some("webapp + infra".to_string()), None]);
    }

    // --- header_group_of (group-header highlight membership) ---

    #[test]
    fn header_group_of_maps_each_row_to_its_header() {
        // Two groups: rows 0-1 under header 0, rows 2-4 under header 2.
        let headers = vec![
            Some("a".to_string()),
            None,
            Some("b".to_string()),
            None,
            None,
        ];
        assert_eq!(header_group_of(&headers), vec![0, 0, 2, 2, 2]);
    }

    #[test]
    fn header_group_of_rows_before_first_header_map_to_zero() {
        // Rows with no header preceding the first group header at row 2.
        let headers = vec![None, None, Some("repo".to_string()), None];
        assert_eq!(header_group_of(&headers), vec![0, 0, 2, 2]);
    }

    // --- visible_count_from_heights ---

    #[test]
    fn visible_count_fits_all_items_when_tall_enough() {
        let heights = [3u16, 1, 1, 3];
        assert_eq!(visible_count_from_heights(&heights, 0, 100), 4);
    }

    #[test]
    fn visible_count_stops_when_next_item_would_overflow() {
        // 3 + 1 + 1 = 5, next 3 would push to 8 > 6 → stop.
        let heights = [3u16, 1, 1, 3];
        assert_eq!(visible_count_from_heights(&heights, 0, 6), 3);
    }

    #[test]
    fn visible_count_honors_offset() {
        let heights = [3u16, 1, 1, 3];
        // Skip first item, budget 4 → fits 1 + 1 + 2? only 1+1=2 then 3 overflows (2+3=5>4) → 2.
        assert_eq!(visible_count_from_heights(&heights, 1, 4), 2);
    }

    #[test]
    fn visible_count_zero_when_first_item_overflows() {
        let heights = [5u16, 1];
        assert_eq!(visible_count_from_heights(&heights, 0, 3), 0);
    }

    #[test]
    fn visible_count_empty_heights() {
        assert_eq!(visible_count_from_heights(&[], 0, 10), 0);
    }

    #[test]
    fn visible_count_offset_past_end() {
        let heights = [1u16, 1];
        assert_eq!(visible_count_from_heights(&heights, 5, 10), 0);
    }
}
