pub mod agent_picker_modal;
pub mod automation_detail;
pub mod automation_editor_modal;
pub mod automations_list_modal;
pub mod automations_panel;
pub mod branch_selector_modal;
pub mod code_review;
pub mod confirm_delete_modal;
pub mod confirm_restore_modal;
pub mod file_viewer;
pub mod global_search;
pub mod highlight;
pub mod host_picker_modal;
pub mod info_panel;
pub mod layout;
pub mod links;
pub mod markdown;
pub mod project_list;
pub mod repo_picker_modal;
pub mod restore_sessions_modal;
pub mod scrollbar;
pub mod selection;
pub mod session_name_modal;
pub mod settings_modal;
pub mod status_bar;
pub mod syntax;
pub mod task_action_picker_modal;
pub mod task_detail;
pub mod task_editor_modal;
pub mod tasks_panel;
pub mod terminal_view;
pub mod theme;
pub mod theme_picker_modal;
pub mod worktree_name_modal;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::session::SessionStatus;
use theme::Theme;

/// One clickable row rendered this frame: its rect (full row width) plus the
/// row's index in whatever list the renderer drew. Pure geometry — the app
/// layer decides what the index means (mirrors how `ScrollbarGeom` is wrapped
/// into `ScrollTarget` hits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowHitbox {
    pub rect: Rect,
    pub index: usize,
}

/// Build one `RowHitbox` per single-line entry of a vertically packed list that
/// has been windowed to the visible range `start..end` — the common shape for
/// the tasks/automations panes. The first visible entry (`start`) is drawn at
/// `area`'s top row; each hitbox carries its index in *entry* space (not row
/// space) so callers map a click straight back to the entry. Rows that would
/// fall below `area` are clipped.
pub fn windowed_row_hitboxes(area: Rect, start: usize, end: usize) -> Vec<RowHitbox> {
    (start..end)
        .take(area.height as usize)
        .enumerate()
        .map(|(row, index)| RowHitbox {
            rect: Rect::new(area.x, area.y + row as u16, area.width, 1),
            index,
        })
        .collect()
}

/// Row hitboxes + optional scrollbar geometry returned by the selector-modal
/// renderers (and the F1 help overlay).
pub type SelectorHits = (Vec<RowHitbox>, Option<scrollbar::ScrollbarGeom>);

/// What a selector/editor modal renderer returns: its [`SelectorHits`] (rows +
/// scrollbar) plus the clickable footer buttons paired with their replay keys.
pub type ModalRender = (SelectorHits, ModalButtons);

/// Render a single-line-per-row selector list into `area`. When the entries
/// overflow, the list windows around `selected` (like the file viewer) and a
/// scrollbar is drawn in the reserved rightmost column. Returns the visible
/// rows' hitboxes (indexed in entry space) plus the scrollbar geometry
/// (`None` when everything fits).
pub fn render_selector_rows(
    frame: &mut Frame,
    area: Rect,
    lines: Vec<Line<'_>>,
    selected: usize,
) -> SelectorHits {
    let total = lines.len();
    let height = area.height as usize;
    if total == 0 || height == 0 || area.width == 0 {
        return (Vec::new(), None);
    }
    let selected = selected.min(total - 1);
    let (rows_area, track) = scrollbar::reserve_track(area, total, height);
    let (start, end) = file_viewer::visible_window(total, selected, height);
    let visible: Vec<Line<'_>> = lines.into_iter().skip(start).take(end - start).collect();
    frame.render_widget(Paragraph::new(visible), rows_area);
    let hitboxes = windowed_row_hitboxes(rows_area, start, end);
    let geom = track.and_then(|t| scrollbar::render_into(frame, t, total, height, selected));
    (hitboxes, geom)
}

/// One button to render in a [`render_button_bar`] row: its visible `label`,
/// whether it is the primary/affirmative action (an accent-filled pill vs the
/// neutral selection-filled pill), and an optional `hint` — a keyboard shortcut
/// suffix (e.g. `·c`) rendered dimmed within the same chip so the key reads as a
/// subordinate annotation, not part of the label. Pure presentation — the app
/// layer maps the rendered hitbox's index back to an action/key.
#[derive(Debug, Clone, Copy)]
pub struct ButtonSpec<'a> {
    pub label: &'a str,
    pub primary: bool,
    pub hint: Option<&'a str>,
}

impl<'a> ButtonSpec<'a> {
    pub fn primary(label: &'a str) -> Self {
        Self {
            label,
            primary: true,
            hint: None,
        }
    }

    pub fn secondary(label: &'a str) -> Self {
        Self {
            label,
            primary: false,
            hint: None,
        }
    }

    /// Attach a dimmed keyboard-shortcut suffix to this button (e.g. `"·c"`).
    pub fn with_hint(mut self, hint: &'a str) -> Self {
        self.hint = Some(hint);
        self
    }
}

/// A rendered button's hitbox: the rect it occupies plus its index in the
/// `specs` slice the caller passed to [`render_button_bar`]. Pure geometry —
/// the app layer maps the index back to a key/action (mirrors [`RowHitbox`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonHit {
    pub rect: Rect,
    pub index: usize,
}

/// Footer button hitboxes paired with the key each replays when clicked,
/// returned by the modal renderers so the index→key map stays colocated with
/// the modal. `view.rs` records each as a `ClickAction::ModalButton`.
pub type ModalButtons = Vec<(
    ButtonHit,
    crossterm::event::KeyCode,
    crossterm::event::KeyModifiers,
)>;

