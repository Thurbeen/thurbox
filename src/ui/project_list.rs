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
use crate::session::{SessionInfo, SessionStatus};

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
///   1. Repo groups, each ordered by its lowest member `display_order` (and
///      then by name). Each group's first row carries the repo header label.
///   2. Within a group: by `display_order`, then original index for stability.
///      Sessions never manually moved (`display_order == None`) sort after all
///      ordered ones, in insertion (= creation) order.
///
/// The order is intentionally a pure function of *manual order*
/// (`display_order`, set by the user via move up/down) and *stable insertion
/// order* — never of status or live recency. A status change (→`Blocked`,
/// →`Idle`) only recolors the dot; rows stay exactly where the user put them.
pub struct SessionOrder {
    /// Input indices in render order.
    pub order: Vec<usize>,
    /// Parallel to `order`: `Some(label)` on each group's first row, else `None`.
    pub headers: Vec<Option<String>>,
    /// Parallel to `order`: tree depth within the repo group (0 = root, 1+ =
    /// child nested under its parent via `parent_session_id`). Children nest
    /// only when their parent is in the same group; a child whose parent lives
    /// in another group (or is gone) stays at depth 0 in its own group.
    pub depths: Vec<u8>,
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

    // Within each group: manual order, then original index (stable — never
    // moved sessions sort after ordered ones, in insertion order).
    let sort_key = |i: usize| (sessions[i].display_order.unwrap_or(i64::MAX), i);
    for g in &mut groups {
        g.members.sort_by_key(|&i| sort_key(i));
    }

    // Groups: by lowest member manual order, then label for determinism.
    // Moves renumber all sessions densely in render order, so a group's
    // minimum reproduces the group order the user last saw. `sort_by_cached_key`
    // computes each group's key once rather than per pairwise comparison.
    groups.sort_by_cached_key(|g| {
        let order = g
            .members
            .iter()
            .map(|&i| sort_key(i).0)
            .min()
            .unwrap_or(i64::MAX);
        (order, g.label.clone().unwrap_or_default())
    });

    let mut order = Vec::with_capacity(sessions.len());
    let mut headers = Vec::with_capacity(sessions.len());
    let mut depths = Vec::with_capacity(sessions.len());
    for g in &groups {
        // Nest children directly under their parent (parent-first, preserving
        // the manual-order-then-index order among siblings and among roots).
        for (j, (i, depth)) in nest_group_members(sessions, &g.members)
            .into_iter()
            .enumerate()
        {
            order.push(i);
            headers.push(if j == 0 { g.label.clone() } else { None });
            depths.push(depth);
        }
    }

    SessionOrder {
        order,
        headers,
        depths,
    }
}

/// Reorder a group's (already manually-sorted) members into parent-first DFS
/// order: every member whose `parent_session_id` is also a member of this group
/// nests directly under that parent; everyone else (no parent, parent in
/// another group, or parent gone) is a root. Returns `(session index, depth)`
/// pairs covering every member exactly once — a visited set plus a flat-emit
/// fallback guard against parent cycles, which current writers can't produce
/// but must not make sessions vanish from the list.
fn nest_group_members(sessions: &[&SessionInfo], members: &[usize]) -> Vec<(usize, u8)> {
    use std::collections::{HashMap, HashSet};

    let id_to_member: HashMap<crate::session::SessionId, usize> =
        members.iter().map(|&i| (sessions[i].id, i)).collect();

    let mut roots: Vec<usize> = Vec::new();
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    for &i in members {
        let parent = sessions[i]
            .parent_session_id
            .and_then(|p| id_to_member.get(&p))
            .copied()
            .filter(|&p| p != i);
        match parent {
            Some(p) => children.entry(p).or_default().push(i),
            None => roots.push(i),
        }
    }

    fn emit(
        i: usize,
        depth: u8,
        children: &HashMap<usize, Vec<usize>>,
        visited: &mut HashSet<usize>,
        out: &mut Vec<(usize, u8)>,
    ) {
        if !visited.insert(i) {
            return;
        }
        out.push((i, depth));
        if let Some(kids) = children.get(&i) {
            for &k in kids {
                emit(k, depth.saturating_add(1), children, visited, out);
            }
        }
    }

    let mut out = Vec::with_capacity(members.len());
    let mut visited = HashSet::new();
    for &r in &roots {
        emit(r, 0, &children, &mut visited, &mut out);
    }
    // Members unreachable from any root (a parent cycle): emit flat.
    for &i in members {
        if visited.insert(i) {
            out.push((i, 0));
        }
    }
    out
}

/// Return a new flat order of **input indices** with each repo group's members
/// sorted alphabetically (case-insensitively) by `name`. Group order is
/// **unchanged** — groups stay in the order [`compute_session_order`] picked
/// them, only their inner ordering changes. Within a group, children sort
/// among their siblings under each parent (recursive DFS, parent first), so
/// the existing parent/child nesting is preserved.
///
/// The caller is expected to renumber `display_order` densely (`0..n`) along
/// the returned order; [`compute_session_order`] then reproduces it exactly.
/// On an empty input returns an empty `Vec`, so a no-op `Shift+S` is safe.
pub fn sort_alphabetically_within_groups(sessions: &[&SessionInfo]) -> Vec<usize> {
    let ord = compute_session_order(sessions);
    let mut out: Vec<usize> = Vec::with_capacity(sessions.len());
    let mut group_start = 0usize;
    for i in 1..=ord.order.len() {
        // Flush at every group boundary (a header on the next row) and at the
        // end of the order. `headers[group_start]` is always `Some`, so the
        // first iteration's check is gated on `i > group_start`.
        let at_boundary = i == ord.order.len() || ord.headers[i].is_some();
        if at_boundary {
            let group: Vec<usize> = ord.order[group_start..i].to_vec();
            out.extend(sort_group_alphabetically(sessions, &group));
            group_start = i;
        }
    }
    out
}

/// Sort one group's members alphabetically while preserving the parent/child
/// nesting `compute_session_order` would render: children stay under their
/// parent and sort among their siblings, recursively. Pre-sorts members by
/// case-insensitive name (with input index as a stable tiebreak) and reuses
/// the existing [`nest_group_members`] DFS — its root/child partitioning and
/// cycle-fallback walk both honor the input order, so the alphabetical
/// pre-sort flows through to roots, children, and leftovers alike.
fn sort_group_alphabetically(sessions: &[&SessionInfo], members: &[usize]) -> Vec<usize> {
    let mut sorted: Vec<usize> = members.to_vec();
    sorted.sort_by_key(|&i| (sessions[i].name.to_lowercase(), i));
    nest_group_members(sessions, &sorted)
        .into_iter()
        .map(|(i, _depth)| i)
        .collect()
}

