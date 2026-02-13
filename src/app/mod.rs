use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use tracing::error;

use crate::claude::{input, PtySession};
use crate::session::{SessionConfig, SessionInfo, SessionStatus};
use crate::ui::{info_panel, layout, session_list, status_bar, terminal_view};

pub enum AppMessage {
    KeyPress(KeyCode, KeyModifiers),
    Resize(u16, u16),
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFocus {
    SessionList,
    Terminal,
}

pub struct App {
    sessions: Vec<PtySession>,
    active_index: usize,
    focus: InputFocus,
    should_quit: bool,
    error_message: Option<String>,
    terminal_rows: u16,
    terminal_cols: u16,
    session_counter: usize,
    show_info_panel: bool,
    show_help: bool,
}

impl App {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            sessions: Vec::new(),
            active_index: 0,
            focus: InputFocus::Terminal,
            should_quit: false,
            error_message: None,
            terminal_rows: rows,
            terminal_cols: cols,
            session_counter: 0,
            show_info_panel: false,
            show_help: false,
        }
    }

    pub fn spawn_initial_session(&mut self) {
        self.spawn_session();
    }

    fn spawn_session(&mut self) {
        self.spawn_session_with_config(&SessionConfig::default());
    }

    fn spawn_session_with_config(&mut self, config: &SessionConfig) {
        self.session_counter += 1;
        let name = format!("Session {}", self.session_counter);
        let (rows, cols) = self.content_area_size();

        match PtySession::spawn_with_config(name, rows, cols, config) {
            Ok(session) => {
                self.sessions.push(session);
                self.active_index = self.sessions.len() - 1;
                self.focus = InputFocus::Terminal;
                self.error_message = None;
            }
            Err(e) => {
                error!("Failed to spawn session: {e}");
                self.error_message = Some(format!("Failed to start claude: {e:#}"));
            }
        }
    }

    fn close_active_session(&mut self) {
        if self.sessions.is_empty() {
            return;
        }

        self.sessions.remove(self.active_index);

        if self.sessions.is_empty() {
            self.active_index = 0;
        } else if self.active_index >= self.sessions.len() {
            self.active_index = self.sessions.len() - 1;
        }
    }

    pub fn update(&mut self, msg: AppMessage) {
        match msg {
            AppMessage::Quit => self.should_quit = true,
            AppMessage::KeyPress(code, mods) => self.handle_key(code, mods),
            AppMessage::Resize(cols, rows) => self.handle_resize(cols, rows),
        }
    }

    fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // Dismiss help overlay with Esc
        if self.show_help {
            if code == KeyCode::Esc {
                self.show_help = false;
            }
            return;
        }

        // Global keybindings (always active)
        if mods.contains(KeyModifiers::CONTROL) {
            match code {
                KeyCode::Char('q') => {
                    self.should_quit = true;
                    return;
                }
                KeyCode::Char('n') => {
                    self.spawn_session();
                    return;
                }
                KeyCode::Char('x') => {
                    self.close_active_session();
                    return;
                }
                KeyCode::Char('l') => {
                    self.focus = match self.focus {
                        InputFocus::SessionList => InputFocus::Terminal,
                        InputFocus::Terminal => InputFocus::SessionList,
                    };
                    return;
                }
                KeyCode::Char('i') => {
                    if self.terminal_cols >= 120 {
                        self.show_info_panel = !self.show_info_panel;
                    }
                    return;
                }
                _ => {}
            }
        }

        // Ctrl+? (Ctrl+Shift+/) for help
        if code == KeyCode::Char('?') {
            self.show_help = true;
            return;
        }

        match self.focus {
            InputFocus::SessionList => self.handle_list_key(code),
            InputFocus::Terminal => self.handle_terminal_key(code, mods),
        }
    }

    fn handle_list_key(&mut self, code: KeyCode) {
        if self.sessions.is_empty() {
            return;
        }

        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                if self.active_index + 1 < self.sessions.len() {
                    self.active_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.active_index > 0 {
                    self.active_index -= 1;
                }
            }
            KeyCode::Enter => {
                self.focus = InputFocus::Terminal;
            }
            _ => {}
        }
    }

    fn handle_terminal_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        if let Some(session) = self.sessions.get(self.active_index) {
            if let Some(bytes) = input::key_to_bytes(code, mods) {
                if let Err(e) = session.send_input(bytes) {
                    error!("Failed to send input: {e}");
                }
            }
        }
    }

    fn handle_resize(&mut self, cols: u16, rows: u16) {
        self.terminal_cols = cols;
        self.terminal_rows = rows;

        // Collapse info panel if terminal gets too narrow
        if cols < 120 {
            self.show_info_panel = false;
        }

        let (r, c) = self.content_area_size();
        for session in &self.sessions {
            session.resize(r, c);
        }
    }

    pub fn tick(&mut self) {
        for session in &mut self.sessions {
            if session.has_exited() && session.info.status == SessionStatus::Running {
                session.info.status = SessionStatus::Idle;
            }
        }
    }

    pub fn view(&self, frame: &mut Frame) {
        let areas = layout::compute_layout(frame.area(), self.show_info_panel);

        status_bar::render_header(frame, areas.header);

        // Session list
        if let Some(list_area) = areas.session_list {
            let infos: Vec<&SessionInfo> = self.sessions.iter().map(|s| &s.info).collect();
            session_list::render_session_list(
                frame,
                list_area,
                &infos,
                self.active_index,
                self.focus == InputFocus::SessionList,
            );
        }

        // Info panel
        if let Some(info_area) = areas.info_panel {
            if let Some(session) = self.sessions.get(self.active_index) {
                info_panel::render_info_panel(frame, info_area, &session.info);
            }
        }

        // Terminal
        match self.sessions.get(self.active_index) {
            Some(session) => {
                if let Ok(parser) = session.parser.lock() {
                    terminal_view::render_terminal(
                        frame,
                        areas.terminal,
                        &parser,
                        &session.info,
                        self.focus == InputFocus::Terminal,
                    );
                }
            }
            None => terminal_view::render_empty_terminal(frame, areas.terminal),
        }

        status_bar::render_footer(
            frame,
            areas.footer,
            self.sessions.len(),
            self.error_message.as_deref(),
        );

        // Help overlay (rendered last, on top of everything)
        if self.show_help {
            render_help_overlay(frame);
        }
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn shutdown(self) {
        for session in self.sessions {
            session.shutdown();
        }
    }

    fn content_area_size(&self) -> (u16, u16) {
        // Header: 1 line, Footer: 1 line, Borders: 2 lines top+bottom
        let rows = self.terminal_rows.saturating_sub(4);
        // Borders: 2 cols left+right, session list ~20%
        let list_width = if self.terminal_cols >= 80 {
            self.terminal_cols / 5
        } else {
            0
        };
        let info_width = if self.show_info_panel && self.terminal_cols >= 120 {
            self.terminal_cols * 15 / 100
        } else {
            0
        };
        let cols = self
            .terminal_cols
            .saturating_sub(list_width + info_width + 2);
        (rows, cols)
    }
}

fn render_help_overlay(frame: &mut Frame) {
    let area = centered_rect(60, 70, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Keybindings ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let help_lines = vec![
        Line::from(Span::styled(
            "Global",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        help_line("Ctrl+Q", "Quit"),
        help_line("Ctrl+N", "New session"),
        help_line("Ctrl+X", "Close active session"),
        help_line("Ctrl+L", "Toggle focus (list / terminal)"),
        help_line("Ctrl+I", "Toggle info panel (width >= 120)"),
        help_line("?", "Show this help"),
        Line::from(""),
        Line::from(Span::styled(
            "Session List (when focused)",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        help_line("j / Down", "Next session"),
        help_line("k / Up", "Previous session"),
        help_line("Enter", "Activate session & focus terminal"),
        Line::from(""),
        Line::from(Span::styled(
            "Terminal (when focused)",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        help_line("*", "All keys forwarded to PTY"),
        Line::from(""),
        Line::from(Span::styled(
            "Press Esc to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let paragraph = Paragraph::new(help_lines).block(block);
    frame.render_widget(paragraph, area);
}

fn help_line<'a>(key: &'a str, desc: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("  {key:<16}"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(desc, Style::default().fg(Color::White)),
    ])
}

/// Create a centered rectangle within the given area.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
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