/// Width (display columns) a button occupies when rendered as a pill chip: the
/// label plus its optional hint suffix, with one space of padding on each side
/// (` label·key `).
fn button_width(spec: &ButtonSpec<'_>) -> u16 {
    let hint = spec.hint.map_or(0, |h| h.chars().count());
    (spec.label.chars().count() + hint) as u16 + 2
}

/// The resting fill style for a button — a filled "pill" chip. Primary actions
/// use the accent colour (a focused-badge look); secondary actions the
/// neutral, contrast-tuned selection pair (`selection_fg` on `selection_bg`),
/// which every palette guarantees is legible — unlike the old `inverted_fg` on
/// `text_muted`, where `inverted_fg` tracks the app background and so collapsed
/// to dark-on-dark (dark themes) / light-on-light (light themes) over the muted
/// mid-gray. The space-padded label on a solid background reads as a button
/// without brackets, and the fill is what the hover highlight brightens.
fn button_style(primary: bool) -> Style {
    if primary {
        Style::default()
            .fg(Theme::inverted_fg())
            .bg(Theme::accent())
            .add_modifier(ratatui::style::Modifier::BOLD)
    } else {
        Style::default()
            .fg(Theme::selection_fg())
            .bg(Theme::selection_bg())
            .add_modifier(ratatui::style::Modifier::BOLD)
    }
}

/// Render a row of filled "pill" buttons (` label `, padded, on a solid accent
/// or neutral selection fill) into the single-row `area`, returning one
/// [`ButtonHit`] per *placed* button (index = position in `specs`). Buttons are
/// separated by one space. When `right_align` is set the row is packed against
/// the right edge of `area` (the convention for modal Save/Cancel and the global
/// footer); otherwise it starts at the left edge.
///
/// Responsive by design: a button that would overflow `area` is dropped rather
/// than wrapped or clipped mid-glyph, so a narrow footer simply shows fewer
/// buttons. The solid fill (not brackets) is what marks each label as a
/// clickable button.
/// Paint a single filled pill button (` label `, padded, on the solid
/// primary/secondary fill) into `rect` — the standalone form of the chips
/// [`render_button_bar`] packs, for callers that own their own layout (e.g. the
/// central-pane tab strip painted on the pane border). `rect.width` should be
/// the label width plus the two padding cells (` label `) so the fill spans the
/// chip.
pub fn render_pill(frame: &mut Frame, rect: Rect, label: &str, primary: bool) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {label} "),
            button_style(primary),
        ))),
        rect,
    );
}

pub fn render_button_bar(
    frame: &mut Frame,
    area: Rect,
    specs: &[ButtonSpec<'_>],
    right_align: bool,
) -> Vec<ButtonHit> {
    if area.height == 0 || area.width == 0 || specs.is_empty() {
        return Vec::new();
    }
    // Total width of every button plus single-space separators.
    let total: u16 =
        specs.iter().map(button_width).sum::<u16>() + specs.len().saturating_sub(1) as u16;

    let mut x = if right_align && total <= area.width {
        area.x + area.width - total
    } else {
        area.x
    };
    let limit = area.x + area.width;

    let mut hits = Vec::with_capacity(specs.len());
    let mut dropped = false;
    for (index, spec) in specs.iter().enumerate() {
        let width = button_width(spec);
        if x + width > limit {
            dropped = true; // out of room — drop the rest rather than corrupt the row
            break;
        }
        let rect = Rect::new(x, area.y, width, 1);
        let base = button_style(spec.primary);
        // The chip is ` label ` (no hint) or ` label·key ` with the hint dimmed
        // on the same fill so the shortcut reads as a subordinate annotation.
        let line = match spec.hint {
            Some(hint) => Line::from(vec![
                Span::styled(format!(" {}", spec.label), base),
                Span::styled(
                    format!("{hint} "),
                    base.add_modifier(ratatui::style::Modifier::DIM),
                ),
            ]),
            None => Line::from(Span::styled(format!(" {} ", spec.label), base)),
        };
        frame.render_widget(Paragraph::new(line), rect);
        hits.push(ButtonHit { rect, index });
        x += width + 1;
    }
    // When a left-aligned bar (the review/global footer) drops buttons, mark the
    // overflow with a muted `…` in the leftover space so the hidden actions are
    // discoverable rather than silently gone. Right-aligned modal footers pack
    // one or two buttons and effectively never overflow, so they keep their
    // clean trailing edge.
    if dropped && !right_align && x < limit {
        let avail = limit - x;
        let marker = if avail >= 3 { " … " } else { "…" };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                marker,
                Style::default().fg(Theme::text_muted()),
            ))),
            Rect::new(x, area.y, avail.min(marker.chars().count() as u16), 1),
        );
    }
    hits
}

/// Map button-bar hitboxes to the keys they replay, by index. Pairs each
/// [`ButtonHit`] with `(code, mods)` from `keys[hit.index]` to build the
/// [`ModalButtons`] the modal renderers return.
pub fn modal_button_keys(
    hits: Vec<ButtonHit>,
    keys: &[(crossterm::event::KeyCode, crossterm::event::KeyModifiers)],
) -> ModalButtons {
    hits.into_iter()
        .filter_map(|h| keys.get(h.index).map(|&(code, mods)| (h, code, mods)))
        .collect()
}