/// Move the session `active_input_idx` one step up or down in the rendered
/// order, returning the new flat order of **input indices** — or `None` when
/// the move is a no-op (active session missing, or already at an edge it
/// can't cross).
///
/// Every move swaps two adjacent **blocks** (a row plus its rendered subtree,
/// so a parent always drags its nested children):
///   - a root block (depth 0) swaps with the adjacent root block in its repo
///     group; at the group edge the *whole group* swaps with the adjacent
///     group; at the very top/bottom of the list it stays put;
///   - a nested child swaps with its adjacent same-depth sibling and never
///     leaves its parent.
///
/// The caller is expected to renumber `display_order` densely (`0..n`) along
/// the returned order; [`compute_session_order`] then reproduces it exactly
/// (groups stay contiguous runs, DFS nesting preserves block order).
pub fn move_in_order(
    ord: &SessionOrder,
    active_input_idx: usize,
    down: bool,
) -> Option<Vec<usize>> {
    let pos = ord.order.iter().position(|&i| i == active_input_idx)?;

    // The two adjacent ranges to swap: `a` directly precedes `b`. A root block
    // (depth 0) swaps within / across its repo group; a nested child swaps with
    // a same-depth sibling only.
    let (a, b) = if ord.depths[pos] == 0 {
        move_root_block_range(ord, pos, down)?
    } else {
        move_child_block_range(ord, pos, down)?
    };

    debug_assert_eq!(a.end, b.start);
    let mut new_order = Vec::with_capacity(ord.order.len());
    new_order.extend_from_slice(&ord.order[..a.start]);
    new_order.extend_from_slice(&ord.order[b.start..b.end]);
    new_order.extend_from_slice(&ord.order[a.start..a.end]);
    new_order.extend_from_slice(&ord.order[b.end..]);
    Some(new_order)
}

/// End of the block rooted at `start`: the first following row at a depth `<=`
/// `start`'s (i.e. the row past `start`'s whole rendered subtree).
fn block_end(ord: &SessionOrder, start: usize) -> usize {
    let n = ord.order.len();
    let mut end = start + 1;
    while end < n && ord.depths[end] > ord.depths[start] {
        end += 1;
    }
    end
}

/// Start of the group containing `p` (the nearest header row at or above `p`).
fn group_start(ord: &SessionOrder, p: usize) -> Option<usize> {
    (0..=p).rev().find(|&q| ord.headers[q].is_some())
}

/// End of the group starting at `start`: the next header row, or the list end.
fn group_end(ord: &SessionOrder, start: usize) -> usize {
    let n = ord.order.len();
    (start + 1..n)
        .find(|&q| ord.headers[q].is_some())
        .unwrap_or(n)
}

/// Adjacent ranges to swap when moving a **root** block (depth 0): with the
/// neighbouring root block in the same group, or — at a group edge — the whole
/// group with its neighbouring group. `None` at the top/bottom of the list.
fn move_root_block_range(
    ord: &SessionOrder,
    pos: usize,
    down: bool,
) -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let n = ord.order.len();
    let end = block_end(ord, pos);
    let gs = group_start(ord, pos)?;
    let ge = group_end(ord, gs);

    if down {
        if end < ge {
            // Swap with the next root block in the group.
            Some((pos..end, end..block_end(ord, end)))
        } else if ge < n {
            // Last root block: the whole group swaps with the next group.
            Some((gs..ge, ge..group_end(ord, ge)))
        } else {
            None // bottom of the list
        }
    } else if pos > gs {
        // Swap with the previous root block in the group.
        let prev = (gs..pos).rev().find(|&q| ord.depths[q] == 0)?;
        Some((prev..pos, pos..end))
    } else if gs > 0 {
        // First root block: the whole group swaps with the previous group.
        let pgs = group_start(ord, gs - 1)?;
        Some((pgs..gs, gs..ge))
    } else {
        None // top of the list
    }
}

/// Adjacent ranges to swap when moving a **nested child**: with the adjacent
/// same-depth sibling only (never leaving its parent). `None` at the first/last
/// sibling.
fn move_child_block_range(
    ord: &SessionOrder,
    pos: usize,
    down: bool,
) -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let n = ord.order.len();
    let depth = ord.depths[pos];
    let end = block_end(ord, pos);

    if down {
        // Next sibling starts right after our block, at the same depth; a
        // shallower row there means the parent's subtree (or group) ended.
        if end < n && ord.depths[end] == depth {
            Some((pos..end, end..block_end(ord, end)))
        } else {
            None // last sibling
        }
    } else {
        // Scan back over deeper rows (the previous sibling's subtree); a
        // same-depth row is that sibling, a shallower one is our parent.
        let mut p = pos - 1;
        while ord.depths[p] > depth {
            p -= 1;
        }
        if ord.depths[p] == depth {
            Some((p..pos, pos..end))
        } else {
            None // first sibling
        }
    }
}

/// Display-ordered view of the session list. All fields are parallel arrays
/// aligned to the rendered order produced by [`compute_session_order`].
pub struct OrderedSessions<'a> {
    pub sessions: Vec<&'a SessionInfo>,
    pub match_positions: Vec<Option<SessionMatch>>,
    pub active_index: usize,
    /// Parallel to `sessions`: `Some(label)` on each repo group's first row,
    /// used to render a subtle header above it. `None` elsewhere. Borrowed from
    /// the source [`SessionOrder`] (never cloned — the order is render-stable).
    pub headers: &'a [Option<String>],
    /// Parallel to `sessions`: tree depth within the repo group
    /// (see [`SessionOrder::depths`]). Borrowed from the source [`SessionOrder`].
    pub depths: &'a [u8],
}

