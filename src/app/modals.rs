// Modal state management for Thurbox TUI.
// This module consolidates all modal-related state into type-safe enums,
// replacing 14+ boolean flags with a single discriminated union.

#![allow(dead_code)] // Public API used in refactoring steps

use std::path::PathBuf;

use crate::session::{RoleConfig, SessionConfig, WorktreeInfo};
use crate::ui::role_editor_modal;

// ── TextInput Helper ────────────────────────────────────────────────────────

/// Simple text input state with cursor tracking.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextInput {
    buffer: String,
    cursor: usize,
}

impl TextInput {
    pub fn new() -> Self {
        Self::default()
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

impl Default for AddProjectModal {
    fn default() -> Self {
        Self {
            name: TextInput::default(),
            path: TextInput::default(),
            field: AddProjectField::Name,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RepoSelectorModal {
    pub index: usize,
}

#[derive(Debug, Clone, Default)]
pub struct SessionModeModal {
    pub index: usize,
}

#[derive(Debug, Clone, Default)]
pub struct BranchSelectorModal {
    pub index: usize,
    pub branches: Vec<String>,
    pub pending_repo_path: Option<PathBuf>,
}

impl BranchSelectorModal {
    pub fn with_branches(branches: Vec<String>) -> Self {
        Self {
            branches,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WorktreeNameModal {
    pub name: TextInput,
    pub pending_base_branch: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RoleSelectorModal {
    pub index: usize,
    pub pending_spawn_config: Option<SessionConfig>,
    pub pending_spawn_worktree: Option<WorktreeInfo>,
    pub pending_spawn_name: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DeleteProjectModal {
    pub project_name: String,
    pub confirmation: TextInput,
    pub error: Option<String>,
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
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

impl Default for RoleEditorModal {
    fn default() -> Self {
        Self {
            view: RoleEditorView::List,
            list_index: 0,
            roles: Vec::new(),
            field: role_editor_modal::RoleEditorField::Name,
            name: TextInput::default(),
            description: TextInput::default(),
            allowed_tools: ToolListState::default(),
            disallowed_tools: ToolListState::default(),
            system_prompt: TextInput::default(),
            editing_index: None,
        }
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

impl Default for ToolListState {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            mode: role_editor_modal::ToolListMode::Browse,
            input: TextInput::default(),
        }
    }
}

impl ToolListState {
    pub fn new() -> Self {
        Self::default()
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

// ── Main Modal Enum ────────────────────────────────────────────────────────

/// Single, discriminated union replacing 14+ boolean flags.
/// Only one modal can be active at a time, making invalid states unrepresentable.
#[derive(Debug, Clone, Default)]
pub enum Modal {
    #[default]
    None,
    Help,
    AddProject(AddProjectModal),
    DeleteProject(DeleteProjectModal),
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

    #[test]
    fn test_text_input_with_unicode() {
        let mut input = TextInput::new();
        // Test with multi-byte UTF-8 characters
        input.insert('ñ');
        input.insert('é');
        assert_eq!(input.cursor_pos(), 2);
        assert_eq!(input.value().len(), 4); // 2 bytes each for ñ and é
    }

    #[test]
    fn test_text_input_delete_at_cursor() {
        let mut input = TextInput::new();
        input.set("hello");
        input.move_left(); // Now at 'o'
        input.delete();
        assert_eq!(input.value(), "hell");
    }

    #[test]
    fn test_modal_state_transitions() {
        // Test that only one modal can be active
        let mut modal = Modal::None;
        assert!(!modal.is_open());

        modal = Modal::Help;
        assert!(modal.is_open());

        modal = Modal::AddProject(AddProjectModal::default());
        assert!(modal.is_open());

        modal.close();
        assert!(!modal.is_open());
    }

    #[test]
    fn test_branch_selector_initial_state() {
        let branch = BranchSelectorModal::default();
        assert_eq!(branch.index, 0);
        assert_eq!(branch.branches.len(), 0);
        assert!(branch.pending_repo_path.is_none());
    }

    #[test]
    fn test_branch_selector_with_branches_builder() {
        let branches = vec!["main".to_string(), "develop".to_string()];
        let selector = BranchSelectorModal::with_branches(branches.clone());
        assert_eq!(selector.index, 0);
        assert_eq!(selector.branches, branches);
    }

    #[test]
    fn test_role_editor_modal_reset() {
        let mut editor = RoleEditorModal {
            list_index: 5,
            ..Default::default()
        };
        editor.roles.push(RoleConfig {
            name: "test".to_string(),
            description: String::new(),
            permissions: crate::session::RolePermissions::default(),
        });
        editor.editing_index = Some(2);

        editor.reset();

        assert_eq!(editor.list_index, 0);
        assert_eq!(editor.roles.len(), 0);
        assert!(editor.editing_index.is_none());
        assert_eq!(editor.view, RoleEditorView::List);
    }

    #[test]
    fn test_tool_list_state_add_tool() {
        let mut state = ToolListState::new();
        state.load(&["existing".to_string()]);

        state.start_adding();
        assert_eq!(state.mode(), role_editor_modal::ToolListMode::Adding);

        state.input_mut().set("new_tool");
        state.confirm_add();

        assert_eq!(state.items().len(), 2);
        assert!(state.items().contains(&"new_tool".to_string()));
    }

    #[test]
    fn test_tool_list_state_delete() {
        let mut state = ToolListState::new();
        state.load(&[
            "tool1".to_string(),
            "tool2".to_string(),
            "tool3".to_string(),
        ]);
        state.selected = 1;

        state.delete_selected();

        assert_eq!(state.items().len(), 2);
        assert!(!state.items().contains(&"tool2".to_string()));
    }

    #[test]
    fn test_navigate_list_boundary_conditions() {
        // Test with list of size 1
        let mut index = 0;
        navigate_list(&mut index, Direction::Up, 1);
        assert_eq!(index, 0);

        navigate_list(&mut index, Direction::Down, 1);
        assert_eq!(index, 0); // Can't move down from only item

        // Test with empty list (max = 0)
        navigate_list(&mut index, Direction::Down, 0);
        assert_eq!(index, 0); // Should stay at 0 when max is 0
    }

    #[test]
    fn test_add_project_modal_default_state() {
        let modal = AddProjectModal::default();
        assert_eq!(modal.name.value(), "");
        assert_eq!(modal.path.value(), "");
        assert_eq!(modal.field, AddProjectField::Name);
    }

    #[test]
    fn test_text_input_equality() {
        let input1 = TextInput::new();
        let input2 = TextInput::default();
        assert_eq!(input1, input2);

        let mut input3 = TextInput::new();
        input3.set("test");
        assert_ne!(input1, input3);
    }
}
