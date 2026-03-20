use std::path::PathBuf;

use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

use super::theme::Theme;
use super::{centered_fixed_height_rect, render_text_field_with_suggestion};
use crate::app::modals::RepoPickerFocus;

pub struct RepoPickerState<'a> {
    pub bookmarks: &'a [PathBuf],
    pub selected: &'a [bool],
    pub worktree: &'a [bool],
    pub list_index: usize,
    pub path_input: &'a str,
    pub path_cursor: usize,
    pub path_suggestion: Option<&'a str>,
    pub focus: RepoPickerFocus,
}

pub fn render_repo_picker_modal(frame: &mut Frame, state: &RepoPickerState<'_>) {
    let list_inner = if state.bookmarks.is_empty() {
        1
    } else {
        state.bookmarks.len().min(10)
    };
    let list_height = list_inner as u16 + 2; // +2 for borders

    // Layout: list + path input(3) + footer(1) + outer border(2)
    let total_height = list_height + 3 + 1 + 2;

    let area = centered_fixed_height_rect(60, total_height, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Select Repos ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::ACCENT));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(list_height), // Bookmark list
            Constraint::Length(3),           // Path input
            Constraint::Min(1),              // Footer
        ])
        .split(inner);

    // Bookmark list with checkboxes
    let list_focused = state.focus == RepoPickerFocus::List;
    let border_color = if list_focused {
        Theme::BORDER_FOCUSED
    } else {
        Theme::BORDER_UNFOCUSED
    };

    let list_block = Block::default()
        .title(format!(" Repos ({}) ", state.bookmarks.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let list_inner_area = list_block.inner(chunks[0]);
    frame.render_widget(list_block, chunks[0]);

    if state.bookmarks.is_empty() {
        let placeholder = Paragraph::new(Line::from(Span::styled(
            "  No bookmarks — add via path input below",
            Style::default().fg(Theme::TEXT_MUTED),
        )));
        frame.render_widget(placeholder, list_inner_area);
    } else {
        let visible_count = list_inner_area.height as usize;
        let scroll_offset = if state.list_index >= visible_count {
            state.list_index - visible_count + 1
        } else {
            0
        };

        let items: Vec<ListItem<'_>> = state
            .bookmarks
            .iter()
            .zip(state.selected.iter())
            .zip(state.worktree.iter())
            .enumerate()
            .skip(scroll_offset)
            .take(visible_count)
            .map(|(i, ((path, &checked), &is_wt))| {
                let is_cursor = i == state.list_index && list_focused;
                let style = if is_cursor {
                    Theme::selected_item()
                } else {
                    Theme::normal_item()
                };
                let check = if checked { "[x] " } else { "[ ] " };
                let display = path.display().to_string();
                let mut spans = vec![Span::styled(format!("{check}{display}"), style)];
                if checked && is_wt {
                    spans.push(Span::styled(" [wt]", Style::default().fg(Theme::ACCENT)));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        frame.render_widget(List::new(items), list_inner_area);
    }

    // Path input
    render_text_field_with_suggestion(
        frame,
        chunks[1],
        "Add Repo Path",
        state.path_input,
        state.path_cursor,
        state.focus == RepoPickerFocus::Input,
        state.path_suggestion,
    );

    // Footer
    let footer = match state.focus {
        RepoPickerFocus::List => Line::from(vec![
            Span::styled("j/k", Theme::keybind()),
            Span::styled(" nav  ", Theme::keybind_desc()),
            Span::styled("Space", Theme::keybind()),
            Span::styled(" toggle  ", Theme::keybind_desc()),
            Span::styled("w", Theme::keybind()),
            Span::styled(" worktree  ", Theme::keybind_desc()),
            Span::styled("Tab", Theme::keybind()),
            Span::styled(" input  ", Theme::keybind_desc()),
            Span::styled("Enter", Theme::keybind()),
            Span::styled(" ok  ", Theme::keybind_desc()),
            Span::styled("Esc", Theme::keybind()),
            Span::styled(" cancel", Theme::keybind_desc()),
        ]),
        RepoPickerFocus::Input => {
            let tab_hint = if state.path_suggestion.is_some() {
                " complete  "
            } else {
                " list  "
            };
            Line::from(vec![
                Span::styled("Tab", Theme::keybind()),
                Span::styled(tab_hint, Theme::keybind_desc()),
                Span::styled("Enter", Theme::keybind()),
                Span::styled(" add repo  ", Theme::keybind_desc()),
                Span::styled("Esc", Theme::keybind()),
                Span::styled(" cancel", Theme::keybind_desc()),
            ])
        }
    };
    frame.render_widget(Paragraph::new(footer), chunks[2]);
}