impl<'a> OrderedSessions<'a> {
    /// Reorder the parallel arrays into render order against a previously
    /// computed [`SessionOrder`], remapping `active_index` and `match_positions`
    /// to follow it. The order is a pure function of the session set's
    /// grouping/nesting inputs (`repo_display_names`, `display_order`, `id`,
    /// `parent_session_id`) — never status — so the caller can cache it keyed by
    /// a content signature and skip the grouping/sort/nest work on frames where
    /// none of those changed (see `App::render_left_panel`). The per-frame remap
    /// of refs / match positions / `active_index` still runs, since those vary
    /// independently of the order; `headers`/`depths` are *borrowed* from the
    /// cached `order` rather than cloned each frame.
    pub fn from_order(
        sessions: &[&'a SessionInfo],
        order: &'a SessionOrder,
        match_positions: &[Option<SessionMatch>],
        active_index: usize,
    ) -> Self {
        let ordered_sessions = order.order.iter().map(|&i| sessions[i]).collect();
        let ordered_matches = order
            .order
            .iter()
            .map(|&i| match_positions.get(i).cloned().flatten())
            .collect();
        let new_active = order
            .order
            .iter()
            .position(|&i| i == active_index)
            .unwrap_or(0);

        Self {
            sessions: ordered_sessions,
            match_positions: ordered_matches,
            active_index: new_active,
            headers: &order.headers,
            depths: &order.depths,
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
    /// Parallel to `sessions`: tree depth within the repo group
    /// (see [`SessionOrder::depths`]). Children render indented.
    pub depths: &'a [u8],
    /// Current animated spinner frame for the `Working` status
    /// (`SPINNER_FRAMES[App::spinner_frame()]`).
    pub spinner: &'a str,
}

pub fn render_left_panel(
    frame: &mut Frame,
    area: Rect,
    state: &mut LeftPanelState<'_>,
) -> Vec<super::RowHitbox> {
    // The session list fills the whole left panel — search lives in the global
    // `Ctrl+/` strip now, so there's no in-list search bar.
    render_session_section(
        frame,
        area,
        state.sessions,
        state.active_session,
        state.show_selection,
        state.session_focus,
        state.session_list_state,
        state.session_match_positions,
        state.session_search_active,
        state.headers,
        state.depths,
        state.spinner,
    )
}

/// Overlay scroll indicators ("^" N" / "v N") on the block borders when items
/// are clipped above or below, using per-item heights to compute the visible
/// range. Renders right-aligned on the top/bottom border lines, consuming no
/// content space.
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
    level: FocusLevel,
    list_state: &mut ListState,
    match_positions: &[Option<SessionMatch>],
    search_active: bool,
    headers: &[Option<String>],
    depths: &[u8],
    spinner: &str,
) -> Vec<super::RowHitbox> {
    let mut block = focus_block(" Sessions ", level);

    if !sessions.is_empty() {
        let dots: Vec<Span> = sessions
            .iter()
            .map(|info| {
                Span::styled(
                    super::status_glyph(info.status, spinner).to_string(),
                    Style::default().fg(status_color(info.status)),
                )
            })
            .collect();
        block = block.title_top(Line::from(dots).right_aligned());
    }

    if sessions.is_empty() {
        render_empty_sessions(frame, area, block);
        return Vec::new();
    }

    // Available width inside the block (subtract 2 for borders)
    let inner_width = area.width.saturating_sub(2) as usize;

    // Header row that each row belongs to, so a group's header highlights
    // whenever *any* of its members is the active row — not just the first.
    let group_of = header_group_of(headers);
    let active_group = show_selection
        .then(|| group_of.get(active_index).copied())
        .flatten();

    // Roll each repo group up to its most-urgent member status, keyed by the
    // group's header row index, so the header shows a single dot you can scan.
    // Computed at render time (not in `SessionOrder`) so status never feeds the
    // order cache or reorders rows — it only recolors.
    let mut group_rollup: std::collections::HashMap<usize, SessionStatus> =
        std::collections::HashMap::new();
    for (i, info) in sessions.iter().enumerate() {
        let h = group_of[i];
        group_rollup
            .entry(h)
            .and_modify(|s| *s = group_status([*s, info.status]))
            .or_insert(info.status);
    }

    let mut item_heights: Vec<u16> = Vec::with_capacity(sessions.len());

    // Session ids on screen, for the cross-group child mark (a child whose
    // parent lives in another repo group gets `↳` instead of indentation).
    let visible_ids: std::collections::HashSet<crate::session::SessionId> =
        sessions.iter().map(|s| s.id).collect();

    let items: Vec<ListItem> = sessions
        .iter()
        .enumerate()
        .map(|(i, info)| {
            let is_active = i == active_index && show_selection;
            let session_match = match_positions.get(i).and_then(|m| m.as_ref());
            let is_dimmed = search_active && session_match.is_none();
            let depth = depths.get(i).copied().unwrap_or(0);
            let cross_group_child = depth == 0
                && info
                    .parent_session_id
                    .is_some_and(|p| p != info.id && visible_ids.contains(&p));

            let mut item_lines = vec![build_session_line(
                info,
                session_match,
                is_active,
                is_dimmed,
                depth,
                cross_group_child,
                inner_width,
                spinner,
            )];

            // Prepend a subtle repo-group header above the first session of
            // each group. The header is highlighted when the active row lives
            // anywhere in its group.
            if let Some(Some(label)) = headers.get(i) {
                let selected = active_group == Some(i);
                let rollup = group_rollup.get(&i).copied().unwrap_or(info.status);
                item_lines.insert(
                    0,
                    group_header_line(label, rollup, inner_width, selected, spinner),
                );
            }

            item_heights.push(item_lines.len() as u16);
            ListItem::new(item_lines)
        })
        .collect();

    // The active-row selection background is painted by `build_session_line`
    // itself (per-span, full width), *not* by the List's `highlight_style` —
    // because `highlight_style` patches the whole item area, which for a row
    // that carries a prepended repo-group header would bleed the background up
    // onto that header. Painting it in the row keeps the highlight to the
    // session line only; the header gets a text-colour change instead.
    let list = List::new(items).block(block);

    list_state.select(show_selection.then_some(active_index));
    frame.render_stateful_widget(list, area, list_state);

    render_scroll_indicators_variable(frame, area, list_state, &item_heights);

    // Hitboxes for the rows actually on screen, computed *after* the stateful
    // render so `list_state.offset()` is the offset ratatui really used. A
    // 2-line item (group header + session row) gets one hitbox spanning both
    // lines, so clicking a group header selects that group's first session.
    let inner = Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    let bottom = inner.y + inner.height;
    let mut hitboxes = Vec::new();
    let mut y = inner.y;
    for (i, &h) in item_heights.iter().enumerate().skip(list_state.offset()) {
        if y >= bottom || h == 0 {
            break;
        }
        let height = h.min(bottom - y);
        hitboxes.push(super::RowHitbox {
            rect: Rect::new(inner.x, y, inner.width, height),
            index: i,
        });
        y += h;
    }
    hitboxes
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

/// A full-width repo-group header: `● ── label ──────────`. The leading dot is
/// the group's rolled-up most-urgent status; the label is muted by default and
/// switches to an accent text colour (no background) when the active row is in
/// its group, so the selection background stays on the session row alone.
fn group_header_line(
    label: &str,
    rollup: SessionStatus,
    inner_width: usize,
    selected: bool,
    spinner: &str,
) -> Line<'static> {
    // Active group: a text-colour change only (accent + bold), never the full
    // selection background — that belongs to the session row, not its header.
    let style = if selected {
        Theme::selected_item()
    } else {
        Style::default().fg(Theme::text_muted())
    };
    let dot = format!("{} ", super::status_glyph(rollup, spinner));
    let mut text = format!("\u{2500}\u{2500} {label} ");
    let used = dot.chars().count() + text.chars().count();
    if inner_width > used {
        text.push_str(&"\u{2500}".repeat(inner_width - used));
    }
    Line::from(vec![
        Span::styled(dot, Style::default().fg(status_color(rollup))),
        Span::styled(text, style),
    ])
}

/// Urgency rank for the repo-group rollup; higher is more urgent.
fn status_urgency(s: SessionStatus) -> u8 {
    match s {
        SessionStatus::Blocked => 4,
        SessionStatus::Error => 3,
        SessionStatus::Working => 2,
        SessionStatus::Done => 1,
        SessionStatus::Idle => 0,
    }
}

/// The most-urgent status across a group's members (Blocked > Error > Working >
/// Done > Idle), so the group header dot surfaces whatever needs attention
/// first. Empty input rolls up to `Idle`.
pub fn group_status(statuses: impl IntoIterator<Item = SessionStatus>) -> SessionStatus {
    statuses
        .into_iter()
        .max_by_key(|s| status_urgency(*s))
        .unwrap_or(SessionStatus::Idle)
}

/// Resolve the agent-reported status text for a session row, if any.
/// Priority:
///   1. Blocked → the agent's notification message (falls back to "Blocked").
///   2. The agent-reported OSC activity title (richer "insight").
///
/// The colored status dot already conveys the state, so a row with no
/// agent-reported text carries no status text at all.
fn agent_status_text(info: &SessionInfo) -> Option<String> {
    if info.status == crate::session::SessionStatus::Blocked {
        return Some(
            info.notification
                .clone()
                .unwrap_or_else(|| info.status.to_string()),
        );
    }
    info.agent_activity
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Separator between the session name and the inline agent status.
const AGENT_STATUS_SEPARATOR: &str = "  ";
/// Minimum columns the inline agent status needs to be worth showing.
const AGENT_STATUS_MIN_WIDTH: usize = 4;

/// Style for the session name span.
fn name_span_style(is_active: bool, is_dimmed: bool) -> Style {
    if is_dimmed {
        Style::default().fg(Theme::text_muted())
    } else if is_active {
        // `build_session_line` paints the active row with `selection_bg`, so the
        // name must use the theme's `selection_fg` (not `accent`) to stay legible
        // on that background — each theme tunes the pair for contrast.
        Style::default()
            .fg(Theme::selection_fg())
            .add_modifier(Modifier::BOLD)
    } else {
        Theme::normal_item()
    }
}

/// Push the prefix marks that sit between the status dot and the name: the
/// tree/cross-group child glyph, the remote-host cloud, and the worktree mark.
fn push_prefix_marks(
    spans: &mut Vec<Span<'_>>,
    info: &SessionInfo,
    is_dimmed: bool,
    depth: u8,
    cross_group_child: bool,
) {
    // Tree prefix for children nested under their parent in the same repo
    // group; `↳` for children whose parent renders elsewhere in the list.
    let tree_style = Style::default().fg(Theme::text_muted());
    if depth > 0 {
        let indent = "  ".repeat(depth as usize - 1);
        spans.push(Span::styled(format!("{indent}\u{2514} "), tree_style));
    } else if cross_group_child {
        spans.push(Span::styled("\u{21b3} ", tree_style));
    }

    // Remote (ssh:<host>) sessions get a cloud mark so it's clear at a glance
    // the agent runs on another machine. Sits right after the status dot.
    if info.remote_host.is_some() {
        spans.push(Span::styled(
            "\u{2601} ",
            mark_style(is_dimmed, Theme::accent),
        ));
    }

    // Worktree sessions get a dedicated mark, subordinate to the status dot.
    if !info.worktrees.is_empty() {
        spans.push(Span::styled(
            "\u{2442} ",
            mark_style(is_dimmed, Theme::branch_name),
        ));
    }
}

/// Style for a prefix mark: muted when the row is dimmed, otherwise the given
/// accent color.
fn mark_style(is_dimmed: bool, color: fn() -> ratatui::style::Color) -> Style {
    if is_dimmed {
        Style::default().fg(Theme::text_muted())
    } else {
        Style::default().fg(color())
    }
}

/// Append the agent-reported status after the name, truncated to fit
/// `inner_width`. An attention notification keeps the status color (same accent
/// as the dot) so it stands out; plain activity text is muted so the name stays
/// the visual anchor.
fn push_agent_status(
    spans: &mut Vec<Span<'_>>,
    info: &SessionInfo,
    status_style: Style,
    inner_width: usize,
) {
    let Some(status_text) = agent_status_text(info) else {
        return;
    };
    let agent_status_style = if info.status == crate::session::SessionStatus::Blocked {
        status_style
    } else {
        Style::default().fg(Theme::text_muted())
    };
    let used: usize = spans
        .iter()
        .map(|s| s.content.chars().count())
        .sum::<usize>()
        + AGENT_STATUS_SEPARATOR.chars().count();
    let avail = inner_width.saturating_sub(used);
    if avail >= AGENT_STATUS_MIN_WIDTH {
        spans.push(Span::raw(AGENT_STATUS_SEPARATOR));
        spans.push(Span::styled(
            super::truncate_ellipsis(&status_text, avail),
            agent_status_style,
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn build_session_line<'a>(
    info: &'a SessionInfo,
    session_match: Option<&SessionMatch>,
    is_active: bool,
    is_dimmed: bool,
    depth: u8,
    cross_group_child: bool,
    inner_width: usize,
    spinner: &str,
) -> Line<'a> {
    let name_style = name_span_style(is_active, is_dimmed);
    let status_style = if is_dimmed {
        Style::default().fg(Theme::text_muted())
    } else {
        Style::default().fg(super::status_color(info.status))
    };

    let mut spans = vec![Span::styled(
        format!(" {} ", super::status_glyph(info.status, spinner)),
        status_style,
    )];

    push_prefix_marks(&mut spans, info, is_dimmed, depth, cross_group_child);

    append_name_spans(
        &mut spans,
        &info.name,
        session_match.and_then(|m| m.positions(&m.name)),
        name_style,
    );

    push_agent_status(&mut spans, info, status_style, inner_width);

    let mut line = Line::from(spans);

    // Paint the selection background across the whole row here, rather than via
    // the List's `highlight_style` (which would also tint the repo-group header
    // prepended above the first row of each group). Pad to the full inner width
    // and patch the selection style over every span so the bar fills the row.
    if is_active {
        let selection_style = Style::default()
            .bg(Theme::selection_bg())
            .fg(Theme::selection_fg())
            .add_modifier(Modifier::BOLD);
        let pad = inner_width.saturating_sub(line.width());
        if pad > 0 {
            line.spans.push(Span::raw(" ".repeat(pad)));
        }
        for span in &mut line.spans {
            span.style = span.style.patch(selection_style);
        }
    }

    line
}

#[cfg(test)]
mod tests {
    use super::super::highlight::highlighted_spans as build_highlighted_spans;
    use super::*;
    use crate::session::SessionStatus;

    #[test]
    fn manually_ordered_sessions_sort_first_and_active_index_follows() {
        use crate::session::SessionInfo;

        let first = SessionInfo::new("first".into());
        let mut moved = SessionInfo::new("moved".into());
        moved.display_order = Some(0);

        let sessions = vec![&first, &moved];
        let matches: Vec<Option<SessionMatch>> = vec![None, None];
        // active_index points at the first input session; the manually ordered
        // one renders above it and active_index is remapped to follow it.
        let order = compute_session_order(&sessions);
        let ordered = OrderedSessions::from_order(&sessions, &order, &matches, 0);
        assert_eq!(ordered.sessions[0].name, "moved");
        assert_eq!(ordered.sessions[1].name, "first");
        assert_eq!(ordered.active_index, 1);
    }

    // --- agent_status_text ---

    #[test]
    fn agent_status_none_without_agent_report() {
        // Working/Done/Idle carry no status text — the colored dot is enough.
        for status in [
            SessionStatus::Working,
            SessionStatus::Done,
            SessionStatus::Idle,
            SessionStatus::Error,
        ] {
            let mut s = info("plain");
            s.status = status;
            assert_eq!(agent_status_text(&s), None);
        }
    }

    #[test]
    fn agent_status_uses_activity_title() {
        let mut s = info("active");
        s.status = SessionStatus::Working;
        s.agent_activity = Some("  Compacting conversation  ".to_string());
        assert_eq!(
            agent_status_text(&s),
            Some("Compacting conversation".to_string())
        );
    }

    #[test]
    fn agent_status_blank_activity_is_none() {
        let mut s = info("blank");
        s.agent_activity = Some("   ".to_string());
        assert_eq!(agent_status_text(&s), None);
    }

    #[test]
    fn agent_status_blocked_prefers_notification() {
        let mut s = info("blocked");
        s.status = SessionStatus::Blocked;
        s.agent_activity = Some("working".to_string());
        s.notification = Some("Needs your approval".to_string());
        assert_eq!(
            agent_status_text(&s),
            Some("Needs your approval".to_string())
        );
    }

    #[test]
    fn agent_status_blocked_without_notification_shows_status() {
        let mut s = info("blocked");
        s.status = SessionStatus::Blocked;
        assert_eq!(
            agent_status_text(&s),
            Some(SessionStatus::Blocked.to_string())
        );
    }

    // --- build_highlighted_spans ---

    #[test]
    fn highlighted_spans_basic() {
        let style = Style::default().fg(Theme::text_primary());
        let spans = build_highlighted_spans("foo-bar", &[0, 4], style);
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
        let order = compute_session_order(&sessions);
        let ordered = OrderedSessions::from_order(&sessions, &order, &[None, None], 0);
        // Single "(no repo)" group: header on row 0, none after.
        assert_eq!(
            ordered.headers.to_vec(),
            vec![Some("(no repo)".to_string()), None]
        );
    }

    #[test]
    fn session_section_hitboxes_span_group_headers() {
        // Two sessions in one "(no repo)" group: the first item is 2 lines
        // tall (header + row) and its hitbox spans both; the second is 1.
        let n1 = info("n1");
        let n2 = info("n2");
        let backend = ratatui::backend::TestBackend::new(30, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut list_state = ListState::default();
        let mut hitboxes = Vec::new();
        terminal
            .draw(|f| {
                let sessions = vec![&n1, &n2];
                let order = compute_session_order(&sessions);
                let ordered = OrderedSessions::from_order(&sessions, &order, &[None, None], 0);
                hitboxes = render_left_panel(
                    f,
                    Rect::new(0, 0, 30, 12),
                    &mut LeftPanelState {
                        sessions: &ordered.sessions,
                        active_session: ordered.active_index,
                        show_selection: true,
                        session_focus: FocusLevel::Focused,
                        session_list_state: &mut list_state,
                        session_match_positions: &ordered.match_positions,
                        session_search_active: false,
                        headers: ordered.headers,
                        depths: ordered.depths,
                        spinner: "◐",
                    },
                );
            })
            .unwrap();
        assert_eq!(hitboxes.len(), 2);
        assert_eq!(hitboxes[0].rect, Rect::new(1, 1, 28, 2));
        assert_eq!(hitboxes[0].index, 0);
        assert_eq!(hitboxes[1].rect, Rect::new(1, 3, 28, 1));
        assert_eq!(hitboxes[1].index, 1);
    }

    #[test]
    fn ordered_sessions_empty_input() {
        let sessions: Vec<&SessionInfo> = vec![];
        let order = compute_session_order(&sessions);
        let ordered = OrderedSessions::from_order(&sessions, &order, &[], 0);
        assert!(ordered.sessions.is_empty());
        assert_eq!(ordered.active_index, 0);
        assert!(ordered.headers.is_empty());
    }

    // --- line builder ---

    /// Inner width wide enough that nothing truncates in these tests.
    const WIDE: usize = 80;

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn line_shows_worktree_glyph_when_worktree_present() {
        let mut s = info("feature");
        s.worktrees.push(crate::session::WorktreeInfo {
            repo_path: std::path::PathBuf::from("/repos/thurbox"),
            worktree_path: std::path::PathBuf::from("/tmp/wt/feat"),
            branch: "feat".to_string(),
        });
        let line = build_session_line(&s, None, false, false, 0, false, WIDE, "◐");
        assert!(line_text(&line).contains('\u{2442}'));
    }

    #[test]
    fn line_no_worktree_glyph_for_plain_session() {
        let s = info("plain");
        let line = build_session_line(&s, None, false, false, 0, false, WIDE, "◐");
        assert!(!line_text(&line).contains('\u{2442}'));
    }

    #[test]
    fn line_shows_remote_glyph_when_remote_host_present() {
        let mut s = info("remote");
        s.remote_host = Some("devbox".to_string());
        let line = build_session_line(&s, None, false, false, 0, false, WIDE, "◐");
        assert!(line_text(&line).contains('\u{2601}'));
    }

    #[test]
    fn line_no_remote_glyph_for_local_session() {
        let s = info("local");
        let line = build_session_line(&s, None, false, false, 0, false, WIDE, "◐");
        assert!(!line_text(&line).contains('\u{2601}'));
    }

    #[test]
    fn line_shows_tree_prefix_for_nested_child() {
        let s = info("worker");
        let line = build_session_line(&s, None, false, false, 1, false, WIDE, "◐");
        assert!(line_text(&line).contains('\u{2514}')); // └
    }

    #[test]
    fn line_shows_arrow_for_cross_group_child() {
        let s = info("worker");
        let line = build_session_line(&s, None, false, false, 0, true, WIDE, "◐");
        let text = line_text(&line);
        assert!(text.contains('\u{21b3}')); // ↳
        assert!(!text.contains('\u{2514}'));
    }

    #[test]
    fn line_carries_no_status_text_without_agent_report() {
        let mut s = info("quiet");
        s.status = SessionStatus::Done;
        let text = line_text(&build_session_line(
            &s, None, false, false, 0, false, WIDE, "◐",
        ));
        assert!(!text.contains("Waiting"));
        assert!(text.trim_end().ends_with("quiet"));
    }

    #[test]
    fn line_appends_agent_activity_after_name() {
        let mut s = info("busy");
        s.status = SessionStatus::Working;
        s.agent_activity = Some("Compacting conversation".to_string());
        let text = line_text(&build_session_line(
            &s, None, false, false, 0, false, WIDE, "◐",
        ));
        assert!(text.contains("busy  Compacting conversation"));
    }

    #[test]
    fn active_line_paints_selection_background_to_full_width() {
        let s = info("active");
        let line = build_session_line(&s, None, true, false, 0, false, WIDE, "◐");
        // The bar is painted on the row itself (every span), so it can't bleed
        // onto a prepended group header, and it fills the full inner width.
        assert!(line
            .spans
            .iter()
            .all(|sp| sp.style.bg == Some(Theme::selection_bg())));
        assert_eq!(line.width(), WIDE);
    }

    #[test]
    fn active_line_tints_spans_without_padding_when_too_narrow() {
        // Inner width smaller than the rendered row: nothing to pad, but the
        // selection background must still cover the spans (and not panic).
        let s = info("a-fairly-long-session-name");
        let line = build_session_line(&s, None, true, false, 0, false, 4, "◐");
        assert!(line
            .spans
            .iter()
            .all(|sp| sp.style.bg == Some(Theme::selection_bg())));
    }

    #[test]
    fn inactive_line_has_no_selection_background() {
        let s = info("idle");
        let line = build_session_line(&s, None, false, false, 0, false, WIDE, "◐");
        assert!(line.spans.iter().all(|sp| sp.style.bg.is_none()));
    }

    #[test]
    fn selected_group_header_changes_text_colour_without_background() {
        let line = group_header_line("repo", SessionStatus::Idle, WIDE, true, "◐");
        // No background tint anywhere — only a text-colour change on the label.
        assert!(line.spans.iter().all(|sp| sp.style.bg.is_none()));
        let label = &line.spans[1];
        assert_eq!(label.style.fg, Some(Theme::accent()));
        assert!(label.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn unselected_group_header_is_muted_without_background() {
        let line = group_header_line("repo", SessionStatus::Idle, WIDE, false, "◐");
        assert!(line.spans.iter().all(|sp| sp.style.bg.is_none()));
        assert_eq!(line.spans[1].style.fg, Some(Theme::text_muted()));
    }

    #[test]
    fn line_attention_appends_notification() {
        let mut s = info("attn");
        s.status = SessionStatus::Blocked;
        s.notification = Some("Review this diff".to_string());
        let text = line_text(&build_session_line(
            &s, None, false, false, 0, false, WIDE, "◐",
        ));
        assert!(text.contains("attn  Review this diff"));
    }

    #[test]
    fn line_truncates_agent_status_with_ellipsis() {
        let mut s = info("busy");
        s.agent_activity = Some("a very long activity title that cannot fit".to_string());
        let line = build_session_line(&s, None, false, false, 0, false, 20, "◐");
        let text = line_text(&line);
        assert!(text.chars().count() <= 20);
        assert!(text.ends_with('\u{2026}'));
    }

    #[test]
    fn line_exact_fit_agent_status_is_not_truncated() {
        let mut s = info("busy");
        s.agent_activity = Some("Ready".to_string());
        // used = " ● " (3) + "busy" (4) + separator (2) = 9; "Ready" fits exactly.
        let text = line_text(&build_session_line(
            &s, None, false, false, 0, false, 14, "◐",
        ));
        assert!(text.ends_with("busy  Ready"));
        assert!(!text.contains('\u{2026}'));
    }

    #[test]
    fn line_agent_status_min_width_boundary() {
        let mut s = info("busy");
        s.agent_activity = Some("Ready".to_string());
        // avail = AGENT_STATUS_MIN_WIDTH → shown truncated; one column less → skipped.
        let shown = line_text(&build_session_line(
            &s, None, false, false, 0, false, 13, "◐",
        ));
        assert!(shown.ends_with("Rea\u{2026}"));
        let skipped = line_text(&build_session_line(
            &s, None, false, false, 0, false, 12, "◐",
        ));
        assert!(skipped.trim_end().ends_with("busy"));
    }

    #[test]
    fn line_skips_agent_status_when_no_room() {
        let mut s = info("a-rather-long-session-name");
        s.agent_activity = Some("activity".to_string());
        let text = line_text(&build_session_line(
            &s, None, false, false, 0, false, 30, "◐",
        ));
        assert!(!text.contains("activity"));
        assert!(text.trim_end().ends_with("a-rather-long-session-name"));
    }

    // --- compute_session_order (grouping + manual order) ---

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

    /// A child of `parent` in the given repo (same helper shape as `info_repo`).
    fn info_child(
        name: &str,
        repo: &str,
        status: SessionStatus,
        parent: &SessionInfo,
    ) -> SessionInfo {
        let mut s = info_repo(name, repo, status);
        s.parent_session_id = Some(parent.id);
        s
    }

    /// `(name, depth)` pairs in render order.
    fn order_names_depths<'a>(sessions: &[&'a SessionInfo]) -> Vec<(&'a str, u8)> {
        let SessionOrder { order, depths, .. } = compute_session_order(sessions);
        order
            .into_iter()
            .zip(depths)
            .map(|(i, d)| (sessions[i].name.as_str(), d))
            .collect()
    }

    #[test]
    fn groups_keep_same_repo_sessions_together() {
        let a = info_repo("a", "webapp", SessionStatus::Done);
        let b = info_repo("b", "infra", SessionStatus::Done);
        let c = info_repo("c", "webapp", SessionStatus::Done);
        let sessions = vec![&a, &b, &c];
        // Equal status: groups ordered by label ("infra" < "webapp"),
        // members of each group adjacent.
        let names = order_names(&sessions);
        assert_eq!(names, vec!["b", "a", "c"]);
    }

    #[test]
    fn status_never_reorders_groups() {
        // Manual order wins: a Blocked session recolors its dot but never
        // bubbles its group. Groups stay in label order ("infra" < "webapp").
        let waiting = info_repo("infra-1", "infra", SessionStatus::Done);
        let attn = info_repo("web-attn", "webapp", SessionStatus::Blocked);
        let busy = info_repo("web-busy", "webapp", SessionStatus::Working);
        let sessions = vec![&waiting, &attn, &busy];
        assert_eq!(
            order_names(&sessions),
            vec!["infra-1", "web-attn", "web-busy"]
        );

        // Flip every status: the order is identical.
        let waiting2 = info_repo("infra-1", "infra", SessionStatus::Blocked);
        let attn2 = info_repo("web-attn", "webapp", SessionStatus::Idle);
        let busy2 = info_repo("web-busy", "webapp", SessionStatus::Error);
        let flipped = vec![&waiting2, &attn2, &busy2];
        assert_eq!(
            order_names(&flipped),
            vec!["infra-1", "web-attn", "web-busy"]
        );
    }

    #[test]
    fn group_status_rolls_up_to_most_urgent() {
        use SessionStatus::*;
        // Precedence: Blocked > Error > Working > Done > Idle.
        assert_eq!(group_status([Idle, Done, Working]), Working);
        assert_eq!(group_status([Done, Idle]), Done);
        assert_eq!(group_status([Working, Error]), Error);
        assert_eq!(group_status([Working, Blocked, Error]), Blocked);
        assert_eq!(group_status([Idle, Idle]), Idle);
        // Empty rolls up to Idle.
        assert_eq!(group_status(std::iter::empty()), Idle);
    }

    #[test]
    fn status_changes_never_reorder_within_a_group() {
        // A live agent flips Working↔Done every tick, and sessions go
        // Idle/Blocked; none of it may move a row.
        let a = info_repo("a", "webapp", SessionStatus::Working);
        let b = info_repo("b", "webapp", SessionStatus::Done);
        let c = info_repo("c", "webapp", SessionStatus::Working);
        let sessions = vec![&a, &b, &c];
        assert_eq!(order_names(&sessions), vec!["a", "b", "c"]);

        // Flip a→Blocked and b→Idle: order is unchanged.
        let a2 = info_repo("a", "webapp", SessionStatus::Blocked);
        let b2 = info_repo("b", "webapp", SessionStatus::Idle);
        let flipped = vec![&a2, &b2, &c];
        assert_eq!(order_names(&flipped), vec!["a", "b", "c"]);
    }

    #[test]
    fn headers_label_only_first_row_of_each_group() {
        let a = info_repo("a", "webapp", SessionStatus::Working);
        let b = info_repo("b", "webapp", SessionStatus::Working);
        let c = info_repo("c", "infra", SessionStatus::Blocked);
        let sessions = vec![&a, &b, &c];
        let SessionOrder { order, headers, .. } = compute_session_order(&sessions);
        let labelled: Vec<(&str, Option<String>)> = order
            .iter()
            .zip(headers.iter())
            .map(|(&i, h)| (sessions[i].name.as_str(), h.clone()))
            .collect();
        // Groups in label order ("infra" < "webapp"), header on first row only.
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
        let SessionOrder { order, headers, .. } = compute_session_order(&sessions);
        assert_eq!(order, vec![0, 1]);
        assert_eq!(headers, vec![Some("(no repo)".to_string()), None]);
    }

    #[test]
    fn multi_repo_session_forms_its_own_composite_group() {
        // A {webapp}, B {webapp, infra}, C {infra}: three distinct groups, the
        // multi-repo session is NOT folded into either single-repo group.
        let a = info_repo("a", "webapp", SessionStatus::Done);
        let b = info_repos("b", &["webapp", "infra"], SessionStatus::Done);
        let c = info_repo("c", "infra", SessionStatus::Done);
        let sessions = vec![&a, &b, &c];

        let SessionOrder { order, headers, .. } = compute_session_order(&sessions);
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
    fn display_order_overrides_insertion_within_group() {
        let mut a = info_repo("a", "webapp", SessionStatus::Idle);
        let mut b = info_repo("b", "webapp", SessionStatus::Idle);
        a.display_order = Some(1);
        b.display_order = Some(0);
        let sessions = vec![&a, &b];
        assert_eq!(order_names(&sessions), vec!["b", "a"]);
    }

    #[test]
    fn group_order_follows_min_member_display_order() {
        // "webapp" sorts after "infra" by label, but holds the lowest
        // display_order, so the webapp group renders first.
        let mut w1 = info_repo("w1", "webapp", SessionStatus::Idle);
        let mut w2 = info_repo("w2", "webapp", SessionStatus::Idle);
        let mut i1 = info_repo("i1", "infra", SessionStatus::Idle);
        w1.display_order = Some(0);
        w2.display_order = Some(1);
        i1.display_order = Some(2);
        let sessions = vec![&i1, &w1, &w2];
        assert_eq!(order_names(&sessions), vec!["w1", "w2", "i1"]);
    }

    #[test]
    fn unordered_sessions_append_after_ordered_in_insertion_order() {
        let mut moved = info_repo("moved", "webapp", SessionStatus::Idle);
        moved.display_order = Some(0);
        let new1 = info_repo("new1", "webapp", SessionStatus::Idle);
        let new2 = info_repo("new2", "webapp", SessionStatus::Idle);
        // Unordered sessions (`None`) land after the ordered one, keeping
        // their insertion order.
        let sessions = vec![&new1, &moved, &new2];
        assert_eq!(order_names(&sessions), vec!["moved", "new1", "new2"]);
    }

    // --- move_in_order ---

    /// Apply `move_in_order` and return the resulting names, or `None` on no-op.
    fn move_names<'a>(
        sessions: &[&'a SessionInfo],
        active: &str,
        down: bool,
    ) -> Option<Vec<&'a str>> {
        let ord = compute_session_order(sessions);
        let active_idx = sessions.iter().position(|s| s.name == active).unwrap();
        move_in_order(&ord, active_idx, down).map(|order| {
            order
                .into_iter()
                .map(|i| sessions[i].name.as_str())
                .collect()
        })
    }

    #[test]
    fn move_swaps_adjacent_root_blocks_within_group() {
        let a = info_repo("a", "webapp", SessionStatus::Idle);
        let b = info_repo("b", "webapp", SessionStatus::Idle);
        let c = info_repo("c", "webapp", SessionStatus::Idle);
        let sessions = vec![&a, &b, &c];
        assert_eq!(move_names(&sessions, "a", true), Some(vec!["b", "a", "c"]));
        assert_eq!(move_names(&sessions, "c", false), Some(vec!["a", "c", "b"]));
    }

    #[test]
    fn move_past_group_edge_moves_whole_group() {
        let i1 = info_repo("i1", "infra", SessionStatus::Idle);
        let i2 = info_repo("i2", "infra", SessionStatus::Idle);
        let w1 = info_repo("w1", "webapp", SessionStatus::Idle);
        let w2 = info_repo("w2", "webapp", SessionStatus::Idle);
        let sessions = vec![&i1, &i2, &w1, &w2];
        // Render order: [i1, i2, w1, w2] ("infra" < "webapp").
        // i2 is the last root block of infra: down moves the whole infra
        // group below webapp.
        assert_eq!(
            move_names(&sessions, "i2", true),
            Some(vec!["w1", "w2", "i1", "i2"])
        );
        // w1 is the first root block of webapp: up moves the whole webapp
        // group above infra.
        assert_eq!(
            move_names(&sessions, "w1", false),
            Some(vec!["w1", "w2", "i1", "i2"])
        );
    }

    #[test]
    fn move_at_list_edges_is_noop() {
        let i1 = info_repo("i1", "infra", SessionStatus::Idle);
        let w1 = info_repo("w1", "webapp", SessionStatus::Idle);
        let sessions = vec![&i1, &w1];
        // i1's group is at the top, w1's at the bottom.
        assert_eq!(move_names(&sessions, "i1", false), None);
        assert_eq!(move_names(&sessions, "w1", true), None);
    }

    #[test]
    fn moving_parent_drags_nested_children() {
        let lead = info_repo("lead", "webapp", SessionStatus::Idle);
        let w1 = info_child("w1", "webapp", SessionStatus::Idle, &lead);
        let other = info_repo("other", "webapp", SessionStatus::Idle);
        let sessions = vec![&lead, &w1, &other];
        // Render order: [lead, w1, other]; moving lead down carries w1 along.
        assert_eq!(
            move_names(&sessions, "lead", true),
            Some(vec!["other", "lead", "w1"])
        );
    }

    #[test]
    fn child_moves_among_siblings_only() {
        let lead = info_repo("lead", "webapp", SessionStatus::Idle);
        let w1 = info_child("w1", "webapp", SessionStatus::Idle, &lead);
        let w2 = info_child("w2", "webapp", SessionStatus::Idle, &lead);
        let other = info_repo("other", "webapp", SessionStatus::Idle);
        let sessions = vec![&lead, &w1, &w2, &other];
        // Render order: [lead, w1, w2, other].
        assert_eq!(
            move_names(&sessions, "w1", true),
            Some(vec!["lead", "w2", "w1", "other"])
        );
        // First/last sibling can't leave the parent.
        assert_eq!(move_names(&sessions, "w1", false), None);
        assert_eq!(move_names(&sessions, "w2", true), None);
    }

    #[test]
    fn renumbering_along_moved_order_reproduces_it() {
        // The app renumbers display_order densely along the returned order;
        // compute_session_order must then reproduce that order exactly.
        let i1 = info_repo("i1", "infra", SessionStatus::Idle);
        let i2 = info_repo("i2", "infra", SessionStatus::Idle);
        let lead = info_repo("lead", "webapp", SessionStatus::Idle);
        let w1 = info_child("w1", "webapp", SessionStatus::Idle, &lead);
        let mut sessions_owned = [i1, i2, lead, w1];

        let sessions: Vec<&SessionInfo> = sessions_owned.iter().collect();
        let ord = compute_session_order(&sessions);
        // Move i2 (last root of the top group) down: whole infra group drops.
        let active = sessions.iter().position(|s| s.name == "i2").unwrap();
        let new_order = move_in_order(&ord, active, true).unwrap();

        for (pos, &idx) in new_order.iter().enumerate() {
            sessions_owned[idx].display_order = Some(pos as i64);
        }
        let sessions: Vec<&SessionInfo> = sessions_owned.iter().collect();
        assert_eq!(order_names(&sessions), vec!["lead", "w1", "i1", "i2"]);
    }

    // --- sort_alphabetically_within_groups ---

    /// Apply `sort_alphabetically_within_groups` and return the resulting names.
    fn sort_names<'a>(sessions: &[&'a SessionInfo]) -> Vec<&'a str> {
        sort_alphabetically_within_groups(sessions)
            .into_iter()
            .map(|i| sessions[i].name.as_str())
            .collect()
    }

    #[test]
    fn sort_orders_members_alphabetically_within_a_group() {
        let c = info_repo("c", "webapp", SessionStatus::Idle);
        let a = info_repo("a", "webapp", SessionStatus::Idle);
        let b = info_repo("b", "webapp", SessionStatus::Idle);
        let sessions = vec![&c, &a, &b];
        assert_eq!(sort_names(&sessions), vec!["a", "b", "c"]);
    }

    #[test]
    fn sort_is_case_insensitive() {
        let z = info_repo("Zeta", "webapp", SessionStatus::Idle);
        let a = info_repo("apple", "webapp", SessionStatus::Idle);
        let b = info_repo("Banana", "webapp", SessionStatus::Idle);
        let sessions = vec![&z, &a, &b];
        assert_eq!(sort_names(&sessions), vec!["apple", "Banana", "Zeta"]);
    }

    #[test]
    fn sort_preserves_group_order() {
        // "webapp" holds the lowest display_order, so its group renders first.
        // Sorting within groups must NOT bubble the alphabetically-earlier
        // "infra" group above it.
        let mut w_b = info_repo("w-b", "webapp", SessionStatus::Idle);
        let mut w_a = info_repo("w-a", "webapp", SessionStatus::Idle);
        let mut i_c = info_repo("i-c", "infra", SessionStatus::Idle);
        w_b.display_order = Some(0);
        w_a.display_order = Some(1);
        i_c.display_order = Some(2);
        let sessions = vec![&i_c, &w_b, &w_a];
        // webapp group stays first, but its members are sorted A→Z.
        assert_eq!(sort_names(&sessions), vec!["w-a", "w-b", "i-c"]);
    }

    #[test]
    fn sort_keeps_children_under_their_parent_sorted_among_siblings() {
        let lead = info_repo("lead", "webapp", SessionStatus::Idle);
        // Insert children in non-alphabetical order to prove they're resorted.
        let w_b = info_child("w-b", "webapp", SessionStatus::Idle, &lead);
        let w_a = info_child("w-a", "webapp", SessionStatus::Idle, &lead);
        let sessions = vec![&w_b, &lead, &w_a];
        // Parent first (sorted among roots), children sorted A→Z under it.
        assert_eq!(sort_names(&sessions), vec!["lead", "w-a", "w-b"]);
    }

    #[test]
    fn sort_renumbers_reproduce_alphabetical_order() {
        // After densely renumbering display_order along the sorted order,
        // compute_session_order must reproduce that exact order — the
        // round-trip the App relies on for persistence.
        let mut owned = [
            info_repo("c", "webapp", SessionStatus::Idle),
            info_repo("a", "webapp", SessionStatus::Idle),
            info_repo("b", "webapp", SessionStatus::Idle),
        ];
        let sessions: Vec<&SessionInfo> = owned.iter().collect();
        let new_order = sort_alphabetically_within_groups(&sessions);
        for (pos, &idx) in new_order.iter().enumerate() {
            owned[idx].display_order = Some(pos as i64);
        }
        let sessions: Vec<&SessionInfo> = owned.iter().collect();
        assert_eq!(order_names(&sessions), vec!["a", "b", "c"]);
    }

    #[test]
    fn sort_empty_input_returns_empty() {
        let sessions: Vec<&SessionInfo> = vec![];
        assert!(sort_alphabetically_within_groups(&sessions).is_empty());
    }

    // --- compute_session_order (parent/child nesting) ---

    #[test]
    fn children_nest_directly_under_their_parent() {
        let lead = info_repo("lead", "webapp", SessionStatus::Idle);
        let other = info_repo("other", "webapp", SessionStatus::Idle);
        let w1 = info_child("w1", "webapp", SessionStatus::Idle, &lead);
        let w2 = info_child("w2", "webapp", SessionStatus::Idle, &lead);
        // Input interleaves the children with an unrelated session; they still
        // nest directly under their parent, in stable order.
        let sessions = vec![&lead, &other, &w1, &w2];
        assert_eq!(
            order_names_depths(&sessions),
            vec![("lead", 0), ("w1", 1), ("w2", 1), ("other", 0)]
        );
    }

    #[test]
    fn grandchildren_nest_one_level_deeper() {
        let lead = info_repo("lead", "webapp", SessionStatus::Idle);
        let worker = info_child("worker", "webapp", SessionStatus::Idle, &lead);
        let sub = info_child("sub", "webapp", SessionStatus::Idle, &worker);
        let sessions = vec![&sub, &worker, &lead];
        assert_eq!(
            order_names_depths(&sessions),
            vec![("lead", 0), ("worker", 1), ("sub", 2)]
        );
    }

    #[test]
    fn attention_child_does_not_move_its_group() {
        let lead = info_repo("lead", "webapp", SessionStatus::Idle);
        let worker = info_child("worker", "webapp", SessionStatus::Blocked, &lead);
        let busy = info_repo("busy", "infra", SessionStatus::Working);
        let sessions = vec![&busy, &lead, &worker];
        // The child's Blocked status doesn't bubble its group: groups stay
        // in label order ("infra" < "webapp"); the child stays nested.
        assert_eq!(
            order_names_depths(&sessions),
            vec![("busy", 0), ("lead", 0), ("worker", 1)]
        );
    }

    #[test]
    fn cross_group_child_stays_in_its_own_group_at_depth_zero() {
        let lead = info_repo("lead", "webapp", SessionStatus::Idle);
        let worker = info_child("worker", "infra", SessionStatus::Idle, &lead);
        let sessions = vec![&lead, &worker];
        // Parent lives in another repo group: no reordering, no indentation.
        assert_eq!(
            order_names_depths(&sessions),
            vec![("worker", 0), ("lead", 0)] // "infra" < "webapp"
        );
    }

    #[test]
    fn dangling_parent_renders_child_as_root() {
        let gone = info_repo("gone", "webapp", SessionStatus::Idle);
        let orphan = info_child("orphan", "webapp", SessionStatus::Idle, &gone);
        let sessions = vec![&orphan]; // parent not in the list
        assert_eq!(order_names_depths(&sessions), vec![("orphan", 0)]);
    }

    #[test]
    fn parent_cycle_emits_all_members_flat() {
        // Cycles can't be produced by current writers; corrupted data must
        // still render every session.
        let mut a = info_repo("a", "webapp", SessionStatus::Idle);
        let mut b = info_repo("b", "webapp", SessionStatus::Idle);
        a.parent_session_id = Some(b.id);
        b.parent_session_id = Some(a.id);
        let sessions = vec![&a, &b];
        assert_eq!(order_names_depths(&sessions), vec![("a", 0), ("b", 0)]);
    }

    #[test]
    fn depths_are_parallel_to_order_and_headers() {
        let lead = info_repo("lead", "webapp", SessionStatus::Idle);
        let worker = info_child("worker", "webapp", SessionStatus::Idle, &lead);
        let other = info_repo("other", "infra", SessionStatus::Idle);
        let sessions = vec![&lead, &worker, &other];
        let SessionOrder {
            order,
            headers,
            depths,
        } = compute_session_order(&sessions);
        assert_eq!(order.len(), 3);
        assert_eq!(headers.len(), 3);
        assert_eq!(depths.len(), 3);
    }

    #[test]
    fn duplicate_repos_collapse_to_one_group() {
        // A repo set with the same repo twice (e.g. two worktrees of one repo)
        // groups under that single repo, alongside a plain single-repo session.
        let multi = info_repos("multi", &["webapp", "webapp"], SessionStatus::Done);
        let single = info_repo("single", "webapp", SessionStatus::Done);
        let sessions = vec![&multi, &single];

        let SessionOrder { order, headers, .. } = compute_session_order(&sessions);
        // Same canonical key → one group; header is the de-duplicated display.
        assert_eq!(order, vec![0, 1]);
        assert_eq!(headers, vec![Some("webapp".to_string()), None]);
    }

    #[test]
    fn multi_repo_grouping_is_order_independent() {
        // Same repo *set*, different selection order → one group; the header
        // reflects the first session's natural order.
        let b = info_repos("b", &["webapp", "infra"], SessionStatus::Done);
        let d = info_repos("d", &["infra", "webapp"], SessionStatus::Done);
        let sessions = vec![&b, &d];

        let SessionOrder { order, headers, .. } = compute_session_order(&sessions);
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
