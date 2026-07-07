//! Renderer for the Settings panel (`crate::app::modals::SettingsModal`) — a
//! centered modal that lists every `settings.toml` knob grouped into Features /
//! Notifications / Scalars sections. Modeled on
//! [`super::automation_editor_modal`].

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::modals::SettingsField;

use super::theme::Theme;
use super::{key_hint_line, render_action_footer, render_modal_frame};

/// Default modal width. Sized to fit the value column with a tidy gap on a
/// normal terminal. Clamped to the screen on narrow terminals.
const MODAL_WIDTH: u16 = 66;

/// Wider modal used on roomy (≥120-col) terminals so the longest field
/// descriptions render in full instead of truncating with `…` — the value
/// column stays the same width, the extra room all goes to the description.
const MODAL_WIDTH_WIDE: u16 = 84;

/// The modal width to use for a terminal `area_width` cols wide: the wide
/// layout once there's room for it, the compact default otherwise.
fn modal_width_for(area_width: u16) -> u16 {
    if area_width >= 120 {
        MODAL_WIDTH_WIDE
    } else {
        MODAL_WIDTH
    }
}

/// A fixed `width × height` rect centered in `area` (clamped to it).
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let col = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(w),
            Constraint::Min(0),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(h),
            Constraint::Min(0),
        ])
        .split(col[1])[1]
}

/// Borrowed view of a `SettingsModal` (`crate::app::modals`) for rendering.
pub struct SettingsModalState<'a> {
    pub modal: &'a crate::app::modals::SettingsModal,
    /// Whether any restart-required field has changed (drives the note).
    pub restart_pending: bool,
}

/// Sections, in render order: a header label and the fields under it.
const SECTIONS: [(&str, &[SettingsField]); 3] = [
    (
        "Features",
        &[
            SettingsField::FeatTasks,
            SettingsField::FeatAutomations,
            SettingsField::FeatFileViewer,
            SettingsField::FeatGlobalSearch,
            SettingsField::FeatInfoPanel,
            SettingsField::FeatShellPane,
            SettingsField::FeatCodeReview,
            SettingsField::FeatPerfHud,
            SettingsField::FeatMouse,
            SettingsField::FeatNotifications,
            SettingsField::FeatSoftDelete,
            SettingsField::FeatVersionCheck,
            SettingsField::FeatAutoUpdate,
        ],
    ),
    (
        "Notifications",
        &[
            SettingsField::NotifAlsoOnWaiting,
            SettingsField::NotifSuppressForActive,
            SettingsField::NotifSound,
            SettingsField::NotifMinInterval,
        ],
    ),
    (
        "Scalars",
        &[
            SettingsField::ScrollbackLines,
            SettingsField::TwoPanelMinCols,
            SettingsField::ThreePanelMinCols,
            SettingsField::AuditRetentionDays,
        ],
    ),
];