/// Render a right-aligned two-button modal footer into the single-row `area`:
/// a `primary` action `(label, key)` plus a secondary Cancel/Close button that
/// always replays `Esc`. Returns the buttons paired with their replay keys (see
/// [`ModalButtons`]) — the shared shape behind every editor/confirm/name modal
/// footer.
pub fn render_action_footer(
    frame: &mut Frame,
    area: Rect,
    primary: (
        &str,
        crossterm::event::KeyCode,
        crossterm::event::KeyModifiers,
    ),
    secondary_label: &str,
) -> ModalButtons {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (primary_label, code, mods) = primary;
    let hits = render_button_bar(
        frame,
        area,
        &[
            ButtonSpec::primary(primary_label),
            ButtonSpec::secondary(secondary_label),
        ],
        true,
    );
    modal_button_keys(hits, &[(code, mods), (KeyCode::Esc, KeyModifiers::NONE)])
}

/// Render the standard selector-modal footer into the single-row `area`: a
/// left-aligned `j/k navigate` hint plus right-aligned `[ Select ]` (Enter) /
/// `[ Cancel ]` (Esc) buttons. Returns the buttons paired with their replay
/// keys (see [`ModalButtons`]). Shared by the picker modals so their clickable
/// footer reads identically.
pub fn render_selector_footer(frame: &mut Frame, area: Rect) -> ModalButtons {
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("j/k", Theme::keybind()),
            Span::styled(" navigate", Theme::keybind_desc()),
        ])),
        area,
    );
    render_action_footer(
        frame,
        area,
        (
            "Select",
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ),
        "Cancel",
    )
}

pub fn status_color(status: SessionStatus) -> Color {
    match status {
        SessionStatus::Working => Theme::status_working(),
        SessionStatus::Blocked => Theme::status_blocked(),
        SessionStatus::Done => Theme::status_done(),
        SessionStatus::Idle => Theme::status_idle(),
        SessionStatus::Error => Theme::status_error(),
    }
}

/// Braille spinner frames for the `Working` status, animated in the live session
/// list (`App::spinner_frame` advances them). Ten frames at ~8 fps reads as a
/// smooth "in progress" spinner.
pub const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The glyph to render for `status`: the animated spinner frame `spinner` while
/// `Working`, otherwise the status's static [`SessionStatus::icon`]. `spinner`
/// is the caller's current frame (e.g. `SPINNER_FRAMES[app.spinner_frame()]`).
pub fn status_glyph(status: SessionStatus, spinner: &str) -> &str {
    if status == SessionStatus::Working {
        spinner
    } else {
        status.icon()
    }
}

/// Truncate `s` to at most `max` display columns, appending `…` when cut.
///
/// Counts by `char` (not bytes), reserving one column for the ellipsis.
/// Returns an empty string when `max` is too small to show anything useful
/// (`max <= 1`), since a lone `…` carries no information.
pub fn truncate_ellipsis(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max <= 1 {
        return String::new();
    }
    let kept: String = s.chars().take(max - 1).collect();
    format!("{kept}…")
}

/// Render a titled, bordered editor frame and return its inner area. Uses the
/// shared [`focus_block`] chrome so a focused editor is highlighted exactly like
/// the session list / tasks panel (bright accent border + highlighted title
/// badge); unfocused it reads as a muted preview. Shared by the automation and
/// task in-pane editors so their chrome stays identical.
pub fn render_editor_frame(frame: &mut Frame, area: Rect, title: &str, focused: bool) -> Rect {
    let level = if focused {
        FocusLevel::Focused
    } else {
        FocusLevel::Inactive
    };
    let title = format!(" {title} ");
    let block = focus_block(&title, level);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

/// Render one editor field row: a left-aligned `label`, then its `value`. When
/// `active` the row is prefixed with `▸` and bolded; `selector` values (adjusted
/// with ←/→) are wrapped in `‹ ›`, while an active text value gets a block
/// cursor. Shared by the automation and task editor field renderers.
///
/// This convenience form draws the cursor at the end of the value; use
/// [`editor_field_line_with_cursor`] to place it at a specific caret position.
pub fn editor_field_line<'a>(label: &str, value: String, selector: bool, active: bool) -> Line<'a> {
    editor_field_line_with_cursor(label, value, selector, active, None)
}

/// Cursor-aware variant of [`editor_field_line`]. See its docs for `cursor`.
pub fn editor_field_line_with_cursor<'a>(
    label: &str,
    value: String,
    selector: bool,
    active: bool,
    cursor: Option<usize>,
) -> Line<'a> {
    let prefix = if active { "▸ " } else { "  " };
    let value_style = if active {
        Style::default()
            .fg(Theme::border_focused())
            .add_modifier(ratatui::style::Modifier::BOLD)
    } else {
        Style::default().fg(Theme::text_primary())
    };

    let mut spans = vec![Span::styled(format!("{prefix}{label:<9}"), Theme::label())];

    if selector {
        spans.push(Span::styled(format!("‹ {value} ›"), value_style));
    } else if active {
        push_value_with_cursor(&mut spans, &value, cursor, value_style);
    } else {
        spans.push(Span::styled(value, value_style));
    }

    Line::from(spans)
}

