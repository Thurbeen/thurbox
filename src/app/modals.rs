// Modal state management for Thurbox TUI.
// This module consolidates all modal-related state into type-safe enums,
// replacing boolean flags with a single discriminated union.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyModifiers};

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

/// Step to the next/previous field in `fields` relative to `current`, wrapping
/// at both ends. `delta` is `+1` (next) or `-1` (previous). Shared by the
/// automation and task editor forms, which navigate different field enums.
fn cycle_field<F: PartialEq + Copy>(fields: &[F], current: F, delta: isize) -> F {
    if fields.is_empty() {
        return current;
    }
    let idx = fields.iter().position(|f| *f == current).unwrap_or(0);
    let len = fields.len() as isize;
    let next = (idx as isize + delta).rem_euclid(len) as usize;
    fields[next]
}

/// Apply a text-editing key (insert/backspace/delete/cursor move) to the
/// currently focused field, if any. Returns `true` when `code` was a
/// text-editing key (whether or not a field was focused), so editor key
/// handlers can share one implementation across the automation and task forms.
fn apply_text_input_key(field: Option<&mut TextInput>, code: KeyCode) -> bool {
    match code {
        KeyCode::Char(c) => {
            if let Some(f) = field {
                f.insert(c);
            }
        }
        KeyCode::Backspace => {
            if let Some(f) = field {
                f.backspace();
            }
        }
        KeyCode::Delete => {
            if let Some(f) = field {
                f.delete();
            }
        }
        KeyCode::Left => {
            if let Some(f) = field {
                f.move_left();
            }
        }
        KeyCode::Right => {
            if let Some(f) = field {
                f.move_right();
            }
        }
        KeyCode::Home => {
            if let Some(f) = field {
                f.home();
            }
        }
        KeyCode::End => {
            if let Some(f) = field {
                f.end();
            }
        }
        _ => return false,
    }
    true
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

// ── AutomationEditorModal ───────────────────────────────────────────────

/// What action a triggered automation performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutomationActionKind {
    /// Paste the prompt into an existing session.
    #[default]
    Send,
    /// Spawn a new session and prompt it.
    Spawn,
}

/// How an automation's schedule is entered in the editor. Cycled with the
/// arrow keys so users never type a cron expression or magic trigger string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TriggerKind {
    /// Fire once after a relative delay (e.g. `30m`).
    Once,
    /// Every hour at a chosen minute.
    Hourly,
    /// Every day at a chosen time.
    #[default]
    Daily,
    /// Mon–Fri at a chosen time.
    Weekdays,
    /// A chosen weekday at a chosen time.
    Weekly,
    /// Raw cron expression (power users).
    Cron,
}

impl TriggerKind {
    /// All kinds in cycle order.
    const ALL: [TriggerKind; 6] = [
        TriggerKind::Once,
        TriggerKind::Hourly,
        TriggerKind::Daily,
        TriggerKind::Weekdays,
        TriggerKind::Weekly,
        TriggerKind::Cron,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TriggerKind::Once => "once",
            TriggerKind::Hourly => "hourly",
            TriggerKind::Daily => "daily",
            TriggerKind::Weekdays => "weekdays",
            TriggerKind::Weekly => "weekly",
            TriggerKind::Cron => "cron",
        }
    }

    fn step(self, delta: i32) -> Self {
        let idx = Self::ALL.iter().position(|k| *k == self).unwrap_or(0) as i32;
        let len = Self::ALL.len() as i32;
        Self::ALL[(idx + delta).rem_euclid(len) as usize]
    }
}

/// Focusable field in the automation editor. The set shown depends on the
/// current [`TriggerKind`] and action (see `AutomationEditorModal::visible_fields`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutomationField {
    #[default]
    Name,
    /// Trigger-kind selector (cycled with ←/→).
    Trigger,
    /// Relative delay text (Once).
    Delay,
    /// Weekday stepper (Weekly).
    Weekday,
    /// Hour-of-day stepper.
    Hour,
    /// Minute stepper.
    Minute,
    /// Raw cron expression text (Cron).
    CronExpr,
    Timezone,
    Action,
    /// Send action: target-session selector (cycled with ←/→).
    Target,
    Repo,
    Worktree,
    Agent,
    Prompt,
}

