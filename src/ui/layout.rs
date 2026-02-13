use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub struct PanelAreas {
    pub header: Rect,
    pub session_list: Option<Rect>,
    pub info_panel: Option<Rect>,
    pub terminal: Rect,
    pub footer: Rect,
}

pub fn compute_layout(area: Rect, show_info_panel: bool) -> PanelAreas {
    // Vertical split: header | content | footer
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    let header = vertical[0];
    let content = vertical[1];
    let footer = vertical[2];

    // If terminal is too narrow, show terminal only
    if area.width < 80 {
        return PanelAreas {
            header,
            session_list: None,
            info_panel: None,
            terminal: content,
            footer,
        };
    }

    if show_info_panel && area.width >= 120 {
        // 3-panel mode: 15% session list | 15% info | 70% terminal
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(15),
                Constraint::Percentage(15),
                Constraint::Percentage(70),
            ])
            .split(content);

        PanelAreas {
            header,
            session_list: Some(horizontal[0]),
            info_panel: Some(horizontal[1]),
            terminal: horizontal[2],
            footer,
        }
    } else {
        // 2-panel mode: 20% session list | 80% terminal
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
            .split(content);

        PanelAreas {
            header,
            session_list: Some(horizontal[0]),
            info_panel: None,
            terminal: horizontal[1],
            footer,
        }
    }
}