/// Push a block-cursor text run into `spans`: the text before the caret styled
/// with `style`, the character under the caret in [`Theme::cursor`] (a single
/// space when the caret sits at end-of-text), then the text after it in `style`.
///
/// The single source of truth for the editor block-cursor split — every editor
/// (and the scroll-windowing path) wraps this rather than re-deriving the
/// `caret + 1..` slice (an easy off-by-one).
pub(crate) fn push_block_cursor<'a>(
    spans: &mut Vec<Span<'a>>,
    chars: &[char],
    caret: usize,
    style: Style,
) {
    let caret = caret.min(chars.len());
    let before: String = chars[..caret].iter().collect();
    if !before.is_empty() {
        spans.push(Span::styled(before, style));
    }
    let cursor_char = chars
        .get(caret)
        .map(|c| c.to_string())
        .unwrap_or_else(|| " ".to_string());
    spans.push(Span::styled(cursor_char, Theme::cursor()));
    let after: String = chars
        .get(caret + 1..)
        .map(|s| s.iter().collect())
        .unwrap_or_default();
    if !after.is_empty() {
        spans.push(Span::styled(after, style));
    }
}

/// Append `value` to `spans` with a real block cursor drawn at the caret
/// position so horizontal movement inside the text is visible. Falls back to a
/// trailing block when no cursor is supplied (preserves the prior end-of-line
/// affordance).
fn push_value_with_cursor<'a>(
    spans: &mut Vec<Span<'a>>,
    value: &str,
    cursor: Option<usize>,
    value_style: Style,
) {
    let chars: Vec<char> = value.chars().collect();
    let caret = cursor.unwrap_or(chars.len());
    push_block_cursor(spans, &chars, caret, value_style);
}

/// Build a footer/hint [`Line`] from `(key, description)` pairs, styling keys
/// with [`Theme::keybind`] and descriptions with [`Theme::keybind_desc`]. Shared
/// by the editor footers so the keybind chrome reads identically everywhere.
pub fn key_hint_line<'a>(pairs: &[(&'a str, &'a str)]) -> Line<'a> {
    let mut spans = Vec::with_capacity(pairs.len() * 2);
    for (key, desc) in pairs {
        spans.push(Span::styled(*key, Theme::keybind()));
        spans.push(Span::styled(*desc, Theme::keybind_desc()));
    }
    Line::from(spans)
}

/// Tri-state focus level for panels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusLevel {
    /// Receiving input: thick accent border + badge title.
    Focused,
    /// Contextually relevant: plain accent border + accent title text.
    Active,
    /// Background: plain dark-gray border + dark-gray title.
    Inactive,
}

/// Build a [`Block`] with tri-state focus styling.
///
/// Focus is communicated by colour (bright accent vs plain accent vs gray)
/// rather than border weight — every level uses rounded borders for a
/// softer, opencode-style chrome.
/// The title text style for a pane at the given focus level — shared by
/// [`focus_block`] and callers that render a pane title themselves (e.g. a
/// right-aligned session-info title alongside the central-pane tab strip).
pub fn title_style(level: FocusLevel) -> Style {
    match level {
        FocusLevel::Focused => Theme::focused_title(),
        FocusLevel::Active => Style::default().fg(Theme::accent()),
        FocusLevel::Inactive => Theme::unfocused_title(),
    }
}

/// The border style for a pane at the given focus level.
fn border_style_for(level: FocusLevel) -> Style {
    match level {
        FocusLevel::Focused => Style::default().fg(Theme::accent_bright()),
        FocusLevel::Active => Style::default().fg(Theme::accent()),
        FocusLevel::Inactive => Style::default().fg(Theme::border_unfocused()),
    }
}

pub fn focus_block(title_text: &str, level: FocusLevel) -> Block<'_> {
    Block::default()
        .title(Line::from(Span::styled(title_text, title_style(level))))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style_for(level))
}

/// Build a [`Block`] with focused or unfocused styling (backward compat).
///
/// Focused: thick borders in accent color with a highlighted title badge.
/// Unfocused: plain borders in gray with a dimmed title.
pub fn focused_block(title_text: &str, focused: bool) -> Block<'_> {
    focus_block(
        title_text,
        if focused {
            FocusLevel::Focused
        } else {
            FocusLevel::Inactive
        },
    )
}

/// Create a centered rectangle with a fixed width percentage and a fixed height in lines.
pub fn centered_fixed_height_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

/// Render a full-screen dim overlay to visually separate a modal from the background.
pub fn render_dim_overlay(frame: &mut Frame) {
    let dim = Block::default().style(Style::default().bg(Theme::modal_dim_bg()));
    frame.render_widget(dim, frame.area());
}

/// Build a modal [`Block`] with the given title style and border color.
fn build_modal_block(title: &str, title_style: Style, border_color: Color) -> Block<'_> {
    Block::default()
        .title(Line::from(Span::styled(format!(" {title} "), title_style)))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(Theme::modal_bg()))
}

/// Build a styled modal [`Block`] with rounded borders and an explicit background.
pub fn modal_block(title: &str) -> Block<'_> {
    build_modal_block(title, Theme::modal_title(), Theme::modal_border())
}

/// Build a danger-styled modal [`Block`] with red borders and background.
pub fn modal_block_danger(title: &str) -> Block<'_> {
    build_modal_block(title, Theme::modal_title_danger(), Theme::danger())
}