/// Editor form for creating or editing an automation.
#[derive(Debug, Clone)]
pub struct AutomationEditorModal {
    /// `Some` when editing an existing automation.
    pub editing_id: Option<i64>,
    pub name: TextInput,
    /// How the schedule is specified.
    pub trigger_kind: TriggerKind,
    /// Relative delay text for `Once` (e.g. `30m`, `2h`, `1h30m`).
    pub delay: TextInput,
    /// Weekday for `Weekly`: 0 = Sunday … 6 = Saturday.
    pub weekday: u32,
    /// Hour-of-day 0–23 for daily/weekdays/weekly.
    pub hour: u32,
    /// Minute 0–59 for hourly/daily/weekdays/weekly.
    pub minute: u32,
    /// Raw cron expression for `Cron`.
    pub cron_expr: TextInput,
    /// Optional IANA timezone.
    pub timezone: TextInput,
    pub action: AutomationActionKind,
    /// Spawn action: repository path.
    pub repo: TextInput,
    /// Spawn action: optional worktree branch.
    pub worktree: TextInput,
    /// Spawn action: optional agent name.
    pub agent: TextInput,
    pub prompt: TextInput,
    pub enabled: bool,
    pub field: AutomationField,
    /// Send action: the running sessions available as targets (id + display
    /// name), captured at open and cycled with the `Target` field.
    pub sessions: Vec<(crate::session::SessionId, String)>,
    /// Index into `sessions` of the selected Send target.
    pub target_index: usize,
}

/// Result of feeding a key to the automation editor — lets the caller decide
/// what "save"/"cancel" mean (close an overlay vs. return focus to the pane).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorOutcome {
    /// Key was consumed; stay in the editor.
    Continue,
    /// `Enter` — the caller should validate + persist the automation.
    Save,
    /// `Esc` — the caller should discard and leave the editor.
    Cancel,
}

impl Default for AutomationEditorModal {
    fn default() -> Self {
        Self {
            editing_id: None,
            name: TextInput::default(),
            trigger_kind: TriggerKind::default(),
            delay: {
                let mut t = TextInput::default();
                t.set("30m");
                t
            },
            weekday: 1, // Monday
            hour: 9,
            minute: 0,
            cron_expr: TextInput::default(),
            timezone: TextInput::default(),
            action: AutomationActionKind::default(),
            repo: TextInput::default(),
            worktree: TextInput::default(),
            agent: TextInput::default(),
            prompt: TextInput::default(),
            enabled: true,
            field: AutomationField::default(),
            sessions: Vec::new(),
            target_index: 0,
        }
    }
}

impl AutomationEditorModal {
    /// The fields shown for the current trigger kind + action, in display and
    /// navigation order.
    pub fn visible_fields(&self) -> Vec<AutomationField> {
        use AutomationField::*;
        let mut fields = vec![Name, Trigger];
        match self.trigger_kind {
            TriggerKind::Once => fields.push(Delay),
            TriggerKind::Hourly => fields.push(Minute),
            TriggerKind::Daily | TriggerKind::Weekdays => fields.extend([Hour, Minute]),
            TriggerKind::Weekly => fields.extend([Weekday, Hour, Minute]),
            TriggerKind::Cron => fields.push(CronExpr),
        }
        // Timezone only matters for wall-clock (cron) schedules, not a relative
        // one-shot delay.
        if self.trigger_kind != TriggerKind::Once {
            fields.push(Timezone);
        }
        fields.push(Action);
        match self.action {
            AutomationActionKind::Send => fields.push(Target),
            AutomationActionKind::Spawn => fields.extend([Repo, Worktree, Agent]),
        }
        fields.push(Prompt);
        fields
    }

    /// Move focus to the next visible field (wraps).
    pub fn next_field(&mut self) {
        self.field = cycle_field(&self.visible_fields(), self.field, 1);
    }

    /// Move focus to the previous visible field (wraps).
    pub fn prev_field(&mut self) {
        self.field = cycle_field(&self.visible_fields(), self.field, -1);
    }

    /// The focused text field, or `None` for selector/stepper fields (which are
    /// adjusted with ←/→ instead — see [`is_adjustable`](Self::is_adjustable)).
    pub fn active_field_mut(&mut self) -> Option<&mut TextInput> {
        use AutomationField::*;
        Some(match self.field {
            Name => &mut self.name,
            Delay => &mut self.delay,
            CronExpr => &mut self.cron_expr,
            Timezone => &mut self.timezone,
            Repo => &mut self.repo,
            Worktree => &mut self.worktree,
            Agent => &mut self.agent,
            Prompt => &mut self.prompt,
            Trigger | Weekday | Hour | Minute | Action | Target => return None,
        })
    }

    /// Whether the focused field is a selector/stepper adjusted with ←/→/Space
    /// rather than edited as text.
    pub fn is_adjustable(&self) -> bool {
        use AutomationField::*;
        matches!(
            self.field,
            Trigger | Weekday | Hour | Minute | Action | Target
        )
    }

    /// Adjust the focused selector/stepper by `delta` (−1 for ←, +1 for →/Space).
    pub fn adjust(&mut self, delta: i32) {
        use AutomationField::*;
        match self.field {
            Trigger => self.trigger_kind = self.trigger_kind.step(delta),
            Action => self.toggle_action(),
            Weekday => self.weekday = wrap_add(self.weekday, delta, 7),
            Hour => self.hour = wrap_add(self.hour, delta, 24),
            Minute => self.minute = wrap_add(self.minute, delta, 60),
            Target => {
                let len = self.sessions.len();
                if len > 0 {
                    self.target_index =
                        wrap_add(self.target_index as u32, delta, len as u32) as usize;
                }
            }
            _ => {}
        }
    }