/// Renders the Settings modal. Returns its per-field click hitboxes (indexed by
/// position in `SettingsField::ORDER`) plus the footer buttons.
pub fn render_settings_modal(
    frame: &mut Frame,
    state: &SettingsModalState<'_>,
) -> (Vec<super::RowHitbox>, super::ModalButtons) {
    // The footer (the selected field's settings.toml key, optional restart note,
    // key hints) is pinned to the bottom; the field list above it scroll-windows
    // around the active field when the modal is shorter than the content.
    let footer = footer_lines(state);
    let width = modal_width_for(frame.area().width);
    // Build the rows first so the modal height matches the real content
    // (section headers, blank separators between sections, and field rows).
    let rows = build_rows(state.modal, (width - 2) as usize);
    let desired = rows.len() as u16 + footer.len() as u16 + 2;
    let height = desired.min(frame.area().height.saturating_sub(2)).max(6);
    let area = centered_rect(width, height, frame.area());

    let inner = render_modal_frame(frame, area, "Settings");

    let visible = (inner.height as usize).saturating_sub(footer.len());
    let active_disp = rows
        .iter()
        .position(|(_, f)| *f == Some(state.modal.field))
        .unwrap_or(0);

    // Window the rows around the active field when they overflow.
    let window = rows.len() > visible && visible > 0;
    let start = if window {
        active_disp
            .saturating_sub(visible / 2)
            .min(rows.len() - visible)
    } else {
        0
    };
    let count = if window { visible } else { rows.len() };

    // Each visible field row gets a click hitbox carrying its ORDER index.
    let mut lines: Vec<Line> = Vec::with_capacity(count + footer.len());
    let mut field_hits: Vec<super::RowHitbox> = Vec::new();
    for (screen_row, (line, field)) in rows.into_iter().skip(start).take(count).enumerate() {
        if let Some(idx) = field.and_then(|f| SettingsField::ORDER.iter().position(|o| *o == f)) {
            field_hits.push(super::RowHitbox {
                rect: Rect::new(inner.x, inner.y + screen_row as u16, inner.width, 1),
                index: idx,
            });
        }
        lines.push(line);
    }
    lines.extend(footer);

    frame.render_widget(Paragraph::new(lines), inner);

    // Clickable `[ Save ]` / `[ Cancel ]` buttons over the bottom footer row.
    let footer_row = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(1),
        inner.width,
        1,
    );
    let buttons = render_action_footer(
        frame,
        footer_row,
        (
            "Save",
            crossterm::event::KeyCode::Char('s'),
            crossterm::event::KeyModifiers::CONTROL,
        ),
        "Cancel",
    );
    (field_hits, buttons)
}

/// The pinned footer: the selected field's settings.toml key, an optional
/// restart note, and the key-hint line.
fn footer_lines<'a>(state: &SettingsModalState<'_>) -> Vec<Line<'a>> {
    let mut footer = Vec::new();
    footer.push(Line::from(vec![
        Span::styled("  key  ", Theme::label()),
        Span::styled(
            state.modal.field.label().to_string(),
            Style::default().fg(Theme::text_muted()),
        ),
    ]));
    if state.restart_pending {
        footer.push(Line::from(Span::styled(
            "  ⟳ some changes apply after restart",
            Style::default().fg(Theme::keybind_hint()),
        )));
    }
    // Save/Cancel are rendered as clickable buttons over this row; keep only
    // the navigation/adjust hints as text here.
    footer.push(key_hint_line(&[
        ("Tab/↑↓", " move  "),
        ("←→", " adjust  "),
        ("Space/click", " toggle"),
    ]));
    footer
}

