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
    /// Optional free-form markdown description (notes, acceptance criteria, …).
    /// `None` when blank. Rendered as markdown in the read-only details panel.
    pub description: Option<String>,
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

impl Task {
    /// Build the prompt seeded into an agent when this task is triggered
    /// (`Send` into a running session, or `Spawn` of a fresh one).
    ///
    /// Beyond the bare title it gives the agent enough context to act on its
    /// own: that it is solving a Thurbox task, the markdown description, how to
    /// fetch the full record (`thurbox-cli task show <id>`), and how to mark the
    /// task done when finished (the trigger already advances it to *in
    /// progress*). Shared by the TUI (`app`) and headless (`thurbox-cli task
    /// run`) dispatch paths so the two never drift.
    pub fn agent_prompt(&self) -> String {
        let mut prompt = format!(
            "You are working on Thurbox task #{id}.\n\n# {title}\n",
            id = self.id,
            title = self.title,
        );
        if let Some(desc) = self.description.as_deref().map(str::trim) {
            if !desc.is_empty() {
                prompt.push('\n');
                prompt.push_str(desc);
                prompt.push('\n');
            }
        }
        prompt.push_str(&format!(
            "\n---\nThis is a Thurbox task. Run `thurbox-cli task show {id}` for the full \
             record. The task is now marked **in progress**; when you finish, run \
             `thurbox-cli task edit {id} --status done`.\n",
            id = self.id,
        ));
        prompt
    }

    /// Session name used when this task is dispatched via a `Spawn` action:
    /// the human task title with a compact id tag, e.g.
    /// `Wire up SSH backend · #42`. The title is kept verbatim (author casing
    /// and spacing) so the session list reads like the task itself; the trailing
    /// [`TASK_ID_TAG`]`<id>` keeps the name unique (the tmux window name is
    /// derived from it) and lets [`matches_spawn_session`](Self::matches_spawn_session)
    /// recover the owning task. The whole name is bounded by
    /// [`SPAWN_SESSION_NAME_MAX`], truncating the title at a word boundary.
    /// Falls back to the bare `task-<id>` form when the title is empty. Shared by
    /// the headless `task run` path.
    pub fn spawn_session_name(&self) -> String {
        let suffix = format!("{TASK_ID_TAG}{}", self.id);
        let budget = SPAWN_SESSION_NAME_MAX.saturating_sub(suffix.len());
        let title = truncate_title(&self.title, budget);
        if title.is_empty() {
            format!("task-{}", self.id)
        } else {
            format!("{title}{suffix}")
        }
    }

    /// Whether `name` is this task's spawned session. Recognizes the current
    /// `… · #<id>` convention (matched on the exact tag-and-id suffix, whose
    /// ` · #` boundary keeps `#4` from matching `…#14`) as well as the legacy
    /// `task-<id>-<slug>` and bare `task-<id>` forms still carried by sessions
    /// spawned before this naming change (the title may also have been edited
    /// since the spawn, so only the id is significant).
    pub fn matches_spawn_session(&self, name: &str) -> bool {
        if name.ends_with(&format!("{TASK_ID_TAG}{}", self.id)) {
            return true;
        }
        let prefix = format!("task-{}", self.id);
        name == prefix
            || name
                .strip_prefix(&prefix)
                .is_some_and(|r| r.starts_with('-'))
    }
}

/// Upper bound on a spawned session name (bytes), matching `validate_safe_name`.
/// tmux window names stay readable in the TUI session list well under this.
pub const SPAWN_SESSION_NAME_MAX: usize = 64;

/// Separator + marker that tags a spawned session name with its task id, e.g.
/// the ` · #` in `Wire up SSH backend · #42`. The surrounding spaces and `#`
/// make the trailing-id match in [`Task::matches_spawn_session`] unambiguous.
pub const TASK_ID_TAG: &str = " · #";