    /// Feed a key to the editor, mutating field state. Returns whether the
    /// caller should save (`Enter`), cancel (`Esc`), or keep editing. Shared by
    /// the centered overlay (Ctrl+P) and the in-pane editor so both behave
    /// identically.
    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> EditorOutcome {
        // Selector/stepper fields (trigger, weekday, hour, minute, action,
        // target) are adjusted with ←/→/Space; text fields edit as usual.
        let adjustable = self.is_adjustable();
        match code {
            KeyCode::Esc => return EditorOutcome::Cancel,
            KeyCode::Enter => return EditorOutcome::Save,
            KeyCode::Char('e') if mods.contains(KeyModifiers::CONTROL) => {
                self.enabled = !self.enabled;
            }
            KeyCode::Tab | KeyCode::Down => self.next_field(),
            KeyCode::BackTab | KeyCode::Up => self.prev_field(),
            KeyCode::Left if adjustable => self.adjust(-1),
            KeyCode::Right | KeyCode::Char(' ') if adjustable => self.adjust(1),
            other => {
                apply_text_input_key(self.active_field_mut(), other);
            }
        }
        EditorOutcome::Continue
    }

    /// Populate the available Send targets and select `selected` (falling back to
    /// the first session when it isn't present).
    pub fn set_target_sessions(
        &mut self,
        sessions: Vec<(crate::session::SessionId, String)>,
        selected: Option<crate::session::SessionId>,
    ) {
        self.target_index = selected
            .and_then(|id| sessions.iter().position(|(sid, _)| *sid == id))
            .unwrap_or(0);
        self.sessions = sessions;
    }

    /// The currently selected Send target (id + display name), if any sessions
    /// are available.
    pub fn selected_target(&self) -> Option<&(crate::session::SessionId, String)> {
        self.sessions.get(self.target_index)
    }

    /// Toggle between Send and Spawn actions.
    pub fn toggle_action(&mut self) {
        self.action = match self.action {
            AutomationActionKind::Send => AutomationActionKind::Spawn,
            AutomationActionKind::Spawn => AutomationActionKind::Send,
        };
    }

    /// The IANA timezone the user entered, if any.
    pub fn timezone(&self) -> Option<String> {
        let tz = self.timezone.value().trim();
        (!tz.is_empty()).then(|| tz.to_string())
    }

    /// Build the [`AutomationSchedule`] described by the current fields, relative
    /// to `now` (used for the `Once` delay). Returns a user-facing error string
    /// for invalid input.
    pub fn build_schedule(&self, now: u64) -> Result<crate::session::AutomationSchedule, String> {
        use crate::session::automation::{parse_duration, preset_to_cron, SchedulePreset};
        use crate::session::AutomationSchedule;
        Ok(match self.trigger_kind {
            TriggerKind::Once => {
                let ms = parse_duration(self.delay.value().trim())
                    .ok_or("Delay must look like 30m, 2h, 1h30m, or 1d")?;
                AutomationSchedule::Once {
                    at: now.saturating_add(ms),
                }
            }
            TriggerKind::Hourly => AutomationSchedule::Cron {
                expr: preset_to_cron(SchedulePreset::Hourly, self.hour, self.minute, self.weekday),
            },
            TriggerKind::Daily => AutomationSchedule::Cron {
                expr: preset_to_cron(SchedulePreset::Daily, self.hour, self.minute, self.weekday),
            },
            TriggerKind::Weekdays => AutomationSchedule::Cron {
                expr: preset_to_cron(
                    SchedulePreset::Weekdays,
                    self.hour,
                    self.minute,
                    self.weekday,
                ),
            },
            TriggerKind::Weekly => AutomationSchedule::Cron {
                expr: preset_to_cron(SchedulePreset::Weekly, self.hour, self.minute, self.weekday),
            },
            TriggerKind::Cron => {
                let expr = self.cron_expr.value().trim();
                if expr.is_empty() {
                    return Err("Cron expression cannot be empty".into());
                }
                AutomationSchedule::Cron {
                    expr: expr.to_string(),
                }
            }
        })
    }