/// Build the display rows: each section header (no field) followed by its field
/// rows (carrying their [`SettingsField`] so the active row can be located).
/// `width` is the modal's inner column count, used to right-align the values.
fn build_rows<'a>(
    m: &crate::app::modals::SettingsModal,
    width: usize,
) -> Vec<(Line<'a>, Option<SettingsField>)> {
    // Width of the value column: the widest value across all fields, so every
    // value (and the `⟳` markers after it) aligns into a single column.
    let value_width = SettingsField::ORDER
        .iter()
        .map(|f| value_text(m, *f).chars().count())
        .max()
        .unwrap_or(3);
    // Width of the keyword column: the widest keyword, so the descriptions that
    // follow all start at the same column.
    let keyword_width = SettingsField::ORDER
        .iter()
        .map(|f| f.keyword().chars().count())
        .max()
        .unwrap_or(0);

    let mut rows: Vec<(Line<'a>, Option<SettingsField>)> = Vec::new();
    for (section_idx, (title, fields)) in SECTIONS.iter().enumerate() {
        // Blank separator above every section but the first, so the titles
        // stand clearly apart.
        if section_idx > 0 {
            rows.push((Line::default(), None));
        }
        rows.push((
            Line::from(Span::styled(
                format!("  {}", title.to_uppercase()),
                Style::default()
                    .fg(Theme::accent())
                    .add_modifier(Modifier::BOLD),
            )),
            None,
        ));
        for field in *fields {
            rows.push((
                field_line(m, *field, width, keyword_width, value_width),
                Some(*field),
            ));
        }
    }
    rows
}

/// The rendered value for a field: a `‹ n ›` stepper for scalars, `on`/`off`
/// for booleans.
fn value_text(m: &crate::app::modals::SettingsModal, field: SettingsField) -> String {
    let v = m.value_string(field);
    if field.is_scalar() {
        format!("‹ {v} ›")
    } else {
        v
    }
}

/// One field row laid out in fixed columns so values and markers align:
/// `▸ <keyword (bold)> <description (dimmed)> … <value right-justified in
/// value_width><⟳ marker>`. The bold keyword carries the "what" at a glance; the
/// dimmer description is the supporting detail.
fn field_line<'a>(
    m: &crate::app::modals::SettingsModal,
    field: SettingsField,
    width: usize,
    keyword_width: usize,
    value_width: usize,
) -> Line<'a> {
    let active = field == m.field;

    let pointer = if active { "▸ " } else { "  " };
    // Keyword: bold, brighter than the description so the hierarchy reads
    // keyword > description.
    let keyword_style = if active {
        Style::default()
            .fg(Theme::accent_bright())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Theme::text_secondary())
            .add_modifier(Modifier::BOLD)
    };
    // Description: non-bold and dimmer, recedes behind the keyword.
    let desc_style = if active {
        Style::default().fg(Theme::accent())
    } else {
        Style::default().fg(Theme::text_muted())
    };

    let value = m.value_string(field);
    let value_str = value_text(m, field);
    let value_style = if field.is_scalar() {
        Style::default()
            .fg(Theme::accent())
            .add_modifier(Modifier::BOLD)
    } else if value == "on" {
        // Enabled: a calm "on" that stands out without shouting.
        Style::default()
            .fg(Theme::tool_allowed())
            .add_modifier(Modifier::BOLD)
    } else {
        // Disabled: dim, so "off" recedes rather than reading like an error.
        Style::default().fg(Theme::text_muted())
    };
    // Always reserve the marker column (2 cols) so value right-edges align on
    // every row, restart-required or not. The `⟳` glyph reuses the same
    // accent-warning colour (bold) as the "some changes apply after restart"
    // footer note, so the two read as the same signal — the old muted-gray
    // glyph was nearly invisible. The reserved blank column stays muted (it
    // paints nothing). `restart_required` is non-trivial, so compute it once.
    let restart = field.restart_required();
    let marker = if restart { " ⟳" } else { "  " };
    let marker_style = if restart {
        Style::default()
            .fg(Theme::keybind_hint())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Theme::text_muted())
    };

    // Columns: pointer(2) + keyword(keyword_width) + gap(1) + desc + pad +
    // value(value_width) + marker(2). The keyword is padded to a fixed width so
    // every description starts in the same column.
    let keyword = field.keyword();
    let keyword_pad = keyword_width.saturating_sub(keyword.chars().count());
    let keyword_block = keyword_width + 1; // +1 gap after the keyword
    let right_block = value_width + marker.chars().count();
    let mut desc = field.description().to_string();
    // +1 minimum gap before the value column.
    let left_budget = width.saturating_sub(2 + keyword_block + right_block + 1);
    if desc.chars().count() > left_budget {
        desc = desc.chars().take(left_budget.saturating_sub(1)).collect();
        desc.push('…');
    }
    let pad = width
        .saturating_sub(2 + keyword_block + desc.chars().count() + right_block)
        .max(1);
    let value_pad = value_width.saturating_sub(value_str.chars().count());

    Line::from(vec![
        Span::styled(
            format!("{pointer}{keyword}{}", " ".repeat(keyword_pad + 1)),
            keyword_style,
        ),
        Span::styled(desc, desc_style),
        Span::raw(" ".repeat(pad + value_pad)),
        Span::styled(value_str, value_style),
        Span::styled(marker, marker_style),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_width_widens_only_on_roomy_terminals() {
        // Below the 120-col threshold the compact default is used.
        assert_eq!(modal_width_for(80), MODAL_WIDTH);
        assert_eq!(modal_width_for(119), MODAL_WIDTH);
        // At/above the threshold the wide layout gives descriptions more room.
        assert_eq!(modal_width_for(120), MODAL_WIDTH_WIDE);
        assert_eq!(modal_width_for(200), MODAL_WIDTH_WIDE);
    }
}
