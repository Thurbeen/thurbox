use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::Frame;

use crate::theme::ThemePalette;

#[derive(Clone, Copy)]
pub struct Chrome<'a> {
    pub palette: &'a ThemePalette,
    pointer: Option<Position>,
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

    pub fn within(mut self, area: Rect) -> Self {
        self.dim = Some(area);
        self
    }

    pub fn at(mut self, pointer: Option<Position>) -> Self {
        self.pointer = pointer;
        self
    }

    pub fn pointer(&self) -> Option<Position> {
        self.pointer
    }

    pub fn hovering(&self, rect: Rect) -> bool {
        self.pointer.is_some_and(|pointer| rect.contains(pointer))
    }

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

    pub fn button_hovered(&self) -> Style {
        Style::default()
            .fg(self.palette.inverted_fg)
            .bg(self.palette.accent_bright)
            .add_modifier(Modifier::BOLD)
    }

    pub fn row_hovered_bg(&self) -> ratatui::style::Color {
        self.palette.selection_bg
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Replay {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
    pub chord: &'static str,
}

impl Replay {
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hover {
    #[default]
    Nothing,
    Row(usize),
    Button(usize),
}

#[derive(Default)]
pub struct Hits {
    pub rows: Vec<(Rect, usize)>,
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

pub fn hint_line<'a>(pairs: &[(&'a str, &'a str)], chrome: Chrome<'_>) -> Line<'a> {
    let mut spans = Vec::with_capacity(pairs.len() * 2);
    for (key, desc) in pairs {
        spans.push(Span::styled(*key, chrome.keybind()));
        spans.push(Span::styled(*desc, chrome.keybind_desc()));
    }
    Line::from(spans)
}

pub struct Pill<'a> {
    pub label: &'a str,
    pub primary: bool,
    pub key: Replay,
}

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

pub fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn pad(text: &str, width: usize) -> String {
    let len = text.chars().count();
    let mut out = text.to_string();
    for _ in len..width {
        out.push(' ');
    }
    out
}