/// Dim the background, clear the modal region, render a styled block, and return the inner area.
pub fn render_modal_frame(frame: &mut Frame, area: Rect, title: &str) -> Rect {
    render_dim_overlay(frame);
    frame.render_widget(Clear, area);
    let block = modal_block(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

/// Dim the background, clear the modal region, render a danger-styled block, and return the inner area.
pub fn render_modal_frame_danger(frame: &mut Frame, area: Rect, title: &str) -> Rect {
    render_dim_overlay(frame);
    frame.render_widget(Clear, area);
    let block = modal_block_danger(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

/// Render a centered yes/no confirmation modal: the `body` lines, a blank
/// spacer, and an action footer (`confirm` label/key + a `Cancel` button).
/// `danger` picks the red-bordered frame. Returns the footer buttons. Shared by
/// the delete / restore confirmations so their scaffold reads identically.
pub fn render_confirm_modal(
    frame: &mut Frame,
    width_pct: u16,
    title: &str,
    danger: bool,
    body: Vec<Line>,
    confirm: (
        &str,
        crossterm::event::KeyCode,
        crossterm::event::KeyModifiers,
    ),
) -> ModalButtons {
    // Outer height: the body lines + a blank spacer + the footer row, plus the
    // top/bottom border (2).
    let height = body.len() as u16 + 2 + 2;
    let area = centered_fixed_height_rect(width_pct, height, frame.area());
    let inner = if danger {
        render_modal_frame_danger(frame, area, title)
    } else {
        render_modal_frame(frame, area, title)
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(body.len() as u16),
            Constraint::Length(1), // blank spacer
            Constraint::Length(1), // footer
        ])
        .split(inner);

    frame.render_widget(Paragraph::new(body), chunks[0]);
    render_action_footer(frame, chunks[2], confirm, "Cancel")
}

/// Set up a list modal with dim overlay, styled border, and list + footer split.
///
/// If `entry_count` is 0 and `empty_message` is `Some`, renders the empty state
/// with the given message and footer keybinds, then returns `None`.
/// Otherwise returns `Some([list_area, footer_area])`.
pub fn render_list_modal_frame<'a>(
    frame: &mut Frame,
    percent_width: u16,
    title: &str,
    entry_count: usize,
    empty_message: Option<&str>,
    empty_footer: Option<Line<'a>>,
) -> Option<[Rect; 2]> {
    let list_height = entry_count.max(1) as u16;
    let total_height = (list_height + 5).min(20);
    let area = centered_fixed_height_rect(percent_width, total_height, frame.area());
    let inner = render_modal_frame(frame, area, title);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    if entry_count == 0 {
        if let Some(msg) = empty_message {
            let empty = Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(Theme::text_muted()),
            )))
            .alignment(ratatui::layout::Alignment::Center);
            frame.render_widget(empty, chunks[0]);

            if let Some(footer) = empty_footer {
                frame.render_widget(Paragraph::new(footer), chunks[1]);
            }
        }
        return None;
    }

    Some([chunks[0], chunks[1]])
}

/// Standard "j/k navigate · Enter select · Esc cancel" footer used by selector
/// modals.
pub fn selector_nav_footer() -> Line<'static> {
    Line::from(vec![
        Span::styled("j/k", Theme::keybind()),
        Span::styled(" navigate  ", Theme::keybind_desc()),
        Span::styled("Enter", Theme::keybind()),
        Span::styled(" select  ", Theme::keybind_desc()),
        Span::styled("Esc", Theme::keybind()),
        Span::styled(" cancel", Theme::keybind_desc()),
    ])
}

/// Build a selector row line with the standard "▸ " selected prefix and
/// selected/normal theme styles.
pub fn selector_line<'a>(label: &str, selected: bool) -> Line<'a> {
    let style = if selected {
        Theme::selected_item()
    } else {
        Theme::normal_item()
    };
    let prefix = if selected { "▸ " } else { "  " };
    Line::from(Span::styled(format!("{prefix}{label}"), style))
}

/// Build a selector list item with the standard "▸ " selected prefix and
/// selected/normal theme styles.
pub fn selector_list_item<'a>(label: &str, selected: bool) -> ratatui::widgets::ListItem<'a> {
    ratatui::widgets::ListItem::new(selector_line(label, selected))
}

/// Render a labeled text input field with cursor visualization and horizontal
/// viewport scrolling.
///
/// When `focused` is true, a block cursor is shown at the current position.
/// If the text exceeds the visible width, the viewport scrolls to keep the
/// cursor visible and overflow indicators (`◀` / `▶`) are shown at the edges.
/// When unfocused, the value is displayed as plain text with a dimmed border.
pub fn render_text_field(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    value: &str,
    cursor: usize,
    focused: bool,
) {
    render_text_field_with_suggestion(frame, area, label, value, cursor, focused, None);
}

