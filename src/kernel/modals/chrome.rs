//! The shared furniture of a system modal: frames, pills, hints, scrollbars.
//!
//! Every style here names the same palette role v1's `ui::theme::Theme` names,
//! so the three modals keep looking like v1's without the kernel reaching into
//! `ui` (which the architecture forbids, and which is being deleted). The
//! palette arrives as data — `Themes::active().palette` — rather than through
//! v1's process-global `Theme::current()`.

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::Frame;

use crate::session::theme_config::ThemePalette;

/// The palette, exposed as the named styles a modal actually asks for.
///
/// One-to-one with v1's `Theme::` helpers (`modal_title`, `section_header`,
/// `keybind`, `selected_item`, …); the names are kept so a reader can diff the
/// two by eye.
///
/// It also carries the pointer, because it is the one thing already handed to
/// every modal renderer and to every piece of shared furniture below: a pill
/// can light itself without `button_bar` growing a parameter no caller would
/// have anything else to pass to.
#[derive(Clone, Copy)]
pub struct Chrome<'a> {
    pub palette: &'a ThemePalette,
    pointer: Option<Position>,
    /// What the dim pass covers. `None` means the whole frame.
    ///
    /// Carried here rather than derived, because the region to dim is not the
    /// modal's own rect and not always the screen: the chrome bands must stay
    /// readable while a modal is up. Dimming the band that reports errors would
    /// work against the reason it exists.
    dim: Option<Rect>,
}

impl<'a> Chrome<'a> {
    pub fn new(palette: &'a ThemePalette) -> Self {
        Self {
            palette,
            pointer: None,
            dim: None,
        }
    }

    /// Confine the dim pass to `area` instead of the whole frame.
    pub fn within(mut self, area: Rect) -> Self {
        self.dim = Some(area);
        self
    }

    /// Aim the frame's chrome at where the pointer is, or nowhere.
    pub fn at(mut self, pointer: Option<Position>) -> Self {
        self.pointer = pointer;
        self
    }

    pub fn pointer(&self) -> Option<Position> {
        self.pointer
    }

    /// Whether the pointer is inside `rect` — the one question every hover
    /// affordance asks.
    pub fn hovering(&self, rect: Rect) -> bool {
        self.pointer.is_some_and(|pointer| rect.contains(pointer))
    }