    /// Build an editor pre-filled from an existing automation, reverse-mapping
    /// the schedule back into structured fields where possible. The caller is
    /// responsible for populating the Send target list via
    /// [`set_target_sessions`](Self::set_target_sessions) afterwards.
    pub fn from_automation(auto: &crate::session::Automation) -> Self {
        use crate::session::{AutomationAction, AutomationSchedule};
        let mut m = Self {
            editing_id: Some(auto.id),
            enabled: auto.enabled,
            ..Self::default()
        };
        m.name.set(&auto.name);
        match &auto.schedule {
            AutomationSchedule::Once { at } => {
                m.trigger_kind = TriggerKind::Once;
                let remaining = at.saturating_sub(crate::sync::current_time_millis());
                m.delay.set(&format_duration_short(remaining));
            }
            AutomationSchedule::Cron { expr } => match recognize_cron(expr) {
                Some((kind, hour, minute, weekday)) => {
                    m.trigger_kind = kind;
                    m.hour = hour;
                    m.minute = minute;
                    m.weekday = weekday;
                }
                None => {
                    m.trigger_kind = TriggerKind::Cron;
                    m.cron_expr.set(expr);
                }
            },
        }
        if let Some(tz) = &auto.timezone {
            m.timezone.set(tz);
        }
        m.prompt.set(&auto.prompt);
        match &auto.action {
            AutomationAction::Send { .. } => {
                m.action = AutomationActionKind::Send;
                // The target list + selected index are filled in by the caller
                // via `set_target_sessions` (it has the running-session list).
            }
            AutomationAction::Spawn {
                repo_path,
                worktree_branch,
                agent,
                ..
            } => {
                m.action = AutomationActionKind::Spawn;
                m.repo.set(&repo_path.to_string_lossy());
                if let Some(w) = worktree_branch {
                    m.worktree.set(w);
                }
                if let Some(a) = agent {
                    m.agent.set(a);
                }
            }
        }
        m
    }
}

/// Add `delta` to `v` modulo `modulus`, wrapping (e.g. hour 23 +1 → 0).
fn wrap_add(v: u32, delta: i32, modulus: u32) -> u32 {
    (v as i32 + delta).rem_euclid(modulus as i32) as u32
}

/// Format a millisecond duration as a compact, re-enterable string like
/// `1h30m` (matching [`crate::session::automation::parse_duration`]).
fn format_duration_short(ms: u64) -> String {
    let total_secs = ms / 1000;
    let days = total_secs / 86_400;
    let hours = (total_secs % 86_400) / 3_600;
    let mins = (total_secs % 3_600) / 60;
    let mut out = String::new();
    if days > 0 {
        out.push_str(&format!("{days}d"));
    }
    if hours > 0 {
        out.push_str(&format!("{hours}h"));
    }
    if mins > 0 || out.is_empty() {
        out.push_str(&format!("{mins}m"));
    }
    out
}

/// Best-effort reverse mapping of a cron expression generated by this editor
/// back into `(TriggerKind, hour, minute, weekday)`. Returns `None` for
/// expressions that don't match a known preset shape (those stay raw `Cron`).
fn recognize_cron(expr: &str) -> Option<(TriggerKind, u32, u32, u32)> {
    let f: Vec<&str> = expr.split_whitespace().collect();
    if f.len() != 5 {
        return None;
    }
    let (min, hour, dom, mon, dow) = (f[0], f[1], f[2], f[3], f[4]);
    if dom != "*" || mon != "*" {
        return None;
    }
    let minute: u32 = min.parse().ok()?;
    if minute >= 60 {
        return None;
    }
    // Hourly: `m * * * *`.
    if hour == "*" && dow == "*" {
        return Some((TriggerKind::Hourly, 0, minute, 1));
    }
    let h: u32 = hour.parse().ok()?;
    if h >= 24 {
        return None;
    }
    match dow {
        "*" => Some((TriggerKind::Daily, h, minute, 1)),
        "1-5" => Some((TriggerKind::Weekdays, h, minute, 1)),
        single => {
            let d: u32 = single.parse().ok()?;
            (d <= 6).then_some((TriggerKind::Weekly, h, minute, d))
        }
    }
}

// ── AutomationsListModal ────────────────────────────────────────────────

/// An entry in the automations list modal.
#[derive(Debug, Clone)]
pub struct AutomationListEntry {
    pub id: i64,
    pub name: String,
    pub summary: String,
    pub enabled: bool,
}

/// Modal state for listing and managing automations.
#[derive(Debug, Clone, Default)]
pub struct AutomationsListModal {
    pub index: usize,
    pub entries: Vec<AutomationListEntry>,
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

// ── TaskEditorModal ─────────────────────────────────────────────────────

/// How a task connects to an agent. Mirrors [`AutomationActionKind`] but adds a
/// `Local` arm for an unconnected todo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskActionKind {
    /// Plain local todo — no agent action.
    #[default]
    Local,
    /// Paste the task title into an existing session.
    Send,
    /// Spawn a new session and prompt it with the task title.
    Spawn,
}

