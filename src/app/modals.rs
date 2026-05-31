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
    /// Send action: target session (id + display name), captured at open.
    pub target_session: Option<(crate::session::SessionId, String)>,
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
            target_session: None,
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
        if self.action == AutomationActionKind::Spawn {
            fields.extend([Repo, Worktree, Agent]);
        }
        fields.push(Prompt);
        fields
    }

    /// Move focus to the next visible field (wraps).
    pub fn next_field(&mut self) {
        let fields = self.visible_fields();
        let idx = fields.iter().position(|f| *f == self.field).unwrap_or(0);
        self.field = fields[(idx + 1) % fields.len()];
    }

    /// Move focus to the previous visible field (wraps).
    pub fn prev_field(&mut self) {
        let fields = self.visible_fields();
        let idx = fields.iter().position(|f| *f == self.field).unwrap_or(0);
        self.field = fields[(idx + fields.len() - 1) % fields.len()];
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
            Trigger | Weekday | Hour | Minute | Action => return None,
        })
    }

    /// Whether the focused field is a selector/stepper adjusted with ←/→/Space
    /// rather than edited as text.
    pub fn is_adjustable(&self) -> bool {
        use AutomationField::*;
        matches!(self.field, Trigger | Weekday | Hour | Minute | Action)
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
            _ => {}
        }
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
    /// the schedule back into structured fields where possible.
    pub fn from_automation(
        auto: &crate::session::Automation,
        session_name: Option<String>,
    ) -> Self {
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
            AutomationAction::Send { session_id } => {
                m.action = AutomationActionKind::Send;
                m.target_session = Some((
                    *session_id,
                    session_name.unwrap_or_else(|| session_id.to_string()),
                ));
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
        let order: Vec<_> = (0..7)
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
        let modal = AutomationEditorModal::from_automation(&auto, None);
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
}
