use std::path::PathBuf;

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
use crate::project::{self, ProjectConfig, ProjectInfo};
use crate::session::{SessionConfig, SessionInfo, SessionStatus};
use crate::ui::{
    add_project_modal, info_panel, layout, project_list, repo_selector_modal, status_bar,
    terminal_view,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddProjectField {
    Name,
    Path,
}

struct TextInput {
    buffer: String,
    cursor: usize,
}

impl TextInput {
    fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
        }
    }

    fn insert(&mut self, c: char) {
        let byte_pos = self.byte_offset();
        self.buffer.insert(byte_pos, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let byte_pos = self.byte_offset();
            self.buffer.remove(byte_pos);
        }
    }

    fn delete(&mut self) {
        let byte_pos = self.byte_offset();
        if byte_pos < self.buffer.len() {
            self.buffer.remove(byte_pos);
        }
    }

    fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_right(&mut self) {
        let char_count = self.buffer.chars().count();
        if self.cursor < char_count {
            self.cursor += 1;
        }
    }

    fn home(&mut self) {
        self.cursor = 0;
    }

    fn end(&mut self) {
        self.cursor = self.buffer.chars().count();
    }

    fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    fn value(&self) -> &str {
        &self.buffer
    }

    fn cursor_pos(&self) -> usize {
        self.cursor
    }

    /// Convert char-based cursor position to byte offset.
    fn byte_offset(&self) -> usize {
        self.buffer
            .char_indices()
            .nth(self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.buffer.len())
    }
}

