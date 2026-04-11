use std::path::PathBuf;

use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

use super::render_modal_frame;
use super::theme::Theme;
use super::{centered_fixed_height_rect, render_text_field, render_text_field_with_suggestion};
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
    pub search_query: &'a str,
    pub search_cursor: usize,
    pub search_active: bool,
    pub filtered_indices: &'a [usize],
}

pub fn render_repo_picker_modal(frame: &mut Frame, state: &RepoPickerState<'_>) {
    let visible_count = if state.filtered_indices.is_empty() {
        1
    } else {
        state.filtered_indices.len().min(10)
    };
    let list_height = visible_count as u16 + 2; // +2 for borders

    let search_height: u16 = if state.search_active { 3 } else { 0 };

    // Layout: search(optional 3) + list + path input(3) + footer(1) + outer border(2)
    let total_height = search_height + list_height + 3 + 1 + 2;

    let area = centered_fixed_height_rect(60, total_height, frame.area());

    let inner = render_modal_frame(frame, area, "Select Repos");

    let mut constraints = Vec::new();
    if state.search_active {
        constraints.push(Constraint::Length(3)); // Search bar
    }
    constraints.push(Constraint::Length(list_height)); // Bookmark list
    constraints.push(Constraint::Length(3)); // Path input
    constraints.push(Constraint::Min(1)); // Footer

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    // Assign named chunk areas based on whether search bar is visible.
    let (search_area, list_area, input_area, footer_area) = if state.search_active {
        (Some(chunks[0]), chunks[1], chunks[2], chunks[3])
    } else {
        (None, chunks[0], chunks[1], chunks[2])
    };

    // Search bar (when active)
    if let Some(area) = search_area {
        let match_label = format!(
            "Search ({}/{})",
            state.filtered_indices.len(),
            state.bookmarks.len()
        );
        render_text_field(
            frame,
            area,
            &match_label,
            state.search_query,
            state.search_cursor,
            state.focus == RepoPickerFocus::Search,
        );
    }

    // Bookmark list with checkboxes
    let list_focused = state.focus == RepoPickerFocus::List;
    let border_color = if list_focused {
        Theme::BORDER_FOCUSED
    } else {
        Theme::BORDER_UNFOCUSED
    };

    let title = format!(" Repos ({}) ", state.bookmarks.len());

    let list_block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let list_inner_area = list_block.inner(list_area);
    frame.render_widget(list_block, list_area);

    if state.filtered_indices.is_empty() {
        let msg = if state.search_query.is_empty() {
            "  No bookmarks — add via path input below"
        } else {
            "  No matches"
        };
        let placeholder = Paragraph::new(Line::from(Span::styled(
            msg,
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
            .filtered_indices
            .iter()
            .enumerate()
            .skip(scroll_offset)
            .take(visible_count)
            .map(|(vi, &real_idx)| {
                let path = &state.bookmarks[real_idx];
                let checked = state.selected[real_idx];
                let is_wt = state.worktree[real_idx];
                let is_cursor = vi == state.list_index && list_focused;

                let style = if is_cursor {
                    Theme::selected_item()
                } else {
                    Theme::normal_item()
                };

                let check = if checked { "[x] " } else { "[ ] " };
                let display = path.display().to_string();

                let mut spans = if !state.search_query.is_empty() {
                    // Build spans with fuzzy highlight
                    let positions = crate::fuzzy::fuzzy_match(state.search_query, &display)
                        .map(|m| m.positions)
                        .unwrap_or_default();
                    let mut result = vec![Span::styled(check, style)];
                    let mut last = 0;
                    for &pos in &positions {
                        if pos > last {
                            result.push(Span::styled(display[last..pos].to_string(), style));
                        }
                        let end = display[pos..]
                            .chars()
                            .next()
                            .map(|c| pos + c.len_utf8())
                            .unwrap_or(pos + 1);
                        result.push(Span::styled(
                            display[pos..end].to_string(),
                            Style::default().fg(Theme::ACCENT),
                        ));
                        last = end;
                    }
                    if last < display.len() {
                        result.push(Span::styled(display[last..].to_string(), style));
                    }
                    result
                } else {
                    vec![Span::styled(format!("{check}{display}"), style)]
                };

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
        input_area,
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
            Span::styled("/", Theme::keybind()),
            Span::styled(" search  ", Theme::keybind_desc()),
            Span::styled("d", Theme::keybind()),
            Span::styled(" delete  ", Theme::keybind_desc()),
            Span::styled("Tab", Theme::keybind()),
            Span::styled(" input  ", Theme::keybind_desc()),
            Span::styled("Enter", Theme::keybind()),
            Span::styled(" ok", Theme::keybind_desc()),
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
        RepoPickerFocus::Search => Line::from(vec![
            Span::styled("Enter", Theme::keybind()),
            Span::styled(" keep filter  ", Theme::keybind_desc()),
            Span::styled("Esc", Theme::keybind()),
            Span::styled(" clear  ", Theme::keybind_desc()),
        ]),
    };
    frame.render_widget(Paragraph::new(footer), footer_area);
}