impl TaskActionKind {
    const ALL: [TaskActionKind; 3] = [Self::Local, Self::Send, Self::Spawn];

    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Send => "send",
            Self::Spawn => "spawn",
        }
    }

    fn step(self, delta: i32) -> Self {
        let idx = Self::ALL.iter().position(|k| *k == self).unwrap_or(0) as i32;
        let len = Self::ALL.len() as i32;
        Self::ALL[(idx + delta).rem_euclid(len) as usize]
    }

    /// The editor fields shown for this action kind, in navigation order. The
    /// single source for both the app's field navigation and the UI renderer.
    pub fn visible_fields(self) -> Vec<TaskField> {
        use TaskField::*;
        let mut fields = vec![Title, Status, Action];
        match self {
            Self::Local => {}
            Self::Send => fields.push(Target),
            Self::Spawn => fields.extend([Repo, Worktree, Base, Agent]),
        }
        fields
    }
}

/// Focusable field in the task editor. The set shown depends on the action kind
/// (see `TaskEditorModal::visible_fields`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskField {
    #[default]
    Title,
    /// Status selector (cycled with ←/→).
    Status,
    /// Action-kind selector (cycled with ←/→).
    Action,
    /// Send action: target-session selector.
    Target,
    Repo,
    Worktree,
    Base,
    Agent,
}

/// Editor form for creating or editing a task.
#[derive(Debug, Clone)]
pub struct TaskEditorModal {
    /// `Some` when editing an existing task.
    pub editing_id: Option<i64>,
    pub title: TextInput,
    pub status: crate::session::TaskStatus,
    pub action: TaskActionKind,
    /// Spawn action: repository path.
    pub repo: TextInput,
    /// Spawn action: optional worktree branch.
    pub worktree: TextInput,
    /// Spawn action: optional base branch.
    pub base: TextInput,
    /// Spawn action: optional agent name.
    pub agent: TextInput,
    pub field: TaskField,
    /// Send action: the running sessions available as targets (id + display
    /// name), captured at open and cycled with the `Target` field.
    pub sessions: Vec<(crate::session::SessionId, String)>,
    /// Index into `sessions` of the selected Send target.
    pub target_index: usize,
}

impl TaskEditorModal {
    /// A blank editor for a new task, with the available Send targets.
    pub fn new(sessions: Vec<(crate::session::SessionId, String)>) -> Self {
        Self {
            editing_id: None,
            title: TextInput::default(),
            status: crate::session::TaskStatus::Todo,
            action: TaskActionKind::Local,
            repo: TextInput::default(),
            worktree: TextInput::default(),
            base: TextInput::default(),
            agent: TextInput::default(),
            field: TaskField::default(),
            sessions,
            target_index: 0,
        }
    }

    /// Build an editor pre-filled from an existing task.
    pub fn from_task(
        task: &crate::session::Task,
        sessions: Vec<(crate::session::SessionId, String)>,
    ) -> Self {
        use crate::session::AutomationAction;
        let mut m = Self::new(sessions);
        m.editing_id = Some(task.id);
        m.title.set(&task.title);
        m.status = task.status;
        match &task.action {
            None => m.action = TaskActionKind::Local,
            Some(AutomationAction::Send { session_id }) => {
                m.action = TaskActionKind::Send;
                m.set_default_target(Some(*session_id));
            }
            Some(AutomationAction::Spawn {
                repo_path,
                worktree_branch,
                base_branch,
                agent,
            }) => {
                m.action = TaskActionKind::Spawn;
                m.repo.set(&repo_path.to_string_lossy());
                if let Some(w) = worktree_branch {
                    m.worktree.set(w);
                }
                if let Some(b) = base_branch {
                    m.base.set(b);
                }
                if let Some(a) = agent {
                    m.agent.set(a);
                }
            }
        }
        m
    }

    /// Select `selected` in the target list (falling back to the first session).
    pub fn set_default_target(&mut self, selected: Option<crate::session::SessionId>) {
        self.target_index = selected
            .and_then(|id| self.sessions.iter().position(|(sid, _)| *sid == id))
            .unwrap_or(0);
    }

    /// The fields shown for the current action kind, in navigation order.
    pub fn visible_fields(&self) -> Vec<TaskField> {
        self.action.visible_fields()
    }

    /// Move focus to the next visible field (wraps).
    pub fn next_field(&mut self) {
        self.field = cycle_field(&self.visible_fields(), self.field, 1);
    }

    /// Move focus to the previous visible field (wraps).
    pub fn prev_field(&mut self) {
        self.field = cycle_field(&self.visible_fields(), self.field, -1);
    }

    /// The focused text field, or `None` for selector fields (adjusted with ←/→).
    pub fn active_field_mut(&mut self) -> Option<&mut TextInput> {
        use TaskField::*;
        Some(match self.field {
            Repo => &mut self.repo,
            Worktree => &mut self.worktree,
            Base => &mut self.base,
            Agent => &mut self.agent,
            Title => &mut self.title,
            Status | Action | Target => return None,
        })
    }

    /// Whether the focused field is a selector adjusted with ←/→/Space.
    pub fn is_adjustable(&self) -> bool {
        matches!(
            self.field,
            TaskField::Status | TaskField::Action | TaskField::Target
        )
    }