pub enum AppMessage {
    KeyPress(KeyCode, KeyModifiers),
    Resize(u16, u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFocus {
    ProjectList,
    SessionList,
    Terminal,
}

pub struct App {
    projects: Vec<ProjectInfo>,
    active_project_index: usize,
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
    show_add_project_modal: bool,
    add_project_name: TextInput,
    add_project_path: TextInput,
    add_project_field: AddProjectField,
    show_repo_selector: bool,
    repo_selector_index: usize,
}

impl App {
    pub fn new(rows: u16, cols: u16, project_configs: Vec<ProjectConfig>) -> Self {
        let projects: Vec<ProjectInfo> = if project_configs.is_empty() {
            vec![ProjectInfo::new_default(project::create_default_project())]
        } else {
            project_configs.into_iter().map(ProjectInfo::new).collect()
        };

        Self {
            projects,
            active_project_index: 0,
            sessions: Vec::new(),
            active_index: 0,
            focus: InputFocus::ProjectList,
            should_quit: false,
            error_message: None,
            terminal_rows: rows,
            terminal_cols: cols,
            session_counter: 0,
            show_info_panel: false,
            show_help: false,
            show_add_project_modal: false,
            add_project_name: TextInput::new(),
            add_project_path: TextInput::new(),
            add_project_field: AddProjectField::Name,
            show_repo_selector: false,
            repo_selector_index: 0,
        }
    }

    pub fn spawn_session(&mut self) {
        let repos = &self.projects[self.active_project_index].config.repos;
        match repos.len() {
            0 => {
                let mut config = SessionConfig::default();
                if let Some(home) = std::env::var_os("HOME") {
                    config.cwd = Some(PathBuf::from(home));
                }
                self.spawn_session_with_config(&config);
            }
            1 => {
                let config = SessionConfig {
                    cwd: Some(repos[0].clone()),
                    ..SessionConfig::default()
                };
                self.spawn_session_with_config(&config);
            }
            _ => {
                self.repo_selector_index = 0;
                self.show_repo_selector = true;
            }
        }
    }

    fn spawn_session_in_repo(&mut self, repo_path: PathBuf) {
        let config = SessionConfig {
            cwd: Some(repo_path),
            ..SessionConfig::default()
        };
        self.spawn_session_with_config(&config);
    }

    fn spawn_session_with_config(&mut self, config: &SessionConfig) {
        self.session_counter += 1;
        let name = format!("Session {}", self.session_counter);
        let (rows, cols) = self.content_area_size();

        match PtySession::spawn_with_config(name, rows, cols, config) {
            Ok(session) => {
                let session_id = session.info.id;
                self.sessions.push(session);
                self.active_index = self.sessions.len() - 1;
                self.focus = InputFocus::Terminal;
                self.error_message = None;

                // Track session in the active project
                if let Some(project) = self.projects.get_mut(self.active_project_index) {
                    project.session_ids.push(session_id);
                }
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

        let session_id = self.sessions[self.active_index].info.id;
        self.sessions.remove(self.active_index);

        // Remove session from its project
        for project in &mut self.projects {
            project.session_ids.retain(|id| *id != session_id);
        }

        if self.sessions.is_empty() {
            self.active_index = 0;
        } else if self.active_index >= self.sessions.len() {
            self.active_index = self.sessions.len() - 1;
        }
    }

    /// Get sessions belonging to the active project.
    fn active_project_sessions(&self) -> Vec<usize> {
        let project = &self.projects[self.active_project_index];
        self.sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| project.session_ids.contains(&s.info.id))
            .map(|(i, _)| i)
            .collect()
    }

    /// Get the active session's index within the active project's session list.
    fn active_session_in_project(&self) -> usize {
        let project_sessions = self.active_project_sessions();
        project_sessions
            .iter()
            .position(|&i| i == self.active_index)
            .unwrap_or(0)
    }

    pub fn update(&mut self, msg: AppMessage) {
        match msg {
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

        // Repo selector modal captures all input
        if self.show_repo_selector {
            self.handle_repo_selector_key(code);
            return;
        }

        // Add-project modal captures all input
        if self.show_add_project_modal {
            self.handle_add_project_key(code);
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
                    if self.focus == InputFocus::ProjectList {
                        self.show_add_project_modal = true;
                        self.add_project_field = AddProjectField::Name;
                    } else {
                        self.spawn_session();
                    }
                    return;
                }
                KeyCode::Char('x') => {
                    self.close_active_session();
                    return;
                }
                KeyCode::Char('l') => {
                    self.focus = match self.focus {
                        InputFocus::ProjectList => InputFocus::SessionList,
                        InputFocus::SessionList => InputFocus::Terminal,
                        InputFocus::Terminal => InputFocus::ProjectList,
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

        match self.focus {
            InputFocus::ProjectList => self.handle_project_list_key(code),
            InputFocus::SessionList => self.handle_session_list_key(code),
            InputFocus::Terminal => self.handle_terminal_key(code, mods),
        }
    }

    fn handle_project_list_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                if self.active_project_index + 1 < self.projects.len() {
                    self.active_project_index += 1;
                    self.sync_active_session_to_project();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.active_project_index > 0 {
                    self.active_project_index -= 1;
                    self.sync_active_session_to_project();
                }
            }
            KeyCode::Enter => {
                self.focus = InputFocus::SessionList;
            }
            KeyCode::Char('?') => {
                self.show_help = true;
            }
            _ => {}
        }
    }

    fn handle_session_list_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('?') => {
                self.show_help = true;
                return;
            }
            KeyCode::Enter => {
                self.focus = InputFocus::Terminal;
                return;
            }
            _ => {}
        }

        let project_sessions = self.active_project_sessions();
        if project_sessions.is_empty() {
            return;
        }

        let current_pos = project_sessions
            .iter()
            .position(|&i| i == self.active_index)
            .unwrap_or(0);

        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                if current_pos + 1 < project_sessions.len() {
                    self.active_index = project_sessions[current_pos + 1];
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if current_pos > 0 {
                    self.active_index = project_sessions[current_pos - 1];
                }
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

    fn handle_add_project_key(&mut self, code: KeyCode) {
        let field = match self.add_project_field {
            AddProjectField::Name => &mut self.add_project_name,
            AddProjectField::Path => &mut self.add_project_path,
        };

        match code {
            KeyCode::Esc => {
                self.show_add_project_modal = false;
                self.add_project_name.clear();
                self.add_project_path.clear();
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.add_project_field = match self.add_project_field {
                    AddProjectField::Name => AddProjectField::Path,
                    AddProjectField::Path => AddProjectField::Name,
                };
            }
            KeyCode::Enter => {
                self.submit_add_project();
            }
            KeyCode::Backspace => {
                field.backspace();
            }
            KeyCode::Delete => {
                field.delete();
            }
            KeyCode::Left => {
                field.move_left();
            }
            KeyCode::Right => {
                field.move_right();
            }
            KeyCode::Home => {
                field.home();
            }
            KeyCode::End => {
                field.end();
            }
            KeyCode::Char(c) => {
                field.insert(c);
            }
            _ => {}
        }
    }

    fn handle_repo_selector_key(&mut self, code: KeyCode) {
        let repo_count = self.projects[self.active_project_index].config.repos.len();
        match code {
            KeyCode::Esc => {
                self.show_repo_selector = false;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.repo_selector_index + 1 < repo_count {
                    self.repo_selector_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.repo_selector_index = self.repo_selector_index.saturating_sub(1);
            }
            KeyCode::Enter => {
                let path = self.projects[self.active_project_index].config.repos
                    [self.repo_selector_index]
                    .clone();
                self.show_repo_selector = false;
                self.spawn_session_in_repo(path);
            }
            _ => {}
        }
    }

    fn submit_add_project(&mut self) {
        let name = self.add_project_name.value().trim().to_string();
        let path = self.add_project_path.value().trim().to_string();

        if name.is_empty() || path.is_empty() {
            self.error_message = Some("Project name and path cannot be empty".to_string());
            return;
        }

        let config = ProjectConfig {
            name,
            repos: vec![PathBuf::from(path)],
        };
        let info = ProjectInfo::new(config);
        self.projects.push(info);
        self.active_project_index = self.projects.len() - 1;

        // Persist to config file (exclude default projects)
        let configs: Vec<ProjectConfig> = self
            .projects
            .iter()
            .filter(|p| !p.is_default)
            .map(|p| p.config.clone())
            .collect();
        if let Err(e) = project::save_project_configs(&configs) {
            error!("Failed to save config: {e}");
            self.error_message = Some(format!("Project added but failed to save config: {e}"));
        } else {
            self.error_message = None;
        }

        // Close modal and clear inputs
        self.show_add_project_modal = false;
        self.add_project_name.clear();
        self.add_project_path.clear();
    }

    /// When switching projects, select the first session of the new project.
    fn sync_active_session_to_project(&mut self) {
        let project_sessions = self.active_project_sessions();
        if let Some(&first) = project_sessions.first() {
            self.active_index = first;
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

        // Left panel (projects + sessions)
        if let Some(left_area) = areas.left_panel {
            let project_entries: Vec<project_list::ProjectEntry<'_>> = self
                .projects
                .iter()
                .map(|p| project_list::ProjectEntry {
                    name: &p.config.name,
                    session_count: p.session_ids.len(),
                })
                .collect();

            let project_session_indices = self.active_project_sessions();
            let project_sessions: Vec<&SessionInfo> = project_session_indices
                .iter()
                .map(|&i| &self.sessions[i].info)
                .collect();

            let panel_focus = match self.focus {
                InputFocus::ProjectList => project_list::LeftPanelFocus::Projects,
                InputFocus::SessionList | InputFocus::Terminal => {
                    project_list::LeftPanelFocus::Sessions
                }
            };

            project_list::render_left_panel(
                frame,
                left_area,
                &project_list::LeftPanelState {
                    projects: &project_entries,
                    active_project: self.active_project_index,
                    sessions: &project_sessions,
                    active_session: self.active_session_in_project(),
                    focus: panel_focus,
                    panel_focused: self.focus != InputFocus::Terminal,
                },
            );
        }

        // Info panel
        if let Some(info_area) = areas.info_panel {
            let active_project = self.projects.get(self.active_project_index);
            if let Some(session) = self.sessions.get(self.active_index) {
                info_panel::render_info_panel(
                    frame,
                    info_area,
                    &session.info,
                    active_project.map(|p| &p.config),
                );
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

        let focus_label = match self.focus {
            InputFocus::ProjectList => "Projects",
            InputFocus::SessionList => "Sessions",
            InputFocus::Terminal => "Terminal",
        };
        status_bar::render_footer(
            frame,
            areas.footer,
            self.sessions.len(),
            self.projects.len(),
            self.error_message.as_deref(),
            focus_label,
        );

        // Help overlay (rendered last, on top of everything)
        if self.show_help {
            render_help_overlay(frame);
        }

        // Add-project modal (on top of everything including help)
        if self.show_add_project_modal {
            add_project_modal::render_add_project_modal(
                frame,
                &add_project_modal::AddProjectModalState {
                    name: self.add_project_name.value(),
                    name_cursor: self.add_project_name.cursor_pos(),
                    path: self.add_project_path.value(),
                    path_cursor: self.add_project_path.cursor_pos(),
                    focused_field: self.add_project_field,
                },
            );
        }

        // Repo selector modal (highest z-order)
        if self.show_repo_selector {
            let active_project = &self.projects[self.active_project_index];
            repo_selector_modal::render_repo_selector_modal(
                frame,
                &repo_selector_modal::RepoSelectorState {
                    repos: &active_project.config.repos,
                    selected_index: self.repo_selector_index,
                },
            );
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

        let three_panel = self.show_info_panel && self.terminal_cols >= 120;

        let list_width = if self.terminal_cols >= 80 {
            if three_panel {
                self.terminal_cols * 15 / 100 // matches layout.rs 3-panel: 15%
            } else {
                self.terminal_cols * 20 / 100 // matches layout.rs 2-panel: 20%
            }
        } else {
            0
        };
        let info_width = if three_panel {
            self.terminal_cols * 15 / 100 // matches layout.rs 3-panel: 15%
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
        help_line("Ctrl+N", "New project (project focus) / session"),
        help_line("Ctrl+X", "Close active session"),
        help_line("Ctrl+L", "Cycle focus (project / session / terminal)"),
        help_line("Ctrl+I", "Toggle info panel (width >= 120)"),
        help_line("?", "Show this help (list focus only)"),
        Line::from(""),
        Line::from(Span::styled(
            "Project List (when focused)",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        help_line("j / Down", "Next project"),
        help_line("k / Up", "Previous project"),
        help_line("Enter", "Focus session list"),
        Line::from(""),
        Line::from(Span::styled(
            "Session List (when focused)",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        help_line("j / Down", "Next session"),
        help_line("k / Up", "Previous session"),
        help_line("Enter", "Focus terminal"),
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

#[cfg(test)]
mod tests {
    use super::*;

    // --- TextInput tests ---

    #[test]
    fn text_input_new_is_empty() {
        let input = TextInput::new();
        assert_eq!(input.value(), "");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn text_input_insert_appends_chars() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.insert('c');
        assert_eq!(input.value(), "abc");
        assert_eq!(input.cursor_pos(), 3);
    }

    #[test]
    fn text_input_insert_at_middle() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('c');
        input.move_left();
        input.insert('b');
        assert_eq!(input.value(), "abc");
        assert_eq!(input.cursor_pos(), 2);
    }

    #[test]
    fn text_input_backspace_removes_before_cursor() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.insert('c');
        input.backspace();
        assert_eq!(input.value(), "ab");
        assert_eq!(input.cursor_pos(), 2);
    }

    #[test]
    fn text_input_backspace_at_start_is_noop() {
        let mut input = TextInput::new();
        input.insert('a');
        input.home();
        input.backspace();
        assert_eq!(input.value(), "a");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn text_input_delete_removes_at_cursor() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.insert('c');
        input.home();
        input.delete();
        assert_eq!(input.value(), "bc");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn text_input_delete_at_end_is_noop() {
        let mut input = TextInput::new();
        input.insert('a');
        input.delete();
        assert_eq!(input.value(), "a");
        assert_eq!(input.cursor_pos(), 1);
    }

    #[test]
    fn text_input_move_left_and_right() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        assert_eq!(input.cursor_pos(), 2);

        input.move_left();
        assert_eq!(input.cursor_pos(), 1);

        input.move_right();
        assert_eq!(input.cursor_pos(), 2);
    }

    #[test]
    fn text_input_move_left_at_zero_is_noop() {
        let mut input = TextInput::new();
        input.move_left();
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn text_input_move_right_at_end_is_noop() {
        let mut input = TextInput::new();
        input.insert('a');
        input.move_right();
        assert_eq!(input.cursor_pos(), 1);
    }

    #[test]
    fn text_input_home_and_end() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.insert('c');

        input.home();
        assert_eq!(input.cursor_pos(), 0);

        input.end();
        assert_eq!(input.cursor_pos(), 3);
    }

    #[test]
    fn text_input_clear_resets_buffer_and_cursor() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.clear();
        assert_eq!(input.value(), "");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn text_input_multibyte_chars() {
        let mut input = TextInput::new();
        input.insert('é');
        input.insert('ñ');
        assert_eq!(input.value(), "éñ");
        assert_eq!(input.cursor_pos(), 2);

        input.move_left();
        input.insert('ü');
        assert_eq!(input.value(), "éüñ");
        assert_eq!(input.cursor_pos(), 2);
    }

    #[test]
    fn text_input_backspace_multibyte() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('é');
        input.insert('b');
        input.backspace();
        assert_eq!(input.value(), "aé");

        input.backspace();
        assert_eq!(input.value(), "a");
    }

    #[test]
    fn text_input_delete_multibyte() {
        let mut input = TextInput::new();
        input.insert('é');
        input.insert('b');
        input.home();
        input.delete();
        assert_eq!(input.value(), "b");
        assert_eq!(input.cursor_pos(), 0);
    }
}
