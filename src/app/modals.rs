// Modal state management for Thurbox TUI.
// This module consolidates all modal-related state into type-safe enums,
// replacing boolean flags with a single discriminated union.

use std::path::PathBuf;

use crate::session::{McpServerConfig, RoleConfig};
use crate::storage::DeletedSessionInfo;

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
    RepoList,
}

// ── Modal State Structs ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AddProjectModal {
    pub name: TextInput,
    pub path: TextInput,
    pub field: AddProjectField,
    pub repos: Vec<PathBuf>,
    pub repo_index: usize,
    pub path_suggestion: Option<String>,
}

impl Default for AddProjectModal {
    fn default() -> Self {
        Self {
            name: TextInput::default(),
            path: TextInput::default(),
            field: AddProjectField::Name,
            repos: Vec::new(),
            repo_index: 0,
            path_suggestion: None,
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
}

#[derive(Debug, Clone, Default)]
pub struct WorktreeNameModal {
    pub name: TextInput,
}

#[derive(Debug, Clone, Default)]
pub struct RoleSelectorModal {
    pub index: usize,
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

// ── EditProjectField ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditProjectField {
    Name,
    Path,
    RepoList,
    Roles,
    McpServers,
}

// ── EditProjectModal ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct EditProjectModal {
    pub name: TextInput,
    pub path: TextInput,
    pub field: EditProjectField,
    pub repos: Vec<PathBuf>,
    pub repo_index: usize,
    pub path_suggestion: Option<String>,
    pub original_id: Option<crate::project::ProjectId>,
    pub role_editor_roles: Vec<RoleConfig>,
    pub role_editor_list_index: usize,
    pub mcp_servers: Vec<McpServerConfig>,
    pub mcp_server_index: usize,
}

impl Default for EditProjectModal {
    fn default() -> Self {
        Self {
            name: TextInput::default(),
            path: TextInput::default(),
            field: EditProjectField::Name,
            repos: Vec::new(),
            repo_index: 0,
            path_suggestion: None,
            original_id: None,
            role_editor_roles: Vec::new(),
            role_editor_list_index: 0,
            mcp_servers: Vec::new(),
            mcp_server_index: 0,
        }
    }
}

// ── ContainerfilePickerModal ─────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct ContainerfilePickerModal {
    pub index: usize,
    pub list: Vec<String>,
}

// ── RestoreSessionsModal ─────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct RestoreSessionsModal {
    pub list: Vec<DeletedSessionInfo>,
    pub index: usize,
}

// ── ScheduleCommandModal ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScheduleCommandField {
    #[default]
    Command,
    Delay,
}

#[derive(Debug, Clone, Default)]
pub struct ScheduleCommandModal {
    pub command: TextInput,
    pub delay_minutes: TextInput,
    pub field: ScheduleCommandField,
}

impl ScheduleCommandModal {
    /// Return a mutable reference to whichever text field is currently focused.
    pub fn active_field_mut(&mut self) -> &mut TextInput {
        match self.field {
            ScheduleCommandField::Command => &mut self.command,
            ScheduleCommandField::Delay => &mut self.delay_minutes,
        }
    }
}

// ── Main Modal Enum ────────────────────────────────────────────────────────

/// Single, discriminated union replacing boolean flags for modal state.
/// Only one modal can be active at a time, making invalid states unrepresentable.
///
/// Note: `RoleEditor`, `McpEditor`, and `DiscardConfirmation` are kept as separate
/// boolean flags on `App` because they overlay the `EditProject` modal without
/// replacing it. See `App::show_role_editor`, `App::show_mcp_editor`, and
/// `App::show_discard_confirmation`.
#[derive(Debug, Clone, Default)]
pub enum Modal {
    #[default]
    None,
    Help,
    AddProject(AddProjectModal),
    DeleteProject(DeleteProjectModal),
    #[allow(dead_code)] // Planned: not yet wired to a trigger key
    RepoSelector(RepoSelectorModal),
    SessionMode(SessionModeModal),
    BranchSelector(BranchSelectorModal),
    WorktreeName(WorktreeNameModal),
    RoleSelector(RoleSelectorModal),
    EditProject(Box<EditProjectModal>),
    ContainerfilePicker(ContainerfilePickerModal),
    RestoreSessions(RestoreSessionsModal),
    ScheduleCommand(ScheduleCommandModal),
}