/// Render a text field with an optional inline suggestion (fish-style).
///
/// When `focused`, cursor at end, and `suggestion` is `Some`, the suggestion
/// text is rendered in dark gray after the cursor block. Pass `None` for a
/// plain text field (identical to [`render_text_field`]).
pub fn render_text_field_with_suggestion(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    value: &str,
    cursor: usize,
    focused: bool,
    suggestion: Option<&str>,
) {
    let border_color = if focused {
        Theme::border_focused()
    } else {
        Theme::border_unfocused()
    };

    let block = Block::default()
        .title(format!(" {label} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);
    let width = inner.width as usize;

    let chars: Vec<char> = value.chars().collect();
    let cursor = cursor.min(chars.len());

    let display = if focused && width > 0 {
        let suggestion_text = if cursor == chars.len() {
            suggestion.unwrap_or("")
        } else {
            ""
        };
        render_focused_field_line(&chars, cursor, width, suggestion_text)
    } else if width > 0 {
        render_unfocused_field_line(value, &chars, width)
    } else {
        Line::from("")
    };

    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(display), inner);
}

/// Computed scroll viewport for a focused text field.
struct Viewport {
    /// First character index of the scrolled-in viewport (before overflow trim).
    start: usize,
    has_left_overflow: bool,
    has_right_overflow: bool,
}

/// Compute the scroll viewport (and overflow indicators) for a focused field.
fn compute_viewport(chars_len: usize, width: usize, cursor: usize) -> Viewport {
    if chars_len < width {
        return Viewport {
            start: 0,
            has_left_overflow: false,
            has_right_overflow: false,
        };
    }
    let usable = width.saturating_sub(1);
    let start = if cursor < usable {
        0
    } else {
        cursor - usable + 1
    };
    Viewport {
        start,
        has_left_overflow: start > 0,
        has_right_overflow: start + width < chars_len + 1,
    }
}

/// Build the rendered line for a focused text field (with cursor block and
/// optional inline suggestion / overflow indicators).
fn render_focused_field_line(
    chars: &[char],
    cursor: usize,
    width: usize,
    suggestion_text: &str,
) -> Line<'static> {
    let vp = compute_viewport(chars.len(), width, cursor);

    let content_start = if vp.has_left_overflow {
        vp.start + 1
    } else {
        vp.start
    };
    let content_width = width
        - if vp.has_left_overflow { 1 } else { 0 }
        - if vp.has_right_overflow { 1 } else { 0 };

    let mut spans = Vec::new();

    if vp.has_left_overflow {
        spans.push(Span::styled("◀", Style::default().fg(Theme::text_muted())));
    }

    let visible_end = (content_start + content_width).min(chars.len());

    if cursor >= content_start && cursor <= visible_end {
        push_cursor_spans(
            &mut spans,
            chars,
            content_start,
            cursor,
            visible_end,
            content_width,
            vp.has_left_overflow,
            suggestion_text,
        );
    } else {
        let visible: String = chars[content_start..visible_end].iter().collect();
        spans.push(Span::styled(
            visible,
            Style::default().fg(Theme::text_primary()),
        ));
    }

    if vp.has_right_overflow {
        spans.push(Span::styled("▶", Style::default().fg(Theme::text_muted())));
    }

    Line::from(spans)
}

/// Push the before/cursor/after text spans and an optional trailing suggestion
/// for the segment of the field that contains the cursor.
#[allow(clippy::too_many_arguments)]
fn push_cursor_spans(
    spans: &mut Vec<Span<'static>>,
    chars: &[char],
    content_start: usize,
    cursor: usize,
    visible_end: usize,
    content_width: usize,
    has_left_overflow: bool,
    suggestion_text: &str,
) {
    let window = &chars[content_start..visible_end];
    let caret = cursor - content_start;
    push_block_cursor(
        spans,
        window,
        caret,
        Style::default().fg(Theme::text_primary()),
    );

    if suggestion_text.is_empty() {
        return;
    }
    // Byte length of the rendered "after" text, used to budget the suggestion
    // tail against the remaining visible width.
    let after_len = window
        .get(caret + 1..)
        .map(|s| s.iter().collect::<String>().len())
        .unwrap_or(0);
    let used = if has_left_overflow { 1 } else { 0 }
        + (cursor - content_start)
        + 1 // cursor block
        + after_len;
    let remaining = content_width.saturating_sub(used);
    if remaining == 0 {
        return;
    }
    let sug: String = suggestion_text.chars().take(remaining).collect();
    if !sug.is_empty() {
        spans.push(Span::styled(sug, Style::default().fg(Theme::text_muted())));
    }
}

