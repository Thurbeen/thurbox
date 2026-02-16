// Modal state management for Thurbox TUI.
// This module consolidates all modal-related state into type-safe enums,
// replacing 14+ boolean flags with a single discriminated union.

#![allow(dead_code)] // Public API used in refactoring steps

use std::path::PathBuf;

use crate::session::{RoleConfig, SessionConfig, WorktreeInfo};
use crate::ui::role_editor_modal;

// ── TextInput Helper ────────────────────────────────────────────────────────

/// Simple text input state with cursor tracking.
#[derive(Debug, Clone)]
pub struct TextInput {
    buffer: String,
    cursor: usize,
}

impl TextInput {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
        }
    }

    pub fn insert(&mut self, c: char) {
        let byte_pos = self.byte_offset();
        self.buffer.insert(byte_pos, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let byte_pos = self.byte_offset();
            self.buffer.remove(byte_pos);
        }
    }

    pub fn delete(&mut self) {
        let byte_pos = self.byte_offset();
        if byte_pos < self.buffer.len() {
            self.buffer.remove(byte_pos);
        }
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        let char_count = self.buffer.chars().count();
        if self.cursor < char_count {
            self.cursor += 1;
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.buffer.chars().count();
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    pub fn set(&mut self, value: &str) {
        self.buffer = value.to_string();
        self.cursor = value.chars().count();
    }

    pub fn value(&self) -> &str {
        &self.buffer
    }

    pub fn cursor_pos(&self) -> usize {
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

impl Default for TextInput {
    fn default() -> Self {
        Self::new()
    }
}

// ── AddProjectField ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddProjectField {
    Name,
    Path,
}

// ── Modal State Structs ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AddProjectModal {
    pub name: TextInput,
    pub path: TextInput,
    pub field: AddProjectField,
}

impl AddProjectModal {
    pub fn new() -> Self {
        Self {
            name: TextInput::new(),
            path: TextInput::new(),
            field: AddProjectField::Name,
        }
    }
}

impl Default for AddProjectModal {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct RepoSelectorModal {
    pub index: usize,
}

impl RepoSelectorModal {
    pub fn new() -> Self {
        Self { index: 0 }
    }
}

impl Default for RepoSelectorModal {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct SessionModeModal {
    pub index: usize,
}

impl SessionModeModal {
    pub fn new() -> Self {
        Self { index: 0 }
    }
}

impl Default for SessionModeModal {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct BranchSelectorModal {
    pub index: usize,
    pub branches: Vec<String>,
    pub pending_repo_path: Option<PathBuf>,
}

impl BranchSelectorModal {
    pub fn new() -> Self {
        Self {
            index: 0,
            branches: Vec::new(),
            pending_repo_path: None,
        }
    }

    pub fn with_branches(branches: Vec<String>) -> Self {
        Self {
            index: 0,
            branches,
            pending_repo_path: None,
        }
    }
}

impl Default for BranchSelectorModal {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct WorktreeNameModal {
    pub name: TextInput,
    pub pending_base_branch: Option<String>,
}

impl WorktreeNameModal {
    pub fn new() -> Self {
        Self {
            name: TextInput::new(),
            pending_base_branch: None,
        }
    }
}

impl Default for WorktreeNameModal {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct RoleSelectorModal {
    pub index: usize,
    pub pending_spawn_config: Option<SessionConfig>,
    pub pending_spawn_worktree: Option<WorktreeInfo>,
    pub pending_spawn_name: Option<String>,
}

impl RoleSelectorModal {
    pub fn new() -> Self {
        Self {
            index: 0,
            pending_spawn_config: None,
            pending_spawn_worktree: None,
            pending_spawn_name: None,
        }
    }
}

impl Default for RoleSelectorModal {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleEditorView {
    List,
    Editor,
}

#[derive(Debug, Clone)]
pub struct RoleEditorModal {
    pub view: RoleEditorView,
    pub list_index: usize,
    pub roles: Vec<RoleConfig>,
    pub field: role_editor_modal::RoleEditorField,
    pub name: TextInput,
    pub description: TextInput,
    pub allowed_tools: ToolListState,
    pub disallowed_tools: ToolListState,
    pub system_prompt: TextInput,
    pub editing_index: Option<usize>,
}

impl RoleEditorModal {
    pub fn new() -> Self {
        Self {
            view: RoleEditorView::List,
            list_index: 0,
            roles: Vec::new(),
            field: role_editor_modal::RoleEditorField::Name,
            name: TextInput::new(),
            description: TextInput::new(),
            allowed_tools: ToolListState::new(),
            disallowed_tools: ToolListState::new(),
            system_prompt: TextInput::new(),
            editing_index: None,
        }
    }

    pub fn reset(&mut self) {
        self.view = RoleEditorView::List;
        self.list_index = 0;
        self.roles.clear();
        self.field = role_editor_modal::RoleEditorField::Name;
        self.name.clear();
        self.description.clear();
        self.allowed_tools.reset();
        self.disallowed_tools.reset();
        self.system_prompt.clear();
        self.editing_index = None;
    }
}

impl Default for RoleEditorModal {
    fn default() -> Self {
        Self::new()
    }
}

// ── ToolListState ──────────────────────────────────────────────────────────

/// State for an editable list of tool names (allowed or disallowed).
#[derive(Debug, Clone)]
pub struct ToolListState {
    items: Vec<String>,
    selected: usize,
    mode: role_editor_modal::ToolListMode,
    input: TextInput,
}

impl ToolListState {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            mode: role_editor_modal::ToolListMode::Browse,
            input: TextInput::new(),
        }
    }

    pub fn reset(&mut self) {
        self.items.clear();
        self.selected = 0;
        self.mode = role_editor_modal::ToolListMode::Browse;
        self.input.clear();
    }

    pub fn load(&mut self, tools: &[String]) {
        self.items = tools.to_vec();
        self.selected = 0;
        self.mode = role_editor_modal::ToolListMode::Browse;
        self.input.clear();
    }

    pub fn start_adding(&mut self) {
        self.mode = role_editor_modal::ToolListMode::Adding;
        self.input.clear();
    }

    pub fn confirm_add(&mut self) {
        let val = self.input.value().trim().to_string();
        if !val.is_empty() {
            self.items.push(val);
            self.selected = self.items.len() - 1;
        }
        self.mode = role_editor_modal::ToolListMode::Browse;
    }

    pub fn cancel_add(&mut self) {
        self.mode = role_editor_modal::ToolListMode::Browse;
    }

    pub fn delete_selected(&mut self) {
        if !self.items.is_empty() {
            self.items.remove(self.selected);
            if self.selected >= self.items.len() && self.selected > 0 {
                self.selected -= 1;
            }
        }
    }

    pub fn move_down(&mut self) {
        if !self.items.is_empty() && self.selected + 1 < self.items.len() {
            self.selected += 1;
        }
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn items(&self) -> &[String] {
        &self.items
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn mode(&self) -> role_editor_modal::ToolListMode {
        self.mode
    }

    pub fn input_mut(&mut self) -> &mut TextInput {
        &mut self.input
    }

    pub fn input(&self) -> &TextInput {
        &self.input
    }
}

impl Default for ToolListState {
    fn default() -> Self {
        Self::new()
    }
}

// ── Main Modal Enum ────────────────────────────────────────────────────────

/// Single, discriminated union replacing 14+ boolean flags.
/// Only one modal can be active at a time, making invalid states unrepresentable.
#[derive(Debug, Clone, Default)]
pub enum Modal {
    #[default]
    None,
    Help,
    AddProject(AddProjectModal),
    RepoSelector(RepoSelectorModal),
    SessionMode(SessionModeModal),
    BranchSelector(BranchSelectorModal),
    WorktreeName(WorktreeNameModal),
    RoleSelector(RoleSelectorModal),
    RoleEditor(RoleEditorModal),
}

impl Modal {
    pub fn is_open(&self) -> bool {
        !matches!(self, Modal::None)
    }

    pub fn close(&mut self) {
        *self = Modal::None;
    }
}

// ── List Navigation Helper ─────────────────────────────────────────────────

/// Direction for list navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

/// Navigate a list index in the given direction, clamping to valid bounds.
/// This consolidates 9+ duplicated navigation patterns into a single function.
pub fn navigate_list(index: &mut usize, direction: Direction, max: usize) {
    match direction {
        Direction::Down => {
            if *index + 1 < max {
                *index += 1;
            }
        }
        Direction::Up => {
            *index = index.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_input_basic() {
        let mut input = TextInput::new();
        input.insert('a');
        input.insert('b');
        input.insert('c');
        assert_eq!(input.value(), "abc");
        assert_eq!(input.cursor_pos(), 3);
    }

    #[test]
    fn test_text_input_backspace() {
        let mut input = TextInput::new();
        input.set("hello");
        input.backspace();
        assert_eq!(input.value(), "hell");
        assert_eq!(input.cursor_pos(), 4);
    }

    #[test]
    fn test_text_input_cursor_movement() {
        let mut input = TextInput::new();
        input.set("hello");
        assert_eq!(input.cursor_pos(), 5);

        input.move_left();
        assert_eq!(input.cursor_pos(), 4);

        input.move_left();
        assert_eq!(input.cursor_pos(), 3);

        input.move_right();
        assert_eq!(input.cursor_pos(), 4);

        input.home();
        assert_eq!(input.cursor_pos(), 0);

        input.end();
        assert_eq!(input.cursor_pos(), 5);
    }

    #[test]
    fn test_navigate_list_down() {
        let mut index = 0;
        navigate_list(&mut index, Direction::Down, 5);
        assert_eq!(index, 1);

        navigate_list(&mut index, Direction::Down, 5);
        assert_eq!(index, 2);
    }

    #[test]
    fn test_navigate_list_down_at_max() {
        let mut index = 4;
        navigate_list(&mut index, Direction::Down, 5);
        assert_eq!(index, 4); // Stays at max-1
    }

    #[test]
    fn test_navigate_list_up() {
        let mut index = 3;
        navigate_list(&mut index, Direction::Up, 5);
        assert_eq!(index, 2);

        navigate_list(&mut index, Direction::Up, 5);
        assert_eq!(index, 1);
    }

    #[test]
    fn test_navigate_list_up_at_min() {
        let mut index = 0;
        navigate_list(&mut index, Direction::Up, 5);
        assert_eq!(index, 0); // Stays at 0
    }

    #[test]
    fn test_modal_default_is_none() {
        let modal = Modal::default();
        assert!(matches!(modal, Modal::None));
        assert!(!modal.is_open());
    }

    #[test]
    fn test_modal_help_is_open() {
        let modal = Modal::Help;
        assert!(modal.is_open());
    }

    #[test]
    fn test_modal_close() {
        let mut modal = Modal::Help;
        assert!(modal.is_open());
        modal.close();
        assert!(!modal.is_open());
    }

    #[test]
    fn test_add_project_modal_default() {
        let modal = AddProjectModal::default();
        assert_eq!(modal.name.value(), "");
        assert_eq!(modal.path.value(), "");
        assert_eq!(modal.field, AddProjectField::Name);
    }

    #[test]
    fn test_tool_list_state_operations() {
        let mut state = ToolListState::new();
        state.load(&["tool1".to_string(), "tool2".to_string()]);
        assert_eq!(state.items().len(), 2);
        assert_eq!(state.selected(), 0);

        state.move_down();
        assert_eq!(state.selected(), 1);

        state.move_up();
        assert_eq!(state.selected(), 0);
    }
}