impl Modal {
    pub fn close(&mut self) {
        *self = Modal::None;
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
    fn test_modal_default_is_none() {
        let modal = Modal::default();
        assert!(matches!(modal, Modal::None));
    }

    #[test]
    fn test_modal_help_is_open() {
        let modal = Modal::Help;
        assert!(!matches!(modal, Modal::None));
    }

    #[test]
    fn test_modal_close() {
        let mut modal = Modal::Help;
        assert!(!matches!(modal, Modal::None));
        modal.close();
        assert!(matches!(modal, Modal::None));
    }

    #[test]
    fn test_add_project_modal_default() {
        let modal = AddProjectModal::default();
        assert_eq!(modal.name.value(), "");
        assert_eq!(modal.path.value(), "");
        assert_eq!(modal.field, AddProjectField::Name);
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
        assert!(matches!(modal, Modal::None));

        modal = Modal::Help;
        assert!(!matches!(modal, Modal::None));

        modal = Modal::AddProject(AddProjectModal::default());
        assert!(!matches!(modal, Modal::None));

        modal.close();
        assert!(matches!(modal, Modal::None));
    }

    #[test]
    fn test_branch_selector_initial_state() {
        let branch = BranchSelectorModal::default();
        assert_eq!(branch.index, 0);
        assert_eq!(branch.branches.len(), 0);
    }

    #[test]
    fn test_add_project_modal_default_state() {
        let modal = AddProjectModal::default();
        assert_eq!(modal.name.value(), "");
        assert_eq!(modal.path.value(), "");
        assert_eq!(modal.field, AddProjectField::Name);
        assert!(modal.repos.is_empty());
        assert_eq!(modal.repo_index, 0);
        assert!(modal.path_suggestion.is_none());
    }

    #[test]
    fn test_add_project_field_has_repo_list_variant() {
        let field = AddProjectField::RepoList;
        assert_ne!(field, AddProjectField::Name);
        assert_ne!(field, AddProjectField::Path);
    }

    #[test]
    fn test_add_project_modal_with_repos() {
        let mut modal = AddProjectModal::default();
        modal.repos.push(PathBuf::from("/path/to/repo1"));
        modal.repos.push(PathBuf::from("/path/to/repo2"));
        modal.repo_index = 1;
        modal.path_suggestion = Some("er/".to_string());

        assert_eq!(modal.repos.len(), 2);
        assert_eq!(modal.repo_index, 1);
        assert_eq!(modal.path_suggestion.as_deref(), Some("er/"));
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

    #[test]
    fn test_schedule_command_modal_default() {
        let modal = ScheduleCommandModal::default();
        assert_eq!(modal.command.value(), "");
        assert_eq!(modal.delay_minutes.value(), "");
        assert_eq!(modal.field, ScheduleCommandField::Command);
    }

    #[test]
    fn test_schedule_command_active_field_returns_command() {
        let mut modal = ScheduleCommandModal::default();
        modal.active_field_mut().insert('a');
        assert_eq!(modal.command.value(), "a");
        assert_eq!(modal.delay_minutes.value(), "");
    }

    #[test]
    fn test_schedule_command_active_field_returns_delay() {
        let mut modal = ScheduleCommandModal {
            field: ScheduleCommandField::Delay,
            ..Default::default()
        };
        modal.active_field_mut().insert('5');
        assert_eq!(modal.delay_minutes.value(), "5");
        assert_eq!(modal.command.value(), "");
    }

    #[test]
    fn test_schedule_command_field_toggle() {
        let field = ScheduleCommandField::Command;
        assert_ne!(field, ScheduleCommandField::Delay);

        let field = ScheduleCommandField::default();
        assert_eq!(field, ScheduleCommandField::Command);
    }
}