/// Build the rendered line for an unfocused text field (plain text, truncated
/// with an ellipsis when it exceeds the visible width).
fn render_unfocused_field_line(value: &str, chars: &[char], width: usize) -> Line<'static> {
    if chars.len() > width {
        let truncated: String = chars[..width - 1].iter().collect();
        return Line::from(vec![
            Span::styled(truncated, Style::default().fg(Theme::text_primary())),
            Span::styled("…", Style::default().fg(Theme::text_muted())),
        ]);
    }
    Line::from(Span::styled(
        value.to_string(),
        Style::default().fg(Theme::text_primary()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(width: u16, height: u16) -> Rect {
        Rect::new(0, 0, width, height)
    }

    #[test]
    fn windowed_row_hitboxes_from_top_clip_to_area() {
        let rows = windowed_row_hitboxes(Rect::new(2, 5, 10, 3), 0, 5);
        assert_eq!(rows.len(), 3, "rows below the area are clipped");
        assert_eq!(rows[0].rect, Rect::new(2, 5, 10, 1));
        assert_eq!(rows[2].rect, Rect::new(2, 7, 10, 1));
        assert_eq!(rows[2].index, 2);
        assert!(windowed_row_hitboxes(Rect::new(0, 0, 10, 5), 0, 0).is_empty());
    }

    #[test]
    fn windowed_row_hitboxes_offset_maps_rows_to_entry_indices() {
        // A window scrolled past the top: the first visible entry (3) draws at
        // the area's top row, and each hitbox keeps its entry-space index.
        let rows = windowed_row_hitboxes(Rect::new(1, 1, 10, 4), 3, 7);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].rect, Rect::new(1, 1, 10, 1));
        assert_eq!(rows[0].index, 3);
        assert_eq!(rows[3].rect, Rect::new(1, 4, 10, 1));
        assert_eq!(rows[3].index, 6);
    }

    #[test]
    fn render_selector_rows_windows_overflow_with_scrollbar() {
        let backend = ratatui::backend::TestBackend::new(40, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = Rect::new(5, 2, 20, 4);

                // Fits: all rows hit, full width, no scrollbar.
                let lines: Vec<Line> = (0..3)
                    .map(|i| selector_line(&format!("r{i}"), false))
                    .collect();
                let (rows, geom) = render_selector_rows(f, area, lines, 0);
                assert_eq!(rows.len(), 3);
                assert_eq!(rows[0].rect, Rect::new(5, 2, 20, 1));
                assert!(geom.is_none());

                // Overflows: windowed around the selection, scrollbar in the
                // reserved rightmost column, indices in entry space.
                let lines: Vec<Line> = (0..10)
                    .map(|i| selector_line(&format!("r{i}"), i == 8))
                    .collect();
                let (rows, geom) = render_selector_rows(f, area, lines, 8);
                assert_eq!(rows.len(), 4, "only the visible window is clickable");
                assert!(rows.iter().any(|r| r.index == 8), "selection stays visible");
                let geom = geom.expect("overflowing list draws a scrollbar");
                assert_eq!(geom.track, Rect::new(24, 2, 1, 4));
                assert!(
                    rows.iter().all(|r| r.rect.width == 19),
                    "rows exclude the track column"
                );
            })
            .unwrap();
    }

    #[test]
    fn render_button_bar_lays_out_left_and_right() {
        let backend = ratatui::backend::TestBackend::new(40, 3);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, 40, 1);
                let specs = [ButtonSpec::primary("Save"), ButtonSpec::secondary("Cancel")];

                // Left-aligned: first button starts at area.x.
                let hits = render_button_bar(f, area, &specs, false);
                assert_eq!(hits.len(), 2);
                assert_eq!(hits[0].rect, Rect::new(0, 0, 6, 1)); // " Save " = 6 cols
                assert_eq!(hits[0].index, 0);
                // " Cancel " => 8 cols, after a 6-col button + 1 separator.
                assert_eq!(hits[1].rect, Rect::new(7, 0, 8, 1));

                // Right-aligned: the row is packed against the right edge.
                let hits = render_button_bar(f, area, &specs, true);
                assert_eq!(hits.len(), 2);
                let last = hits[1].rect;
                assert_eq!(last.x + last.width, area.x + area.width);
            })
            .unwrap();
    }

    #[test]
    fn render_button_bar_drops_overflowing_buttons() {
        let backend = ratatui::backend::TestBackend::new(40, 3);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                // Only room for the first button (" Save " = 6 cols; a second
                // would need 6+1+8 = 15).
                let area = Rect::new(0, 0, 9, 1);
                let specs = [ButtonSpec::primary("Save"), ButtonSpec::secondary("Cancel")];
                let hits = render_button_bar(f, area, &specs, false);
                assert_eq!(hits.len(), 1, "overflowing button is dropped");
                assert_eq!(hits[0].index, 0);
            })
            .unwrap();
    }

    #[test]
    fn render_button_bar_left_overflow_shows_ellipsis() {
        let backend = ratatui::backend::TestBackend::new(40, 1);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                // " Save " = 6 fits; " Cancel " = 8 needs 6+1+8 = 15 > 12, so it
                // is dropped, leaving room for the ` … ` overflow marker.
                let area = Rect::new(0, 0, 12, 1);
                let specs = [ButtonSpec::primary("Save"), ButtonSpec::secondary("Cancel")];
                let hits = render_button_bar(f, area, &specs, false);
                assert_eq!(hits.len(), 1);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let row: String = (0..12).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(
            row.contains('…'),
            "left-aligned overflow marks dropped buttons with an ellipsis, got {row:?}"
        );
    }

    #[test]
    fn render_button_bar_right_overflow_has_no_ellipsis() {
        let backend = ratatui::backend::TestBackend::new(40, 1);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, 12, 1);
                let specs = [ButtonSpec::primary("Save"), ButtonSpec::secondary("Cancel")];
                let _ = render_button_bar(f, area, &specs, true);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let row: String = (0..12).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(
            !row.contains('…'),
            "right-aligned modal footers don't draw the overflow marker, got {row:?}"
        );
    }

    #[test]
    fn button_with_hint_widens_chip_and_renders_suffix() {
        let backend = ratatui::backend::TestBackend::new(40, 1);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, 40, 1);
                let specs = [ButtonSpec::primary("Comment").with_hint("·c")];
                let hits = render_button_bar(f, area, &specs, false);
                assert_eq!(hits.len(), 1);
                // " Comment·c ": label 7 + hint 2 + 2 padding = 11 cols.
                assert_eq!(hits[0].rect.width, 11);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let row: String = (0..11).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(row, " Comment·c ");
    }

    #[test]
    fn truncate_ellipsis_keeps_short_strings_intact() {
        assert_eq!(truncate_ellipsis("hello", 10), "hello");
        assert_eq!(truncate_ellipsis("hello", 5), "hello");
    }

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn key_hint_line_alternates_key_and_desc_spans() {
        let line = key_hint_line(&[("Enter", " save  "), ("Esc", " cancel")]);
        assert_eq!(line.spans.len(), 4);
        assert_eq!(line_text(&line), "Enter save  Esc cancel");
    }

    #[test]
    fn editor_field_line_marks_active_and_wraps_selectors() {
        // Inactive plain text: no cursor, no marker.
        assert_eq!(
            line_text(&editor_field_line("repo", "x".into(), false, false)),
            "  repo     x"
        );
        // Active text field gets a "▸" prefix and a block cursor at the caret.
        // With no cursor supplied the caret sits past the end, drawn as a
        // trailing space-block.
        assert_eq!(
            line_text(&editor_field_line("repo", "x".into(), false, true)),
            "▸ repo     x "
        );
        // Selector values are wrapped in guillemets (no cursor even when active).
        assert_eq!(
            line_text(&editor_field_line("status", "todo".into(), true, true)),
            "▸ status   ‹ todo ›"
        );
    }

    #[test]
    fn editor_field_line_with_cursor_draws_block_at_caret() {
        // Caret in the middle: the character under the cursor is its own span,
        // so the visible text is unchanged but split before/cursor/after.
        let line = editor_field_line_with_cursor("title", "hello".into(), false, true, Some(2));
        assert_eq!(line_text(&line), "▸ title    hello");
        // Spans: label, "he", cursor "l", "lo".
        assert_eq!(line.spans.len(), 4);
        assert_eq!(line.spans[2].content.as_ref(), "l");

        // Caret at end: a trailing space-block is appended after the value.
        let line = editor_field_line_with_cursor("title", "hi".into(), false, true, Some(2));
        assert_eq!(line_text(&line), "▸ title    hi ");

        // Caret at start: cursor is the first character.
        let line = editor_field_line_with_cursor("title", "ab".into(), false, true, Some(0));
        assert_eq!(line.spans[1].content.as_ref(), "a");
    }

    #[test]
    fn truncate_ellipsis_cuts_and_appends_ellipsis() {
        assert_eq!(truncate_ellipsis("hello world", 5), "hell…");
    }

    #[test]
    fn truncate_ellipsis_returns_empty_when_too_narrow() {
        assert_eq!(truncate_ellipsis("hello", 1), "");
        assert_eq!(truncate_ellipsis("hello", 0), "");
    }

    #[test]
    fn truncate_ellipsis_counts_by_char_not_byte() {
        // Multi-byte chars count as one column each.
        assert_eq!(truncate_ellipsis("héllo wörld", 5), "héll…");
    }

    #[test]
    fn centered_rect_has_exact_height() {
        let rect = centered_fixed_height_rect(50, 10, area(100, 40));
        assert_eq!(rect.height, 10);
    }

    #[test]
    fn centered_rect_is_horizontally_centered() {
        let rect = centered_fixed_height_rect(50, 10, area(100, 40));
        assert_eq!(rect.x, 25);
        assert_eq!(rect.width, 50);
    }

    #[test]
    fn centered_rect_is_vertically_centered() {
        let rect = centered_fixed_height_rect(50, 10, area(100, 40));
        // With Min(0) / Length(10) / Min(0), the 10 lines should be centered
        // in 40 rows: (40 - 10) / 2 = 15
        assert_eq!(rect.y, 15);
    }

    #[test]
    fn centered_rect_clamps_to_area_height() {
        let rect = centered_fixed_height_rect(50, 50, area(100, 20));
        // Height is clamped to available area
        assert!(rect.height <= 20);
    }

    #[test]
    fn status_color_maps_all_variants() {
        // The default palette colours for the hooks-driven states.
        assert_eq!(status_color(SessionStatus::Working), Color::Yellow);
        assert_eq!(status_color(SessionStatus::Blocked), Color::Red);
        assert_eq!(status_color(SessionStatus::Done), Color::LightBlue);
        assert_eq!(status_color(SessionStatus::Idle), Color::Green);
        assert_eq!(status_color(SessionStatus::Error), Color::Red);
    }

    #[test]
    fn focused_block_returns_block_for_both_states() {
        let focused = focused_block(" Test ", true);
        let unfocused = focused_block(" Test ", false);
        // Verify both produce valid blocks that can compute inner area
        let test_area = area(40, 10);
        let inner_focused = focused.inner(test_area);
        let inner_unfocused = unfocused.inner(test_area);
        // Both should produce inner areas smaller than the outer area (borders consume space)
        assert!(inner_focused.width < test_area.width);
        assert!(inner_focused.height < test_area.height);
        assert!(inner_unfocused.width < test_area.width);
        assert!(inner_unfocused.height < test_area.height);
    }

    #[test]
    fn modal_block_produces_valid_block_with_borders() {
        let test_area = area(40, 10);
        let block = modal_block("Test Modal");
        let inner = block.inner(test_area);
        assert!(inner.width < test_area.width);
        assert!(inner.height < test_area.height);
    }

    #[test]
    fn modal_block_danger_produces_valid_block_with_borders() {
        let test_area = area(40, 10);
        let block = modal_block_danger("Delete");
        let inner = block.inner(test_area);
        assert!(inner.width < test_area.width);
        assert!(inner.height < test_area.height);
    }

    #[test]
    fn modal_title_matches_focused_title() {
        assert_eq!(Theme::modal_title(), Theme::focused_title());
    }

    #[test]
    fn modal_title_danger_uses_danger_color() {
        let style = Theme::modal_title_danger();
        assert_eq!(style.bg, Some(Theme::danger()));
    }
}