    /// v1 `Theme::modal_title` — the focused-panel badge: bold, inverted on
    /// accent.
    pub fn modal_title(&self) -> Style {
        Style::default()
            .fg(self.palette.inverted_fg)
            .bg(self.palette.accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn section_header(&self) -> Style {
        Style::default()
            .fg(self.palette.accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn keybind(&self) -> Style {
        Style::default().fg(self.palette.keybind_hint)
    }

    pub fn keybind_desc(&self) -> Style {
        Style::default().fg(self.palette.text_muted)
    }

    pub fn selected_item(&self) -> Style {
        Style::default()
            .fg(self.palette.accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn normal_item(&self) -> Style {
        Style::default().fg(self.palette.text_primary)
    }

    pub fn muted(&self) -> Style {
        Style::default().fg(self.palette.text_muted)
    }

    /// v1 `ui::button_style`: a filled pill. Primary is the accent badge;
    /// secondary the selection pair, which every palette guarantees is legible.
    pub fn button(&self, primary: bool) -> Style {
        if primary {
            Style::default()
                .fg(self.palette.inverted_fg)
                .bg(self.palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(self.palette.selection_fg)
                .bg(self.palette.selection_bg)
                .add_modifier(Modifier::BOLD)
        }
    }

    /// The same pill with the pointer on it — v1's `apply_hover_highlight`
    /// button leg: the fill brightens to `accent_bright` and the text is forced
    /// to `inverted_fg`, which is what keeps a *secondary* pill legible once its
    /// own fill is replaced.
    pub fn button_hovered(&self) -> Style {
        Style::default()
            .fg(self.palette.inverted_fg)
            .bg(self.palette.accent_bright)
            .add_modifier(Modifier::BOLD)
    }

    /// The band a list row takes under the pointer. v1 tints only the
    /// background here, so a selected row keeps its accent text.
    pub fn row_hovered_bg(&self) -> ratatui::style::Color {
        self.palette.selection_bg
    }
}

/// The keystroke a footer pill replays when it is clicked.
///
/// A pill carries a key rather than a closure so a click enters the modal
/// through [`super::Modals::on_key`] — v1's `try_modal_button_click`, and the
/// same rule as [`crate::kernel::node::ClickVerb::Key`]: a button and its key
/// are one code path, so they cannot come to mean different things.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Replay {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
    /// The canonical spelling of the same keystroke, which `on_key` wants
    /// beside the event (help stores it when capturing).
    pub chord: &'static str,
}

impl Replay {
    /// `Ctrl+S`, which the settings modal's ` Save ` pill replays.
    pub const CTRL_S: Self = Self {
        code: KeyCode::Char('s'),
        modifiers: crossterm::event::KeyModifiers::CONTROL,
        chord: "ctrl+s",
    };

    pub const ESC: Self = Self {
        code: KeyCode::Esc,
        modifiers: KeyModifiers::NONE,
        chord: "esc",
    };
    pub const ENTER: Self = Self {
        code: KeyCode::Enter,
        modifiers: KeyModifiers::NONE,
        chord: "enter",
    };

    pub const fn new(code: KeyCode, modifiers: KeyModifiers, chord: &'static str) -> Self {
        Self {
            code,
            modifiers,
            chord,
        }
    }
}

/// What the pointer is over inside a modal, as something comparable.
///
/// Compared rather than the raw cell so a pointer crossing a wide row costs one
/// repaint instead of one per column — the rule the rest of the interface
/// already follows (`App::hover`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hover {
    #[default]
    Nothing,
    Row(usize),
    Button(usize),
}

/// Where a modal's clickable things landed last frame.
///
/// Refilled by `render` and read by the click and hover paths, because that is
/// the only way either can work: a modal paints cell by cell and has no node
/// tree for the kernel to collect identity from, unlike a plugin.
#[derive(Default)]
pub struct Hits {
    /// Selectable list rows, each with the index it selects.
    pub rows: Vec<(Rect, usize)>,
    /// Footer pills, each with the keystroke it replays.
    pub buttons: Vec<(Rect, Replay)>,
}

impl Hits {
    pub fn clear(&mut self) {
        self.rows.clear();
        self.buttons.clear();
    }

    pub fn row_at(&self, x: u16, y: u16) -> Option<usize> {
        let position = Position::new(x, y);
        self.rows
            .iter()
            .find(|(rect, _)| rect.contains(position))
            .map(|(_, index)| *index)
    }

    pub fn button_at(&self, x: u16, y: u16) -> Option<Replay> {
        let position = Position::new(x, y);
        self.buttons
            .iter()
            .find(|(rect, _)| rect.contains(position))
            .map(|(_, replay)| *replay)
    }

    /// What a pointer at `(x, y)` is over. Buttons win, as they do for a click.
    pub fn hover_at(&self, x: u16, y: u16) -> Hover {
        let position = Position::new(x, y);
        if let Some(index) = self
            .buttons
            .iter()
            .position(|(rect, _)| rect.contains(position))
        {
            return Hover::Button(index);
        }
        match self.row_at(x, y) {
            Some(index) => Hover::Row(index),
            None => Hover::Nothing,
        }
    }
}

/// Band the list row under the pointer, once the modal has finished painting.
///
/// A second pass rather than a style on the row's spans, because a `Paragraph`
/// line only colours the cells its text occupies — the band has to reach the
/// full width of the row, which is v1's reason for painting hover into the
/// buffer in `apply_hover_highlight`. Pills need no such pass: `button_bar`
/// paints its own fill and picks the hovered style there.
pub fn paint_row_hover(frame: &mut Frame, hits: &Hits, chrome: Chrome<'_>) {
    let Some(pointer) = chrome.pointer else {
        return;
    };
    let Some((rect, _)) = hits.rows.iter().find(|(rect, _)| rect.contains(pointer)) else {
        return;
    };
    let background = chrome.row_hovered_bg();
    let buffer = frame.buffer_mut();
    for y in rect.y..rect.y.saturating_add(rect.height) {
        for x in rect.x..rect.x.saturating_add(rect.width) {
            if let Some(cell) = buffer.cell_mut(Position::new(x, y)) {
                cell.set_bg(background);
            }
        }
    }
}

/// A centred rect taking `percent_x` × `percent_y` of `area`. v1's
/// `view::centered_rect`.
pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
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

/// A centred rect `percent_x` wide and exactly `height` tall. v1's
/// `ui::centered_fixed_height_rect`, used where the modal grows with its
/// content.
pub fn centered_fixed_height_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(height.min(area.height)),
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

/// A centred rect of a fixed size, clamped to `area`. v1's
/// `settings_modal::centered_rect`.
pub fn centered_fixed_rect(width: u16, height: u16, area: Rect) -> Rect {
    let column = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(width.min(area.width)),
            Constraint::Min(0),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(height.min(area.height)),
            Constraint::Min(0),
        ])
        .split(column[1])[1]
}

/// Dim the screen, clear the modal's cells and draw its frame; returns the
/// inner rect. v1's `ui::render_modal_frame`.
///
/// The dim pass is what makes a modal read as *above* the interface rather than
/// merely drawn later — it is the visual half of capturing input.
pub fn modal_frame(frame: &mut Frame, area: Rect, title: &str, chrome: Chrome<'_>) -> Rect {
    frame.render_widget(
        Block::default().style(Style::default().bg(chrome.palette.modal_dim_bg)),
        chrome.dim.unwrap_or_else(|| frame.area()),
    );
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(Line::from(Span::styled(
            format!(" {title} "),
            chrome.modal_title(),
        )))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(chrome.palette.modal_border))
        .style(Style::default().bg(chrome.palette.modal_bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

/// A modal frame whose title *is* its tab strip.
///
/// Two reasons it lives in the title rather than in a row of its own. The strip
/// cost two rows — the headings and a rule — which is a lot to spend on two
/// words in a modal that is mostly list. And a modal titled `Settings` whose
/// first tab is also `Settings` says the same thing twice: the frame already
/// names what you are looking at, so letting it name *which part* is one label,
/// not two.
///
/// Returns the inner rect and each tab's cells on the border, so a click can
/// select the tab it landed on.
pub fn modal_frame_tabs(
    frame: &mut Frame,
    area: Rect,
    tabs: &[(&str, bool)],
    chrome: Chrome<'_>,
) -> (Rect, Vec<Rect>) {
    frame.render_widget(
        Block::default().style(Style::default().bg(chrome.palette.modal_dim_bg)),
        chrome.dim.unwrap_or_else(|| frame.area()),
    );
    frame.render_widget(Clear, area);

    let mut spans = Vec::with_capacity(tabs.len() * 2 + 1);
    let mut rects = Vec::with_capacity(tabs.len());
    // The title starts one cell in from the corner, and every span before a tab
    // shifts it — tracked as it is built rather than recomputed, so the hitbox
    // and the glyphs cannot disagree.
    let mut x = area.x + 1;
    for (nth, (label, active)) in tabs.iter().enumerate() {
        if nth > 0 {
            spans.push(Span::styled(
                "│",
                Style::default().fg(chrome.palette.modal_border),
            ));
            x += 1;
        }
        let text = format!(" {label} ");
        let width = text.chars().count() as u16;
        let rect = Rect::new(x, area.y, width, 1);
        let style = if *active {
            chrome.modal_title()
        } else if chrome.hovering(rect) {
            chrome.button_hovered()
        } else {
            chrome.muted()
        };
        spans.push(Span::styled(text, style));
        rects.push(rect);
        x += width;
    }

    let block = Block::default()
        .title(Line::from(spans))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(chrome.palette.modal_border))
        .style(Style::default().bg(chrome.palette.modal_bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    (inner, rects)
}

/// A footer key hint: keys in the hint colour, their descriptions muted. v1's
/// `ui::key_hint_line`.
pub fn hint_line<'a>(pairs: &[(&'a str, &'a str)], chrome: Chrome<'_>) -> Line<'a> {
    let mut spans = Vec::with_capacity(pairs.len() * 2);
    for (key, desc) in pairs {
        spans.push(Span::styled(*key, chrome.keybind()));
        spans.push(Span::styled(*desc, chrome.keybind_desc()));
    }
    Line::from(spans)
}

/// A button in a modal footer: its label, whether it is the primary action, and
/// the keystroke a click on it replays.
pub struct Pill<'a> {
    pub label: &'a str,
    pub primary: bool,
    pub key: Replay,
}

/// Right-pack `pills` into a single-row `area`, returning each placed one with
/// the key it replays, so a click can be mapped back to it.
///
/// v1's `ui::render_button_bar` with `right_align`: a pill that would overflow
/// is dropped rather than clipped mid-glyph — so the returned list is the
/// pills that are actually on screen and nothing else.
pub fn button_bar(
    frame: &mut Frame,
    area: Rect,
    pills: &[Pill<'_>],
    chrome: Chrome<'_>,
) -> Vec<(Rect, Replay)> {
    if area.height == 0 || area.width == 0 || pills.is_empty() {
        return Vec::new();
    }
    let width_of = |pill: &Pill<'_>| pill.label.chars().count() as u16 + 2;
    let total: u16 = pills.iter().map(width_of).sum::<u16>() + pills.len().saturating_sub(1) as u16;
    let mut x = if total <= area.width {
        area.x + area.width - total
    } else {
        area.x
    };
    let limit = area.x + area.width;

    let mut placed = Vec::with_capacity(pills.len());
    for pill in pills {
        let width = width_of(pill);
        if x + width > limit {
            break;
        }
        let rect = Rect::new(x, area.y, width, 1);
        let style = if chrome.hovering(rect) {
            chrome.button_hovered()
        } else {
            chrome.button(pill.primary)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(format!(" {} ", pill.label), style))),
            rect,
        );
        placed.push((rect, pill.key));
        x += width + 1;
    }
    placed
}

/// Split a row so a right-edge scrollbar has a column, when the content
/// overflows. v1's `ui::scrollbar::reserve_track`.
pub fn reserve_track(area: Rect, content_len: usize, viewport: usize) -> (Rect, Option<Rect>) {
    if content_len <= viewport || area.width <= 1 {
        return (area, None);
    }
    (
        Rect {
            width: area.width - 1,
            ..area
        },
        Some(Rect {
            x: area.x + area.width - 1,
            width: 1,
            ..area
        }),
    )
}

/// Draw a vertical scrollbar, exactly as v1 does — the same ratatui widget with
/// the same two roles, so the thumb reads identically in both binaries.
pub fn scrollbar(
    frame: &mut Frame,
    track: Rect,
    content_len: usize,
    viewport: usize,
    position: usize,
    chrome: Chrome<'_>,
) {
    if track.height == 0 || track.width == 0 {
        return;
    }
    let mut state = ScrollbarState::new(content_len)
        .position(position)
        .viewport_content_length(viewport);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_style(Style::default().fg(chrome.palette.accent))
            .track_style(Style::default().fg(chrome.palette.text_muted)),
        track,
        &mut state,
    );
}

/// Truncate `text` to `width` columns, marking the cut with `…`.
///
/// Character-counted rather than byte-counted, so a description with a
/// non-ASCII glyph in it is not cut mid-codepoint.
pub fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Pad `text` to `width` columns.
pub fn pad(text: &str, width: usize) -> String {
    let len = text.chars().count();
    let mut out = text.to_string();
    for _ in len..width {
        out.push(' ');
    }
    out
}