/// The task title trimmed and whitespace-collapsed to a single line, truncated
/// at a word boundary so its byte length does not exceed `budget` (never
/// splitting a multi-byte char or leaving trailing whitespace).
fn truncate_title(title: &str, budget: usize) -> String {
    let collapsed = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() <= budget {
        return collapsed;
    }
    // Cut at the last char boundary within budget, then back off to the last
    // whole word so we never dangle a half word.
    let mut end = budget;
    while end > 0 && !collapsed.is_char_boundary(end) {
        end -= 1;
    }
    let head = &collapsed[..end];
    let trimmed = match head.rsplit_once(' ') {
        Some((words, _partial)) => words,
        None => head,
    };
    trimmed.trim_end().to_string()
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

    fn sample_task(description: Option<&str>) -> Task {
        Task {
            id: 42,
            title: "Wire up SSH backend".to_string(),
            description: description.map(str::to_string),
            status: TaskStatus::Todo,
            action: None,
            source: SOURCE_LOCAL.to_string(),
            external_id: None,
            external_url: None,
            created_at: 0,
            updated_at: 0,
            deleted_at: None,
        }
    }

    #[test]
    fn agent_prompt_carries_id_title_and_cli_hints() {
        let prompt = sample_task(Some("Implement `SshTmuxBackend`.")).agent_prompt();
        assert!(prompt.contains("Thurbox task #42"));
        assert!(prompt.contains("# Wire up SSH backend"));
        assert!(prompt.contains("Implement `SshTmuxBackend`."));
        // Self-service context: how to read more and how to close it out.
        assert!(prompt.contains("thurbox-cli task show 42"));
        assert!(prompt.contains("thurbox-cli task edit 42 --status done"));
    }

    #[test]
    fn spawn_session_name_keeps_the_title_verbatim() {
        // The human title is preserved (casing + spacing), tagged with the id.
        assert_eq!(
            sample_task(None).spawn_session_name(),
            "Wire up SSH backend · #42"
        );
    }

    #[test]
    fn spawn_session_name_collapses_whitespace() {
        let mut task = sample_task(None);
        task.title = "  Fix   TUI\tcrash  ".to_string();
        // Internal runs collapse to single spaces; ends are trimmed.
        assert_eq!(task.spawn_session_name(), "Fix TUI crash · #42");
    }

    #[test]
    fn spawn_session_name_truncates_long_titles_at_a_word_boundary() {
        let mut task = sample_task(None);
        task.title = "Improve task session naming with simpler shorter human readable identifiers"
            .to_string();
        let name = task.spawn_session_name();
        assert!(name.len() <= SPAWN_SESSION_NAME_MAX);
        assert!(name.ends_with(" · #42"));
        // The kept title portion is whole words (no dangling partial word).
        let title = name.strip_suffix(" · #42").unwrap();
        assert!(!title.ends_with(' '));
        assert!(
            "Improve task session naming with simpler shorter human readable identifiers"
                .starts_with(title)
        );
        // A single pathological word is hard-capped, still within budget.
        task.title = "x".repeat(200);
        let name = task.spawn_session_name();
        assert!(name.len() <= SPAWN_SESSION_NAME_MAX);
        assert!(name.ends_with(" · #42"));
    }

    #[test]
    fn spawn_session_name_falls_back_to_bare_id_for_blank_title() {
        let mut task = sample_task(None);
        task.title = "   ".to_string();
        assert_eq!(task.spawn_session_name(), "task-42");
    }

    #[test]
    fn matches_spawn_session_accepts_current_and_legacy_names() {
        let task = sample_task(None);
        // Current convention: trailing " · #<id>" tag.
        assert!(task.matches_spawn_session("Wire up SSH backend · #42"));
        assert!(task.matches_spawn_session("Some since-edited title · #42"));
        // Legacy forms still relink after a restart.
        assert!(task.matches_spawn_session("task-42-wire-up-ssh-backend"));
        assert!(task.matches_spawn_session("task-42")); // legacy bare convention
                                                        // Different task ids never match — including the tricky #4 vs #42 boundary.
        assert!(!task.matches_spawn_session("Wire up SSH backend · #421"));
        assert!(!task.matches_spawn_session("Wire up SSH backend · #4"));
        assert!(!task.matches_spawn_session("task-421"));
        assert!(!task.matches_spawn_session("task-4"));
        assert!(!task.matches_spawn_session("my-task-42"));
    }

    #[test]
    fn agent_prompt_omits_empty_description_block() {
        // No description and a blank-only description both skip the body.
        for desc in [None, Some("   ")] {
            let prompt = sample_task(desc).agent_prompt();
            assert!(prompt.contains("# Wire up SSH backend"));
            // Title line is immediately followed by the `---` hint separator.
            assert!(prompt.contains("# Wire up SSH backend\n\n---\n"));
        }
    }
}
