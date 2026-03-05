use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

use super::centered_fixed_height_rect;
use super::project_list::render_scroll_indicators;
use super::theme::Theme;

pub struct BranchSelectorState<'a> {
    pub branches: &'a [String],
    pub selected_index: usize,
}

pub fn render_branch_selector_modal(frame: &mut Frame, state: &BranchSelectorState<'_>) {
    let height = (state.branches.len().min(15) + 4) as u16;
    let area = centered_fixed_height_rect(50, height, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Base Branch ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::ACCENT));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // Branch list
            Constraint::Length(1), // Footer
        ])
        .split(inner);

    let items: Vec<ListItem<'_>> = state
        .branches
        .iter()
        .map(|branch| {
            ListItem::new(Line::from(Span::styled(
                branch.as_str(),
                Theme::normal_item(),
            )))
        })
        .collect();

    let list = List::new(items)
        .highlight_symbol("▸ ")
        .highlight_style(Theme::selected_item());

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected_index));
    frame.render_stateful_widget(list, chunks[0], &mut list_state);

    // Compute a block-like rect around the list area for scroll indicators.
    // Expand by 1 on each side to cover the outer block's border lines.
    let indicator_area = Rect::new(
        chunks[0].x.saturating_sub(1),
        chunks[0].y.saturating_sub(1),
        chunks[0].width + 2,
        chunks[0].height + 2,
    );
    render_scroll_indicators(frame, indicator_area, state.branches.len(), &list_state, 1);

    let footer = Line::from(vec![
        Span::styled("j/k", Theme::keybind()),
        Span::styled(" navigate  ", Theme::keybind_desc()),
        Span::styled("Enter", Theme::keybind()),
        Span::styled(" select  ", Theme::keybind_desc()),
        Span::styled("Esc", Theme::keybind()),
        Span::styled(" cancel", Theme::keybind_desc()),
    ]);
    frame.render_widget(Paragraph::new(footer), chunks[1]);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The modal caps at 15 visible branches plus 4 lines of chrome
    /// (2 borders + 1 title line padding + 1 footer).
    #[test]
    fn modal_height_caps_at_15_branches() {
        let height = |n: usize| (n.min(15) + 4) as u16;
        assert_eq!(height(1), 5);
        assert_eq!(height(15), 19);
        assert_eq!(height(30), 19); // capped
        assert_eq!(height(0), 4);
    }

    #[test]
    fn branch_selector_state_holds_index() {
        let branches = vec!["main".to_string(), "dev".to_string(), "feature".to_string()];
        let state = BranchSelectorState {
            branches: &branches,
            selected_index: 2,
        };
        assert_eq!(state.selected_index, 2);
        assert_eq!(state.branches.len(), 3);
    }
}