    /// Adjust the focused selector by `delta` (−1 for ←, +1 for →/Space).
    pub fn adjust(&mut self, delta: i32) {
        match self.field {
            TaskField::Status => {
                // Cycle in either direction (cycle() only goes forward, so step
                // backward via two forward cycles).
                self.status = if delta < 0 {
                    self.status.cycle().cycle()
                } else {
                    self.status.cycle()
                };
            }
            TaskField::Action => {
                self.action = self.action.step(delta);
                // Snap focus back to a valid field when the field set shrinks.
                if !self.visible_fields().contains(&self.field) {
                    self.field = TaskField::Action;
                }
            }
            TaskField::Target => {
                let len = self.sessions.len();
                if len > 0 {
                    self.target_index =
                        wrap_add(self.target_index as u32, delta, len as u32) as usize;
                }
            }
            _ => {}
        }
    }

    /// The currently selected Send target (id + display name), if any.
    pub fn selected_target(&self) -> Option<&(crate::session::SessionId, String)> {
        self.sessions.get(self.target_index)
    }

    /// Build the [`AutomationAction`] (or `None` for a local task) described by
    /// the current fields. Returns a user-facing error for invalid input.
    pub fn build_action(&self) -> Result<Option<crate::session::AutomationAction>, String> {
        use crate::session::AutomationAction;
        Ok(match self.action {
            TaskActionKind::Local => None,
            TaskActionKind::Send => {
                let Some((session_id, _)) = self.selected_target() else {
                    return Err("No target session — start a session first".into());
                };
                Some(AutomationAction::Send {
                    session_id: *session_id,
                })
            }
            TaskActionKind::Spawn => {
                let repo = self.repo.value().trim();
                if repo.is_empty() {
                    return Err("Repo path required for spawn action".into());
                }
                let worktree = self.worktree.value().trim();
                let base = self.base.value().trim();
                let agent = self.agent.value().trim();
                Some(AutomationAction::Spawn {
                    repo_path: crate::paths::expand_tilde(repo),
                    worktree_branch: (!worktree.is_empty()).then(|| worktree.to_string()),
                    base_branch: (!base.is_empty()).then(|| base.to_string()),
                    agent: (!agent.is_empty()).then(|| agent.to_string()),
                })
            }
        })
    }

