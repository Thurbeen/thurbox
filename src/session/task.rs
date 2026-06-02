//! Tasks — a todo list that can connect items to coding agents.
//!
//! A task is a named todo item with a status (`Todo`/`InProgress`/`Done`) and an
//! optional [`AutomationAction`]: triggering a task either pastes its title into
//! an existing session (`Send`) or spawns a new session — optionally on a fresh
//! git worktree — and prompts it (`Spawn`). A task with no action is a plain
//! local todo (triggering is a no-op until it is connected to an agent).
//!
//! The `source`/`external_id`/`external_url` fields scaffold future sync with
//! external issue trackers (Jira, GitHub Issues, …); local tasks use
//! `source = "local"` and leave the external fields empty.
//!
//! This module is pure data (no local crate imports beyond `super`), matching
//! the architecture rule for `session`. Persistence lives in `storage::tasks`;
//! dispatch lives in the `app` layer.

use super::AutomationAction;

/// `source` value for a task created locally inside thurbox.
pub const SOURCE_LOCAL: &str = "local";

/// Lifecycle state of a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskStatus {
    #[default]
    Todo,
    InProgress,
    Done,
}

impl TaskStatus {
    /// Storage discriminant (`status` column).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::InProgress => "in_progress",
            Self::Done => "done",
        }
    }

    /// Human-readable label for display (e.g. in the editor and details panel).
    /// Unlike [`as_str`](Self::as_str), the multi-word state uses a space.
    pub fn label(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::InProgress => "in progress",
            Self::Done => "done",
        }
    }

    /// Parse a status stored in the database, defaulting unknown values to
    /// `Todo`.
    pub fn from_db(s: &str) -> Self {
        match s {
            "in_progress" => Self::InProgress,
            "done" => Self::Done,
            _ => Self::Todo,
        }
    }

    /// Advance to the next status, wrapping `Todo → InProgress → Done → Todo`.
    /// Drives the `Space` keybinding in the tasks panel.
    pub fn cycle(self) -> Self {
        match self {
            Self::Todo => Self::InProgress,
            Self::InProgress => Self::Done,
            Self::Done => Self::Todo,
        }
    }
}

/// A persisted task (todo item).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: i64,
    /// The task text; this is what seeds a `Send`/`Spawn` action when triggered.
    pub title: String,
    pub status: TaskStatus,
    /// How the task connects to an agent. `None` = an unconnected local todo.
    pub action: Option<AutomationAction>,
    /// Origin of the task (`"local"` or an external tracker name). Scaffolding
    /// for future external sync.
    pub source: String,
    /// Identifier in the external tracker, when `source` is not `"local"`.
    pub external_id: Option<String>,
    /// Link to the task in the external tracker, when applicable.
    pub external_url: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    /// Soft-delete marker (unix millis). `None` = active.
    pub deleted_at: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trips_through_db_string() {
        for s in [TaskStatus::Todo, TaskStatus::InProgress, TaskStatus::Done] {
            assert_eq!(TaskStatus::from_db(s.as_str()), s);
        }
    }

    #[test]
    fn unknown_status_defaults_to_todo() {
        assert_eq!(TaskStatus::from_db("garbage"), TaskStatus::Todo);
        assert_eq!(TaskStatus::default(), TaskStatus::Todo);
    }

    #[test]
    fn cycle_walks_todo_in_progress_done() {
        assert_eq!(TaskStatus::Todo.cycle(), TaskStatus::InProgress);
        assert_eq!(TaskStatus::InProgress.cycle(), TaskStatus::Done);
        assert_eq!(TaskStatus::Done.cycle(), TaskStatus::Todo);
    }

    #[test]
    fn label_uses_spaced_form() {
        assert_eq!(TaskStatus::Todo.label(), "todo");
        assert_eq!(TaskStatus::InProgress.label(), "in progress");
        assert_eq!(TaskStatus::Done.label(), "done");
    }
}
