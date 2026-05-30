// Modal state management for Thurbox TUI.
// This module consolidates all modal-related state into type-safe enums,
// replacing boolean flags with a single discriminated union.

use std::path::PathBuf;

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

// ── Modal State Structs ────────────────────────────────────────────────────

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
pub struct SessionNameModal {
    pub name: TextInput,
}

#[derive(Debug, Clone, Default)]
pub struct ThemePickerModal {
    pub index: usize,
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
    /// When editing an existing command, holds the original command ID to cancel on submit
    /// and the session ID + name to preserve the target session.
    pub editing: Option<EditingCommand>,
}

/// State preserved when editing an existing scheduled command.
#[derive(Debug, Clone)]
pub struct EditingCommand {
    pub id: i64,
    pub session_id: crate::session::SessionId,
    pub session_name: String,
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

// ── ScheduledCommandsListModal ──────────────────────────────────────────

/// An entry in the scheduled commands list modal.
#[derive(Debug, Clone)]
pub struct ScheduledCommandListEntry {
    pub id: i64,
    pub session_name: String,
    pub command_text: String,
    pub countdown: String,
}

/// Modal state for listing and cancelling pending scheduled commands.
#[derive(Debug, Clone, Default)]
pub struct ScheduledCommandsListModal {
    pub index: usize,
    pub commands: Vec<ScheduledCommandListEntry>,
}

// ── RepoPickerModal ─────────────────────────────────────────────────────

/// Which section of the repo picker is focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepoPickerFocus {
    /// The list of bookmarked/recent repos (multi-select).
    #[default]
    List,
    /// The text input for adding a new path.
    Input,
    /// The fuzzy search filter input.
    Search,
}

#[derive(Debug, Clone, Default)]
pub struct RepoPickerModal {
    /// Bookmarked repos shown in the list.
    pub bookmarks: Vec<PathBuf>,
    /// Which bookmarks are selected (checked).
    pub selected: Vec<bool>,
    /// Whether each selected repo should use worktree mode (parallel to `bookmarks`).
    pub worktree: Vec<bool>,
    /// Cursor index in the bookmark list (indexes into `filtered_indices`).
    pub list_index: usize,
    /// Text input for adding a new repo path.
    pub path_input: TextInput,
    /// Autocomplete suggestion for the path input.
    pub path_suggestion: Option<String>,
    /// Which section is focused (list vs input vs search).
    pub focus: RepoPickerFocus,
    /// Fuzzy search input for filtering bookmarks.
    pub search_input: TextInput,
    /// Indices into `bookmarks` that match the current search query.
    /// When search is empty, contains `0..bookmarks.len()`.
    pub filtered_indices: Vec<usize>,
}

impl RepoPickerModal {
    /// Clear the search query and reset the filter to show all bookmarks.
    pub fn clear_search(&mut self) {
        self.search_input.clear();
        self.filtered_indices = (0..self.bookmarks.len()).collect();
        self.list_index = 0;
    }
}

// ── Main Modal Enum ────────────────────────────────────────────────────────

/// Single, discriminated union replacing boolean flags for modal state.
/// Only one modal can be active at a time, making invalid states unrepresentable.
#[derive(Debug, Clone, Default)]
pub enum Modal {
    #[default]
    None,
    Help,
    BranchSelector(BranchSelectorModal),
    WorktreeName(WorktreeNameModal),
    AgentPicker(crate::ui::agent_picker_modal::AgentPickerState),
    RestoreSessions(RestoreSessionsModal),
    ScheduleCommand(ScheduleCommandModal),
    ScheduledCommandsList(ScheduledCommandsListModal),
    RepoPicker(RepoPickerModal),
    SessionName(SessionNameModal),
    ThemePicker(ThemePickerModal),
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
        let mut modal = Modal::None;
        assert!(matches!(modal, Modal::None));

        modal = Modal::Help;
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

    #[test]
    fn test_schedule_command_modal_editing_default_is_none() {
        let modal = ScheduleCommandModal::default();
        assert!(modal.editing.is_none());
    }

    #[test]
    fn test_scheduled_commands_list_modal_default() {
        let modal = ScheduledCommandsListModal::default();
        assert_eq!(modal.index, 0);
        assert!(modal.commands.is_empty());
    }

    #[test]
    fn test_repo_picker_clear_search_resets_filter() {
        let mut rp = RepoPickerModal {
            bookmarks: vec!["/a".into(), "/b".into(), "/c".into()],
            selected: vec![false, true, false],
            worktree: vec![false, false, false],
            list_index: 1,
            filtered_indices: vec![1], // simulating an active filter
            ..Default::default()
        };
        rp.search_input.set("b");

        rp.clear_search();

        assert_eq!(rp.search_input.value(), "");
        assert_eq!(rp.filtered_indices, vec![0, 1, 2]);
        assert_eq!(rp.list_index, 0);
    }

    #[test]
    fn test_repo_picker_clear_search_empty_bookmarks() {
        let mut rp = RepoPickerModal::default();
        rp.clear_search();
        assert!(rp.filtered_indices.is_empty());
        assert_eq!(rp.list_index, 0);
    }

    #[test]
    fn test_repo_picker_default_has_empty_search() {
        let rp = RepoPickerModal::default();
        assert_eq!(rp.search_input.value(), "");
        assert!(rp.filtered_indices.is_empty());
        assert_eq!(rp.focus, RepoPickerFocus::List);
    }

    #[test]
    fn test_schedule_command_modal_with_editing() {
        let mut modal = ScheduleCommandModal::default();
        modal.command.set("test cmd");
        modal.delay_minutes.set("5");
        modal.editing = Some(EditingCommand {
            id: 42,
            session_id: "test-session".parse().unwrap_or_default(),
            session_name: "my-session".to_string(),
        });
        assert_eq!(modal.command.value(), "test cmd");
        assert_eq!(modal.editing.as_ref().unwrap().id, 42);
        assert_eq!(modal.editing.as_ref().unwrap().session_name, "my-session");
    }
}