    /// Feed a key to the editor. Returns whether the caller should save
    /// (`Enter`), cancel (`Esc`), or keep editing.
    pub fn handle_key(&mut self, code: KeyCode, _mods: KeyModifiers) -> EditorOutcome {
        let adjustable = self.is_adjustable();
        match code {
            KeyCode::Esc => return EditorOutcome::Cancel,
            KeyCode::Enter => return EditorOutcome::Save,
            KeyCode::Tab | KeyCode::Down => self.next_field(),
            KeyCode::BackTab | KeyCode::Up => self.prev_field(),
            KeyCode::Left if adjustable => self.adjust(-1),
            KeyCode::Right | KeyCode::Char(' ') if adjustable => self.adjust(1),
            other => {
                apply_text_input_key(self.active_field_mut(), other);
            }
        }
        EditorOutcome::Continue
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
    AutomationEditor(AutomationEditorModal),
    AutomationsList(AutomationsListModal),
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
    fn cycle_field_wraps_both_directions() {
        let fields = ['a', 'b', 'c'];
        assert_eq!(cycle_field(&fields, 'a', 1), 'b');
        assert_eq!(cycle_field(&fields, 'c', 1), 'a'); // wrap forward
        assert_eq!(cycle_field(&fields, 'a', -1), 'c'); // wrap backward
        assert_eq!(cycle_field(&fields, 'b', -1), 'a');
        // Unknown current value falls back to index 0, then steps.
        assert_eq!(cycle_field(&fields, 'z', 1), 'b');
        // Empty slice returns the input unchanged.
        assert_eq!(cycle_field::<char>(&[], 'x', 1), 'x');
    }

    #[test]
    fn apply_text_input_key_edits_and_reports_handled() {
        let mut input = TextInput::new();
        input.set("ab");
        assert!(apply_text_input_key(Some(&mut input), KeyCode::Char('c')));
        assert_eq!(input.value(), "abc");
        assert!(apply_text_input_key(Some(&mut input), KeyCode::Backspace));
        assert_eq!(input.value(), "ab");
        // Non-text keys are not handled.
        assert!(!apply_text_input_key(Some(&mut input), KeyCode::Enter));
        assert!(!apply_text_input_key(Some(&mut input), KeyCode::Tab));
        // A text key with no focused field is still "handled" (a no-op).
        assert!(apply_text_input_key(None, KeyCode::Char('x')));
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
    fn test_automation_editor_default() {
        let modal = AutomationEditorModal::default();
        assert_eq!(modal.name.value(), "");
        assert_eq!(modal.field, AutomationField::Name);
        assert_eq!(modal.trigger_kind, TriggerKind::Daily);
        assert_eq!(modal.hour, 9);
        assert_eq!(modal.minute, 0);
        assert_eq!(modal.action, AutomationActionKind::Send);
        assert!(modal.enabled, "new automations default to enabled");
        assert!(modal.editing_id.is_none());
    }

    #[test]
    fn test_automation_editor_active_field() {
        let mut modal = AutomationEditorModal::default();
        modal.active_field_mut().unwrap().insert('x');
        assert_eq!(modal.name.value(), "x");
        // Selector/stepper fields have no text input.
        for f in [
            AutomationField::Trigger,
            AutomationField::Hour,
            AutomationField::Minute,
            AutomationField::Action,
        ] {
            modal.field = f;
            assert!(modal.active_field_mut().is_none(), "{f:?} is not text");
            assert!(modal.is_adjustable());
        }
    }

    #[test]
    fn test_automation_editor_daily_field_order_for_send() {
        let mut modal = AutomationEditorModal::default(); // Daily + Send
        let order: Vec<_> = (0..8)
            .map(|_| {
                let f = modal.field;
                modal.next_field();
                f
            })
            .collect();
        assert_eq!(
            order,
            vec![
                AutomationField::Name,
                AutomationField::Trigger,
                AutomationField::Hour,
                AutomationField::Minute,
                AutomationField::Timezone,
                AutomationField::Action,
                // Send exposes a target-session selector after the action.
                AutomationField::Target,
                AutomationField::Prompt,
            ]
        );
        assert_eq!(modal.field, AutomationField::Name);
    }

    #[test]
    fn test_automation_editor_steppers_wrap() {
        let mut modal = AutomationEditorModal {
            hour: 23,
            field: AutomationField::Hour,
            ..Default::default()
        };
        modal.adjust(1);
        assert_eq!(modal.hour, 0);
        modal.adjust(-1);
        assert_eq!(modal.hour, 23);

        modal.minute = 0;
        modal.field = AutomationField::Minute;
        modal.adjust(-1);
        assert_eq!(modal.minute, 59);

        modal.field = AutomationField::Trigger;
        modal.trigger_kind = TriggerKind::Once;
        modal.adjust(-1);
        assert_eq!(modal.trigger_kind, TriggerKind::Cron, "wraps backward");
    }

    #[test]
    fn test_automation_editor_build_schedule() {
        use crate::session::AutomationSchedule;
        let mut modal = AutomationEditorModal::default(); // Daily 09:00
        assert_eq!(
            modal.build_schedule(0).unwrap(),
            AutomationSchedule::Cron {
                expr: "0 9 * * *".into()
            }
        );

        modal.trigger_kind = TriggerKind::Weekdays;
        assert_eq!(
            modal.build_schedule(0).unwrap(),
            AutomationSchedule::Cron {
                expr: "0 9 * * 1-5".into()
            }
        );

        modal.trigger_kind = TriggerKind::Once;
        modal.delay.set("30m");
        assert_eq!(
            modal.build_schedule(1000).unwrap(),
            AutomationSchedule::Once {
                at: 1_800_000 + 1000
            }
        );

        modal.delay.set("bogus");
        assert!(modal.build_schedule(0).is_err());
    }

    #[test]
    fn test_automation_editor_spawn_shows_extra_fields() {
        let mut modal = AutomationEditorModal {
            action: AutomationActionKind::Spawn,
            ..Default::default()
        };
        assert!(modal.visible_fields().contains(&AutomationField::Repo));
        modal.toggle_action();
        assert_eq!(modal.action, AutomationActionKind::Send);
        assert!(!modal.visible_fields().contains(&AutomationField::Repo));
    }

    #[test]
    fn test_automations_list_modal_default() {
        let modal = AutomationsListModal::default();
        assert_eq!(modal.index, 0);
        assert!(modal.entries.is_empty());
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
    fn test_automation_editor_from_spawn_automation() {
        use crate::session::{Automation, AutomationAction, AutomationSchedule};
        let auto = Automation {
            id: 7,
            name: "nightly".into(),
            enabled: false,
            schedule: AutomationSchedule::Cron {
                expr: "0 9 * * 1-5".into(),
            },
            timezone: Some("UTC".into()),
            action: AutomationAction::Spawn {
                repo_path: "/tmp/repo".into(),
                worktree_branch: Some("feat/x".into()),
                base_branch: None,
                agent: Some("codex".into()),
            },
            prompt: "triage".into(),
            created_at: 0,
            updated_at: 0,
            last_run_at: None,
            next_run_at: None,
        };
        let modal = AutomationEditorModal::from_automation(&auto);
        assert_eq!(modal.editing_id, Some(7));
        assert!(!modal.enabled);
        // `0 9 * * 1-5` is recognized as the Weekdays preset at 09:00.
        assert_eq!(modal.trigger_kind, TriggerKind::Weekdays);
        assert_eq!(modal.hour, 9);
        assert_eq!(modal.minute, 0);
        assert_eq!(modal.action, AutomationActionKind::Spawn);
        assert_eq!(modal.repo.value(), "/tmp/repo");
        assert_eq!(modal.worktree.value(), "feat/x");
        assert_eq!(modal.agent.value(), "codex");
    }

    #[test]
    fn test_recognize_cron_presets_and_raw() {
        assert_eq!(
            recognize_cron("30 * * * *"),
            Some((TriggerKind::Hourly, 0, 30, 1))
        );
        assert_eq!(
            recognize_cron("0 9 * * *"),
            Some((TriggerKind::Daily, 9, 0, 1))
        );
        assert_eq!(
            recognize_cron("0 9 * * 1-5"),
            Some((TriggerKind::Weekdays, 9, 0, 1))
        );
        assert_eq!(
            recognize_cron("15 8 * * 3"),
            Some((TriggerKind::Weekly, 8, 15, 3))
        );
        // Anything irregular stays raw.
        assert_eq!(recognize_cron("0 9 1 * *"), None);
        assert_eq!(recognize_cron("*/5 * * * *"), None);
        assert_eq!(recognize_cron("0 9 * *"), None);
    }

    #[test]
    fn test_format_duration_short() {
        assert_eq!(format_duration_short(1_800_000), "30m");
        assert_eq!(format_duration_short(5_400_000), "1h30m");
        assert_eq!(format_duration_short(90_000_000), "1d1h");
        assert_eq!(format_duration_short(0), "0m");
    }

    #[test]
    fn task_editor_local_visible_fields_and_action() {
        let m = TaskEditorModal::new(Vec::new());
        assert_eq!(
            m.visible_fields(),
            vec![TaskField::Title, TaskField::Status, TaskField::Action]
        );
        assert_eq!(m.build_action().unwrap(), None);
    }

    #[test]
    fn task_action_kind_visible_fields_per_kind() {
        use TaskField::*;
        assert_eq!(
            TaskActionKind::Local.visible_fields(),
            vec![Title, Status, Action]
        );
        assert_eq!(
            TaskActionKind::Send.visible_fields(),
            vec![Title, Status, Action, Target]
        );
        assert_eq!(
            TaskActionKind::Spawn.visible_fields(),
            vec![Title, Status, Action, Repo, Worktree, Base, Agent]
        );
        // The modal method must delegate to the canonical action method.
        let mut m = TaskEditorModal::new(Vec::new());
        m.action = TaskActionKind::Spawn;
        assert_eq!(m.visible_fields(), TaskActionKind::Spawn.visible_fields());
    }

    #[test]
    fn task_editor_action_cycles_local_send_spawn() {
        let mut m = TaskEditorModal::new(Vec::new());
        m.field = TaskField::Action;
        assert_eq!(m.action, TaskActionKind::Local);
        m.adjust(1);
        assert_eq!(m.action, TaskActionKind::Send);
        m.adjust(1);
        assert_eq!(m.action, TaskActionKind::Spawn);
        // Spawn exposes repo/worktree/base/agent.
        assert!(m.visible_fields().contains(&TaskField::Repo));
        m.adjust(1);
        assert_eq!(m.action, TaskActionKind::Local);
    }

    #[test]
    fn task_editor_spawn_requires_repo() {
        let mut m = TaskEditorModal::new(Vec::new());
        m.action = TaskActionKind::Spawn;
        assert!(m.build_action().is_err());
        m.repo.set("/tmp/repo");
        m.worktree.set("feat/x");
        match m.build_action().unwrap() {
            Some(crate::session::AutomationAction::Spawn { repo_path, .. }) => {
                assert_eq!(repo_path, std::path::PathBuf::from("/tmp/repo"));
            }
            other => panic!("expected spawn, got {other:?}"),
        }
    }

    #[test]
    fn task_editor_save_and_cancel_outcomes() {
        let mut m = TaskEditorModal::new(Vec::new());
        assert_eq!(
            m.handle_key(KeyCode::Enter, KeyModifiers::NONE),
            EditorOutcome::Save
        );
        assert_eq!(
            m.handle_key(KeyCode::Esc, KeyModifiers::NONE),
            EditorOutcome::Cancel
        );
        // Typing edits the title field.
        m.handle_key(KeyCode::Char('h'), KeyModifiers::NONE);
        m.handle_key(KeyCode::Char('i'), KeyModifiers::NONE);
        assert_eq!(m.title.value(), "hi");
    }

    #[test]
    fn task_editor_status_selector_cycles_both_ways() {
        let mut m = TaskEditorModal::new(Vec::new());
        m.field = TaskField::Status;
        assert_eq!(m.status, crate::session::TaskStatus::Todo);
        m.adjust(1);
        assert_eq!(m.status, crate::session::TaskStatus::InProgress);
        m.adjust(-1);
        assert_eq!(m.status, crate::session::TaskStatus::Todo);
    }
}
