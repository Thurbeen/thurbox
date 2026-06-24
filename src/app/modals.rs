// Modal state management for Thurbox TUI: a single discriminated `Modal` enum
// makes invalid states (two modals open at once) unrepresentable.

use std::collections::HashSet;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyModifiers};

use crate::storage::DeletedSessionInfo;

// ── TextInput Helper ────────────────────────────────────────────────────────

/// Feed pasted `text` to `insert` one char at a time, dropping control
/// characters. When `keep_newlines` is set, `\n` is preserved (multi-line
/// fields); every other control char (including `\r`) is always dropped so a
/// single-line field never gains a line break and `\r\n` collapses to one.
fn insert_pasted(text: &str, keep_newlines: bool, mut insert: impl FnMut(char)) {
    for c in text.chars() {
        if (keep_newlines && c == '\n') || !c.is_control() {
            insert(c);
        }
    }
}

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

    /// Insert pasted text at the cursor. Control characters (newlines, tabs, …)
    /// are dropped so a single-line input never gains a line break from a paste.
    pub fn insert_str(&mut self, s: &str) {
        insert_pasted(s, false, |c| self.insert(c));
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
        byte_index(&self.buffer, self.cursor)
    }
}

/// Multi-line text input with a char-indexed cursor over a `\n`-delimited
/// buffer. Backs the task editor's description field. Mirrors [`TextInput`] but
/// adds newline insertion and vertical (line) cursor movement.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextArea {
    buffer: String,
    /// Cursor as a char index into `buffer` (each `\n` counts as one char).
    cursor: usize,
}

impl TextArea {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, c: char) {
        let byte_pos = self.byte_offset();
        self.buffer.insert(byte_pos, c);
        self.cursor += 1;
    }

    /// Insert a line break at the cursor.
    pub fn insert_newline(&mut self) {
        self.insert('\n');
    }

    /// Insert pasted text at the cursor, preserving newlines (this is a
    /// multi-line field). `\r` is dropped and other control characters are
    /// skipped so a pasted `\r\n` collapses to a single line break.
    pub fn insert_str(&mut self, s: &str) {
        insert_pasted(s, true, |c| self.insert(c));
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
        if self.cursor < self.char_count() {
            self.cursor += 1;
        }
    }

    /// Move up one line, keeping the column (clamped to the shorter line).
    pub fn move_up(&mut self) {
        let (line, col) = self.cursor_line_col();
        if line > 0 {
            self.cursor = self.cursor_at(line - 1, col);
        }
    }

    /// Move down one line, keeping the column (clamped to the shorter line).
    pub fn move_down(&mut self) {
        let (line, col) = self.cursor_line_col();
        if line + 1 < self.buffer.split('\n').count() {
            self.cursor = self.cursor_at(line + 1, col);
        }
    }

    /// Move to the start of the current line.
    pub fn home(&mut self) {
        let (line, _) = self.cursor_line_col();
        self.cursor = self.cursor_at(line, 0);
    }

    /// Move to the end of the current line.
    pub fn end(&mut self) {
        let (line, _) = self.cursor_line_col();
        self.cursor = self.cursor_at(line, usize::MAX);
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    pub fn set(&mut self, value: &str) {
        self.buffer = value.to_string();
        self.cursor = self.char_count();
    }

    pub fn value(&self) -> &str {
        &self.buffer
    }

    /// Cursor as zero-based `(line, col)` in chars, for drawing the block cursor.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let mut line = 0;
        let mut col = 0;
        for c in self.buffer.chars().take(self.cursor) {
            if c == '\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    fn char_count(&self) -> usize {
        self.buffer.chars().count()
    }

    /// Char index of `(line, col)`, clamping `col` to that line's length and
    /// `line` past the end to the buffer end.
    fn cursor_at(&self, line: usize, col: usize) -> usize {
        let mut idx = 0;
        for (l, text) in self.buffer.split('\n').enumerate() {
            let len = text.chars().count();
            if l == line {
                return idx + col.min(len);
            }
            idx += len + 1;
        }
        self.char_count()
    }

    /// Convert the char-based cursor to a byte offset into `buffer`.
    fn byte_offset(&self) -> usize {
        byte_index(&self.buffer, self.cursor)
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

/// Byte offset of char index `idx` in `s` (end of string when out of range).
/// Shared by both text widgets, which index a `char` cursor into a byte buffer.
fn byte_index(s: &str, idx: usize) -> usize {
    s.char_indices().nth(idx).map(|(i, _)| i).unwrap_or(s.len())
}

/// Delete the chars in `[start, cursor)` (char indices) from `buffer` and park
/// `cursor` at `start`. A no-op when `start >= cursor`. Backs every backward
/// line-edit (`Ctrl+W` word, `Ctrl+U` line) of both text widgets.
fn remove_before_cursor(buffer: &mut String, cursor: &mut usize, start: usize) {
    if start >= *cursor {
        return;
    }
    let (s, e) = (byte_index(buffer, start), byte_index(buffer, *cursor));
    buffer.replace_range(s..e, "");
    *cursor = start;
}

/// Delete the chars in `[cursor, end)` (char indices) from `buffer`, leaving
/// `cursor` in place. A no-op when `end <= cursor`. Backs the forward line-edit
/// (`Ctrl+K` kill to line end) of both text widgets.
fn remove_after_cursor(buffer: &mut String, cursor: usize, end: usize) {
    if end <= cursor {
        return;
    }
    let (s, e) = (byte_index(buffer, cursor), byte_index(buffer, end));
    buffer.replace_range(s..e, "");
}

/// Char index of the start of the word ending at `cursor`: skip any trailing
/// whitespace, then the run of non-whitespace before it. Backs the `Ctrl+W`
/// handlers of both text widgets (newlines count as whitespace).
fn word_start_before(chars: &[char], cursor: usize) -> usize {
    let mut start = cursor;
    while start > 0 && chars[start - 1].is_whitespace() {
        start -= 1;
    }
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    start
}

/// Readline-style line editing shared by the single-line [`TextInput`] and the
/// multi-line [`TextArea`], so the Ctrl-chord dispatch lives in one place. The
/// cursor-move and single-char delete chords delegate to each widget's existing
/// inherent methods; only the word/line kills carry their own logic.
trait LineEdit {
    /// Delete the word before the cursor (readline `Ctrl+W`).
    fn delete_word_before(&mut self);
    /// Delete from the cursor to the start of the current line (readline `Ctrl+U`).
    fn kill_to_line_start(&mut self);
    /// Delete from the cursor to the end of the current line (readline `Ctrl+K`).
    fn kill_to_line_end(&mut self);
    /// Move the cursor to the start of the current line (readline `Ctrl+A`).
    fn line_start(&mut self);
    /// Move the cursor to the end of the current line (readline `Ctrl+E`).
    fn line_end(&mut self);
    /// Move the cursor one char left (readline `Ctrl+B`).
    fn cursor_left(&mut self);
    /// Move the cursor one char right (readline `Ctrl+F`).
    fn cursor_right(&mut self);
    /// Delete the char under the cursor (readline `Ctrl+D`).
    fn delete_forward(&mut self);
    /// Delete the char before the cursor (readline `Ctrl+H`, like Backspace).
    fn delete_backward(&mut self);
}

/// The `LineEdit` methods identical for both text widgets: the word kill plus
/// the cursor-move / single-char deletes that delegate to each widget's
/// inherent methods. Defined once here so the single-line and multi-line impls
/// share one copy (the line kills, which differ per widget, stay inline). Both
/// widgets expose the `buffer`/`cursor` fields and inherent methods this needs.
macro_rules! line_edit_shared {
    () => {
        fn delete_word_before(&mut self) {
            let chars: Vec<char> = self.buffer.chars().collect();
            let start = word_start_before(&chars, self.cursor);
            remove_before_cursor(&mut self.buffer, &mut self.cursor, start);
        }
        fn line_start(&mut self) {
            self.home();
        }
        fn line_end(&mut self) {
            self.end();
        }
        fn cursor_left(&mut self) {
            self.move_left();
        }
        fn cursor_right(&mut self) {
            self.move_right();
        }
        fn delete_forward(&mut self) {
            self.delete();
        }
        fn delete_backward(&mut self) {
            self.backspace();
        }
    };
}

impl LineEdit for TextInput {
    line_edit_shared!();

    /// For a single-line input this clears everything before the cursor.
    fn kill_to_line_start(&mut self) {
        remove_before_cursor(&mut self.buffer, &mut self.cursor, 0);
    }

    /// For a single-line input this clears everything from the cursor onward.
    fn kill_to_line_end(&mut self) {
        let end = self.buffer.chars().count();
        remove_after_cursor(&mut self.buffer, self.cursor, end);
    }
}

impl LineEdit for TextArea {
    line_edit_shared!();

    fn kill_to_line_start(&mut self) {
        let (line, _) = self.cursor_line_col();
        let line_start = self.cursor_at(line, 0);
        remove_before_cursor(&mut self.buffer, &mut self.cursor, line_start);
    }

    /// Kill to the end of the current line; at the line end (with text below)
    /// this consumes the trailing newline, joining the next line — matching
    /// readline/emacs `Ctrl+K`.
    fn kill_to_line_end(&mut self) {
        let (line, _) = self.cursor_line_col();
        let mut end = self.cursor_at(line, usize::MAX);
        if end == self.cursor && self.cursor < self.char_count() {
            end = self.cursor + 1;
        }
        remove_after_cursor(&mut self.buffer, self.cursor, end);
    }
}

/// Apply a readline `Ctrl`+letter line-edit chord to `field`, giving modal text
/// fields the same emacs-style editing as a terminal:
///
/// - `Ctrl+A` / `Ctrl+E` — move to line start / end
/// - `Ctrl+B` / `Ctrl+F` — move one char left / right
/// - `Ctrl+H` / `Ctrl+D` — delete the char before / under the cursor
/// - `Ctrl+W` — delete the word before the cursor
/// - `Ctrl+U` / `Ctrl+K` — kill to line start / end
///
/// Returns `true` when `code` is a `Ctrl`+letter chord — handled or swallowed
/// (any unmapped letter) — so the caller treats it as consumed and never
/// inserts the bare letter as text. `Ctrl` with a non-letter key (arrows,
/// Home/End) returns `false` so normal cursor handling still applies.
fn apply_ctrl_line_edit(field: &mut impl LineEdit, code: KeyCode, mods: KeyModifiers) -> bool {
    if !mods.contains(KeyModifiers::CONTROL) {
        return false;
    }
    let KeyCode::Char(c) = code else {
        return false; // Ctrl+<non-letter>: defer to normal handling
    };
    match c.to_ascii_lowercase() {
        'a' => field.line_start(),
        'e' => field.line_end(),
        'b' => field.cursor_left(),
        'f' => field.cursor_right(),
        'h' => field.delete_backward(),
        'd' => field.delete_forward(),
        'w' => field.delete_word_before(),
        'u' => field.kill_to_line_start(),
        'k' => field.kill_to_line_end(),
        _ => {} // other Ctrl+letter: swallow (never insert)
    }
    true
}

/// Apply a key to a multi-line [`TextArea`] field: `Enter` inserts a newline,
/// `Up`/`Down` move within the text, and the rest edit / move the cursor.
/// Returns `true` when the key was consumed. `Esc` (cancel) and `Tab`/`BackTab`
/// (field navigation) are deliberately *not* handled here — they belong to the
/// owning editor — and `Ctrl` chords should be routed through
/// [`apply_ctrl_line_edit`] first. Shared by the automation `Prompt` and task
/// `Description` fields so their editing behavior can't drift apart.
fn handle_textarea_key(area: &mut TextArea, code: KeyCode) -> bool {
    match code {
        KeyCode::Enter => area.insert_newline(),
        KeyCode::Up => area.move_up(),
        KeyCode::Down => area.move_down(),
        KeyCode::Char(c) => area.insert(c),
        KeyCode::Backspace => area.backspace(),
        KeyCode::Delete => area.delete(),
        KeyCode::Left => area.move_left(),
        KeyCode::Right => area.move_right(),
        KeyCode::Home => area.home(),
        KeyCode::End => area.end(),
        _ => return false,
    }
    true
}

/// Apply a text-editing key (insert/backspace/delete/cursor move + readline
/// `Ctrl+W`/`Ctrl+U`) to the currently focused field, if any. Returns `true`
/// when `code`+`mods` was a text-editing key (whether or not a field was
/// focused), so editor key handlers can share one implementation across the
/// automation and task forms. A `Ctrl`+letter chord is consumed here (W/U edit,
/// the rest swallowed) so no literal control-letter leaks into the field; a
/// `Ctrl`+<non-letter> chord (arrows, Home/End) falls through to normal handling.
pub(super) fn apply_text_input_key(
    field: Option<&mut TextInput>,
    code: KeyCode,
    mods: KeyModifiers,
) -> bool {
    if mods.contains(KeyModifiers::CONTROL) {
        if let KeyCode::Char(_) = code {
            if let Some(f) = field {
                apply_ctrl_line_edit(f, code, mods);
            }
            return true;
        }
        // Ctrl+<non-letter> (arrows, Home/End): fall through to normal handling.
    }
    if !is_text_input_key(code) {
        return false;
    }
    if let Some(f) = field {
        apply_text_edit_op(f, code);
    }
    true
}

/// Whether `code` is a text-editing key handled by [`apply_text_input_key`]
/// (insert/backspace/delete/cursor move).
fn is_text_input_key(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Char(_)
            | KeyCode::Backspace
            | KeyCode::Delete
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Home
            | KeyCode::End
    )
}

/// Apply a single text-editing key to a focused field. `code` must be a key for
/// which [`is_text_input_key`] returns `true`; anything else is a no-op.
fn apply_text_edit_op(f: &mut TextInput, code: KeyCode) {
    match code {
        KeyCode::Char(c) => f.insert(c),
        KeyCode::Backspace => f.backspace(),
        KeyCode::Delete => f.delete(),
        KeyCode::Left => f.move_left(),
        KeyCode::Right => f.move_right(),
        KeyCode::Home => f.home(),
        KeyCode::End => f.end(),
        _ => {}
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

#[derive(Debug, Clone)]
pub struct ThemePickerModal {
    pub index: usize,
    /// Palette active when the picker opened. The picker live-previews by
    /// mutating the global palette as the selection moves, so cancelling
    /// (`Esc`) restores this snapshot; only confirming (`Enter`) persists.
    pub original: crate::session::ThemePalette,
}

/// What a hard delete would destroy: uncommitted changes and/or unmerged
/// commits across a session's worktree(s). `unknown` means the state could not
/// be determined (a remote-host session or a failed git query), in which case
/// we confirm anyway. Drives whether the confirmation prompt is shown at all
/// and what it lists. See [`DeleteRisk::from_stats`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeleteRisk {
    pub dirty: bool,
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub untracked: usize,
    pub ahead: usize,
    pub unknown: bool,
}

impl DeleteRisk {
    /// Risk could not be determined (remote session / git error) → confirm.
    pub fn unknown() -> Self {
        DeleteRisk {
            unknown: true,
            ..Default::default()
        }
    }

    /// Reduce per-worktree git stats into a delete risk. `stats[i]` is `None`
    /// when worktree `i` could not be inspected (not a git worktree / git
    /// failed) — any such entry forces `unknown`. Returns `None` (delete
    /// silently) only when every worktree is known-clean: no dirty tree, no
    /// untracked files, no commits ahead, nothing unknown.
    pub fn from_stats(stats: &[Option<crate::session::GitStats>]) -> Option<DeleteRisk> {
        let mut risk = DeleteRisk::default();
        for entry in stats {
            match entry {
                None => risk.unknown = true,
                Some(s) => {
                    risk.dirty |= s.dirty;
                    risk.files_changed += s.files_changed;
                    risk.insertions += s.insertions;
                    risk.deletions += s.deletions;
                    risk.untracked += s.untracked;
                    risk.ahead += s.ahead;
                }
            }
        }
        if risk.unknown || risk.dirty || risk.untracked > 0 || risk.ahead > 0 {
            Some(risk)
        } else {
            None
        }
    }
}

/// Confirmation prompt for a destructive (hard) session delete, shown only when
/// the `soft_delete` feature flag is off **and** the session has work at risk
/// (see [`DeleteRisk`]). Carries the target session so the confirm handler can
/// tear it down without re-resolving the active index.
#[derive(Debug, Clone)]
pub struct ConfirmDeleteModal {
    pub session_id: crate::session::SessionId,
    pub session_name: String,
    pub risk: DeleteRisk,
}

/// Confirmation prompt for a **best-effort** restore of a force-deleted session.
/// Force-delete tore down the worktree directory + tmux window (and any
/// uncommitted work), but the git branch — and its committed history — survives,
/// so restore can reattach it. This modal warns that only committed state is
/// recovered, then restores the carried session on confirm.
#[derive(Debug, Clone)]
pub struct ConfirmRestoreModal {
    pub deleted: DeletedSessionInfo,
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
    /// Run a shell command headlessly (no session/agent).
    Exec,
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
    /// Exec action: shell command text.
    Command,
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
    /// Exec action: the shell command to run.
    pub command: TextInput,
    /// Multi-line agent prompt (Send/Spawn). A `TextArea` so it accepts newlines
    /// and renders wrapped, mirroring the task editor's description field.
    pub prompt: TextArea,
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
            command: TextInput::default(),
            prompt: TextArea::default(),
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
            AutomationActionKind::Exec => fields.push(Command),
        }
        // Exec has no prompt (it runs a command, not an agent turn).
        if self.action != AutomationActionKind::Exec {
            fields.push(Prompt);
        }
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
            Command => &mut self.command,
            // Prompt is a multi-line `TextArea`, handled explicitly in
            // `handle_key`, not as a single-line `TextInput`.
            Prompt | Trigger | Weekday | Hour | Minute | Action | Target => return None,
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

    /// Feed a key to the editor, mutating field state. Returns whether the caller
    /// should save (`Ctrl+S`, or `Enter` on any non-prompt field), cancel
    /// (`Esc`), or keep editing. On the multi-line `Prompt` field `Enter` inserts
    /// a newline instead of saving. Shared by the centered overlay (Ctrl+P) and
    /// the in-pane editor so both behave identically.
    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> EditorOutcome {
        // Ctrl+S is the universal save: the multi-line Prompt field needs a save
        // path that isn't Enter (Enter inserts a newline there).
        if mods.contains(KeyModifiers::CONTROL)
            && matches!(code, KeyCode::Char('s') | KeyCode::Char('S'))
        {
            return EditorOutcome::Save;
        }
        // Ctrl+E toggles enabled from any field. Lifted above the Prompt branch
        // (which swallows every Ctrl chord) so it still works while editing it.
        if mods.contains(KeyModifiers::CONTROL)
            && matches!(code, KeyCode::Char('e') | KeyCode::Char('E'))
        {
            self.enabled = !self.enabled;
            return EditorOutcome::Continue;
        }

        // The Prompt is a multi-line `TextArea`: Enter inserts a newline, Up/Down
        // move within the text, and Ctrl+letter chords edit (or are swallowed)
        // rather than leaking literal letters. Mirrors the task editor's
        // description field.
        if self.field == AutomationField::Prompt {
            if apply_ctrl_line_edit(&mut self.prompt, code, mods) {
                return EditorOutcome::Continue;
            }
            match code {
                KeyCode::Esc => return EditorOutcome::Cancel,
                KeyCode::Tab => self.next_field(),
                KeyCode::BackTab => self.prev_field(),
                _ => {
                    handle_textarea_key(&mut self.prompt, code);
                }
            }
            return EditorOutcome::Continue;
        }

        // Selector/stepper fields (trigger, weekday, hour, minute, action,
        // target) are adjusted with ←/→/Space; text fields edit as usual.
        let adjustable = self.is_adjustable();
        match code {
            KeyCode::Esc => return EditorOutcome::Cancel,
            KeyCode::Enter => return EditorOutcome::Save,
            KeyCode::Tab | KeyCode::Down => self.next_field(),
            KeyCode::BackTab | KeyCode::Up => self.prev_field(),
            KeyCode::Left if adjustable => self.adjust(-1),
            KeyCode::Right | KeyCode::Char(' ') if adjustable => self.adjust(1),
            other => {
                apply_text_input_key(self.active_field_mut(), other, mods);
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

    /// Cycle through Send → Spawn → Exec actions.
    pub fn toggle_action(&mut self) {
        self.action = match self.action {
            AutomationActionKind::Send => AutomationActionKind::Spawn,
            AutomationActionKind::Spawn => AutomationActionKind::Exec,
            AutomationActionKind::Exec => AutomationActionKind::Send,
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
            AutomationAction::Exec { command } => {
                m.action = AutomationActionKind::Exec;
                m.command.set(command);
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

/// Render a cron expression as a short, human-readable schedule
/// (e.g. `daily 09:00`, `hourly :05`, `weekdays 09:00`, `Mondays 09:00`).
/// Returns `None` for expressions that don't match a known preset shape, so the
/// caller can fall back to the raw cron string for power-user expressions.
pub(crate) fn humanize_cron(expr: &str) -> Option<String> {
    let (kind, hour, minute, dow) = recognize_cron(expr)?;
    let hhmm = format!("{hour:02}:{minute:02}");
    Some(match kind {
        TriggerKind::Hourly => format!("hourly :{minute:02}"),
        TriggerKind::Daily => format!("daily {hhmm}"),
        TriggerKind::Weekdays => format!("weekdays {hhmm}"),
        TriggerKind::Weekly => format!("{} {hhmm}", weekday_plural(dow)),
        // `recognize_cron` never yields Once/Cron, but stay total.
        TriggerKind::Once | TriggerKind::Cron => return None,
    })
}

/// Pluralised weekday name for a Unix cron day-of-week number (0 or 7 = Sunday).
fn weekday_plural(dow: u32) -> &'static str {
    match dow % 7 {
        0 => "Sundays",
        1 => "Mondays",
        2 => "Tuesdays",
        3 => "Wednesdays",
        4 => "Thursdays",
        5 => "Fridays",
        6 => "Saturdays",
        _ => "weekly",
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
    /// Bookmarked repos shown in the list. For a header row this is the parent
    /// folder; for a child/standalone row it is the repo path.
    pub bookmarks: Vec<PathBuf>,
    /// Which bookmarks are selected (checked). Header rows stay unselected.
    pub selected: Vec<bool>,
    /// Whether each selected repo should use worktree mode (parallel to `bookmarks`).
    pub worktree: Vec<bool>,
    /// Whether each row is a parent header (non-selectable group title).
    pub is_header: Vec<bool>,
    /// Whether each row is a child repo nested under a parent header (drives the
    /// indentation). `false` for headers and standalone repos. Parallel to `bookmarks`.
    pub is_child: Vec<bool>,
    /// Parent folders whose child tree is currently collapsed (keyed by path).
    /// Survives row rebuilds so collapsing state is kept across re-scans.
    pub collapsed: HashSet<PathBuf>,
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
    /// Push a row, keeping all parallel vectors in lockstep.
    pub fn push_row(&mut self, path: PathBuf, selected: bool, is_header: bool, is_child: bool) {
        self.bookmarks.push(path);
        self.selected.push(selected);
        self.worktree.push(false);
        self.is_header.push(is_header);
        self.is_child.push(is_child);
    }

    /// Whether the row at `idx` is a parent header (bounds-safe).
    pub fn is_header_row(&self, idx: usize) -> bool {
        self.is_header.get(idx).copied().unwrap_or(false)
    }

    /// Whether the row at `idx` is a child repo under a parent (bounds-safe).
    pub fn is_child_row(&self, idx: usize) -> bool {
        self.is_child.get(idx).copied().unwrap_or(false)
    }

    /// Clear the search query and recompute the filter (collapse still applies).
    pub fn clear_search(&mut self) {
        self.search_input.clear();
        self.list_index = 0;
        self.recompute_filter();
    }

    /// Toggle the collapsed state of the parent header at `real_idx` (a no-op on
    /// non-header rows) and recompute the visible rows.
    pub fn toggle_collapsed(&mut self, real_idx: usize) {
        if !self.is_header_row(real_idx) {
            return;
        }
        let path = self.bookmarks[real_idx].clone();
        if !self.collapsed.insert(path.clone()) {
            self.collapsed.remove(&path);
        }
        self.recompute_filter();
    }

    /// Rebuild `filtered_indices` from the current search query and collapse
    /// state. Header rows are always visible; a child is hidden when its parent
    /// is collapsed (unless a search is active, which expands all so matches are
    /// findable). Keeps `list_index` in range.
    pub fn recompute_filter(&mut self) {
        let query = self.search_input.value().to_string();
        let searching = !query.is_empty();
        let matches = |path: &PathBuf| {
            !searching || crate::fuzzy::fuzzy_match(&query, &path.display().to_string()).is_some()
        };

        let mut indices = Vec::new();
        let mut current_collapsed = false;
        for (i, path) in self.bookmarks.iter().enumerate() {
            let visible = if self.is_header[i] {
                current_collapsed = self.collapsed.contains(path);
                true
            } else if self.is_child[i] {
                let hidden = current_collapsed && !searching;
                !hidden && matches(path)
            } else {
                matches(path)
            };
            if visible {
                indices.push(i);
            }
        }
        self.filtered_indices = indices;
        if self.list_index >= self.filtered_indices.len() {
            self.list_index = self.filtered_indices.len().saturating_sub(1);
        }
    }
}

// ── TaskEditorModal ─────────────────────────────────────────────────────

/// Focusable field in the task editor. A task is just title + description +
/// status; the agent action is chosen at trigger time (`r`), not authored here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskField {
    #[default]
    Title,
    /// Multi-line markdown description (Enter inserts a newline; Ctrl+S saves).
    Description,
    /// Status selector (cycled with ←/→).
    Status,
}

/// The editor fields, in navigation order. Constant — no longer action-driven.
const TASK_FIELDS: [TaskField; 3] = [TaskField::Title, TaskField::Description, TaskField::Status];

/// Editor form for creating or editing a task.
#[derive(Debug, Clone)]
pub struct TaskEditorModal {
    /// `Some` when editing an existing task.
    pub editing_id: Option<i64>,
    pub title: TextInput,
    /// Multi-line markdown description.
    pub description: TextArea,
    pub status: crate::session::TaskStatus,
    pub field: TaskField,
}

impl TaskEditorModal {
    /// A blank editor for a new task.
    pub fn new() -> Self {
        Self {
            editing_id: None,
            title: TextInput::default(),
            description: TextArea::default(),
            status: crate::session::TaskStatus::Todo,
            field: TaskField::default(),
        }
    }

    /// Build an editor pre-filled from an existing task.
    pub fn from_task(task: &crate::session::Task) -> Self {
        let mut m = Self::new();
        m.editing_id = Some(task.id);
        m.title.set(&task.title);
        if let Some(d) = &task.description {
            m.description.set(d);
        }
        m.status = task.status;
        m
    }

    /// The fields shown, in navigation order.
    pub fn visible_fields(&self) -> Vec<TaskField> {
        TASK_FIELDS.to_vec()
    }

    /// Move focus to the next visible field (wraps).
    pub fn next_field(&mut self) {
        self.field = cycle_field(&TASK_FIELDS, self.field, 1);
    }

    /// Move focus to the previous visible field (wraps).
    pub fn prev_field(&mut self) {
        self.field = cycle_field(&TASK_FIELDS, self.field, -1);
    }

    /// The focused text field, or `None` for the description / status selector.
    pub fn active_field(&self) -> Option<&TextInput> {
        match self.field {
            TaskField::Title => Some(&self.title),
            // Description is a `TextArea` (handled explicitly); Status is a
            // selector adjusted with ←/→.
            TaskField::Description | TaskField::Status => None,
        }
    }

    /// The focused text field, or `None` for the description / status selector.
    pub fn active_field_mut(&mut self) -> Option<&mut TextInput> {
        match self.field {
            TaskField::Title => Some(&mut self.title),
            TaskField::Description | TaskField::Status => None,
        }
    }

    /// Whether the focused field is a selector adjusted with ←/→/Space.
    pub fn is_adjustable(&self) -> bool {
        matches!(self.field, TaskField::Status)
    }

    /// Adjust the focused selector by `delta` (−1 for ←, +1 for →/Space).
    pub fn adjust(&mut self, delta: i32) {
        if self.field == TaskField::Status {
            // Cycle in either direction (cycle() only goes forward, so step
            // backward via two forward cycles).
            self.status = if delta < 0 {
                self.status.cycle().cycle()
            } else {
                self.status.cycle()
            };
        }
    }

    /// Feed a key to the editor. Returns whether the caller should save, cancel
    /// (`Esc`), or keep editing.
    ///
    /// `Ctrl+S` saves from any field. On most fields `Enter` saves; on the
    /// multi-line `Description` field `Enter` inserts a newline and `Up`/`Down`
    /// move within the text (field navigation there is `Tab`/`BackTab` only).
    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> EditorOutcome {
        // Ctrl+S is the universal save (the description field needs a save path
        // that isn't Enter).
        if mods.contains(KeyModifiers::CONTROL)
            && matches!(code, KeyCode::Char('s') | KeyCode::Char('S'))
        {
            return EditorOutcome::Save;
        }

        if self.field == TaskField::Description {
            // Readline Ctrl+W / Ctrl+U editing (and swallow other Ctrl chords so
            // they never insert a literal letter).
            if apply_ctrl_line_edit(&mut self.description, code, mods) {
                return EditorOutcome::Continue;
            }
            match code {
                KeyCode::Esc => return EditorOutcome::Cancel,
                KeyCode::Tab => self.next_field(),
                KeyCode::BackTab => self.prev_field(),
                _ => {
                    handle_textarea_key(&mut self.description, code);
                }
            }
            return EditorOutcome::Continue;
        }

        let adjustable = self.is_adjustable();
        match code {
            KeyCode::Esc => return EditorOutcome::Cancel,
            KeyCode::Enter => return EditorOutcome::Save,
            KeyCode::Tab | KeyCode::Down => self.next_field(),
            KeyCode::BackTab | KeyCode::Up => self.prev_field(),
            KeyCode::Left if adjustable => self.adjust(-1),
            KeyCode::Right | KeyCode::Char(' ') if adjustable => self.adjust(1),
            other => {
                apply_text_input_key(self.active_field_mut(), other, mods);
            }
        }
        EditorOutcome::Continue
    }
}

impl Default for TaskEditorModal {
    fn default() -> Self {
        Self::new()
    }
}

// ── Help / Keybinding Editor Modal ──────────────────────────────────────────

/// State for the interactive F1 keybinding editor.
///
/// `selected` indexes into [`Action::rebindable_in_order`], which matches the
/// row order rendered by `render_help_overlay`. When `capturing` is set, the
/// next keypress is captured as the new chord for the selected action rather
/// than being interpreted normally.
#[derive(Debug, Clone, Default)]
pub struct HelpModal {
    pub selected: usize,
    pub capturing: bool,
}

// ── Main Modal Enum ────────────────────────────────────────────────────────

/// One editable row in the [`SettingsModal`]. Section headers are render-only,
/// so this enum lists only the focusable fields, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsField {
    // ── [features] ──────────────────────────────────────────────────────
    FeatTasks,
    FeatAutomations,
    FeatFileViewer,
    FeatGlobalSearch,
    FeatInfoPanel,
    FeatShellPane,
    FeatMouse,
    FeatNotifications,
    FeatSoftDelete,
    FeatVersionCheck,
    FeatAutoUpdate,
    // ── [notifications] ─────────────────────────────────────────────────
    NotifAlsoOnWaiting,
    NotifSuppressForActive,
    NotifSound,
    NotifMinInterval,
    // ── top-level scalars ───────────────────────────────────────────────
    ScrollbackLines,
    TwoPanelMinCols,
    ThreePanelMinCols,
    AuditRetentionDays,
}

impl SettingsField {
    /// Field nav order — also the render order (headers are interleaved by the
    /// renderer). Used by [`cycle_field`] and the scroll-windowing logic.
    pub const ORDER: [SettingsField; 19] = [
        SettingsField::FeatTasks,
        SettingsField::FeatAutomations,
        SettingsField::FeatFileViewer,
        SettingsField::FeatGlobalSearch,
        SettingsField::FeatInfoPanel,
        SettingsField::FeatShellPane,
        SettingsField::FeatMouse,
        SettingsField::FeatNotifications,
        SettingsField::FeatSoftDelete,
        SettingsField::FeatVersionCheck,
        SettingsField::FeatAutoUpdate,
        SettingsField::NotifAlsoOnWaiting,
        SettingsField::NotifSuppressForActive,
        SettingsField::NotifSound,
        SettingsField::NotifMinInterval,
        SettingsField::ScrollbackLines,
        SettingsField::TwoPanelMinCols,
        SettingsField::ThreePanelMinCols,
        SettingsField::AuditRetentionDays,
    ];

    /// The field's `settings.toml` key and one-line help, in a single table so
    /// the two parallel lookups never drift (and stay one match, not two).
    /// The help avoids naming key chords — those are user-configurable, so a
    /// hard-coded hint would drift from the actual binding.
    fn meta(self) -> (&'static str, &'static str) {
        use SettingsField::*;
        match self {
            FeatTasks => ("tasks", "Tasks panel and task search"),
            FeatAutomations => ("automations", "Automations pane and schedule firing"),
            FeatFileViewer => ("file_viewer", "File viewer column"),
            FeatGlobalSearch => ("global_search", "Global search strip"),
            FeatInfoPanel => ("info_panel", "Info panel column"),
            FeatShellPane => ("shell_pane", "Per-session shell pane"),
            FeatMouse => ("mouse", "Mouse: clicks, wheel, drag-select, hover"),
            FeatNotifications => ("notifications", "OS desktop notifications on attention"),
            FeatSoftDelete => ("soft_delete", "Soft-delete sessions with an undo window"),
            FeatVersionCheck => ("version_check", "Check GitHub for updates (network call)"),
            FeatAutoUpdate => (
                "auto_update",
                "Silently self-update on launch (network call)",
            ),
            NotifAlsoOnWaiting => (
                "also_on_waiting",
                "Also notify when a session finishes (Done)",
            ),
            NotifSuppressForActive => (
                "suppress_for_active",
                "Skip the session you're already viewing",
            ),
            NotifSound => ("sound", "Play the OS notification sound"),
            NotifMinInterval => (
                "min_interval_secs",
                "Min seconds between notifications per session",
            ),
            ScrollbackLines => (
                "scrollback_lines",
                "Terminal history lines kept per session",
            ),
            TwoPanelMinCols => (
                "two_panel_min_cols",
                "Min width (cols) to show the 2nd panel",
            ),
            ThreePanelMinCols => (
                "three_panel_min_cols",
                "Min width (cols) to show the 3rd panel",
            ),
            AuditRetentionDays => ("audit_retention_days", "Days of audit-log history kept"),
        }
    }

    /// The field's `settings.toml` key (e.g. `tasks`).
    pub fn label(self) -> &'static str {
        self.meta().0
    }

    /// One-line help shown for the selected field in the panel footer.
    pub fn description(self) -> &'static str {
        self.meta().1
    }

    /// Whether the field holds a boolean (toggled with Space/Enter) vs. a
    /// numeric scalar (stepped with ←/→).
    pub fn is_scalar(self) -> bool {
        use SettingsField::*;
        matches!(
            self,
            NotifMinInterval
                | ScrollbackLines
                | TwoPanelMinCols
                | ThreePanelMinCols
                | AuditRetentionDays
        )
    }

    /// Whether changing this field takes effect only after a restart. Feature
    /// flags that gate UI panels (read from `App.features` every frame) apply
    /// live; everything else is read once at startup from `settings::global()`,
    /// which is a write-once value that can't be re-applied in-process.
    ///
    /// Derived from the canonical
    /// [`crate::session::settings::Settings::restart_only_differs`] rather than
    /// a second hand-maintained list: flip just this field on a default draft
    /// and ask whether that single change registers as restart-only. So the UI
    /// `⟳` marker can never disagree with the toast/reload partition.
    pub fn restart_required(self) -> bool {
        let mut modal = SettingsModal::new(crate::session::settings::Settings::default());
        modal.field = self;
        if self.is_scalar() {
            modal.adjust(1);
        } else {
            modal.toggle();
        }
        modal.restart_required_changed()
    }
}

/// Settings panel: a centered modal that edits a working copy of [`Settings`]
/// (`draft`) and writes it back to `settings.toml` on save. Feature flags that
/// gate UI panels apply live; the rest take effect after a restart. No live
/// preview — edits apply only on save, so `Esc` is a clean discard.
#[derive(Debug, Clone)]
pub struct SettingsModal {
    pub draft: crate::session::settings::Settings,
    pub original: crate::session::settings::Settings,
    pub field: SettingsField,
}

impl SettingsModal {
    pub fn new(draft: crate::session::settings::Settings) -> Self {
        Self {
            original: draft.clone(),
            draft,
            field: SettingsField::FeatTasks,
        }
    }

    pub fn next_field(&mut self) {
        self.field = cycle_field(&SettingsField::ORDER, self.field, 1);
    }

    pub fn prev_field(&mut self) {
        self.field = cycle_field(&SettingsField::ORDER, self.field, -1);
    }

    /// Toggle the boolean the current field maps to (no-op on scalar fields).
    pub fn toggle(&mut self) {
        use SettingsField::*;
        let f = &mut self.draft.features;
        let n = &mut self.draft.notifications;
        match self.field {
            FeatTasks => f.tasks = !f.tasks,
            FeatAutomations => f.automations = !f.automations,
            FeatFileViewer => f.file_viewer = !f.file_viewer,
            FeatGlobalSearch => f.global_search = !f.global_search,
            FeatInfoPanel => f.info_panel = !f.info_panel,
            FeatShellPane => f.shell_pane = !f.shell_pane,
            FeatMouse => f.mouse = !f.mouse,
            FeatNotifications => f.notifications = !f.notifications,
            FeatSoftDelete => f.soft_delete = !f.soft_delete,
            FeatVersionCheck => f.version_check = !f.version_check,
            FeatAutoUpdate => f.auto_update = !f.auto_update,
            NotifAlsoOnWaiting => n.also_on_waiting = !n.also_on_waiting,
            NotifSuppressForActive => n.suppress_for_active = !n.suppress_for_active,
            NotifSound => n.sound = !n.sound,
            NotifMinInterval | ScrollbackLines | TwoPanelMinCols | ThreePanelMinCols
            | AuditRetentionDays => {}
        }
    }

    /// Step a scalar field by `delta` (±1), with field-specific step sizes and
    /// clamping (no-op on boolean fields).
    pub fn adjust(&mut self, delta: i32) {
        use SettingsField::*;
        let d = &mut self.draft;
        match self.field {
            ScrollbackLines => {
                d.scrollback_lines =
                    step_clamp(d.scrollback_lines as i64, delta, 500, 100, 200_000) as usize;
            }
            TwoPanelMinCols => {
                d.two_panel_min_cols =
                    step_clamp(i64::from(d.two_panel_min_cols), delta, 5, 40, 400) as u16;
            }
            ThreePanelMinCols => {
                d.three_panel_min_cols =
                    step_clamp(i64::from(d.three_panel_min_cols), delta, 5, 40, 400) as u16;
            }
            AuditRetentionDays => {
                d.audit_retention_days =
                    step_clamp(d.audit_retention_days as i64, delta, 5, 1, 3650) as u64;
            }
            NotifMinInterval => {
                d.notifications.min_interval_secs =
                    step_clamp(d.notifications.min_interval_secs as i64, delta, 5, 0, 3600) as u64;
            }
            _ => {}
        }
    }

    /// String value rendered for the current field.
    pub fn value_string(&self, field: SettingsField) -> String {
        use SettingsField::*;
        let f = &self.draft.features;
        let n = &self.draft.notifications;
        let on = |b: bool| if b { "on" } else { "off" }.to_string();
        match field {
            FeatTasks => on(f.tasks),
            FeatAutomations => on(f.automations),
            FeatFileViewer => on(f.file_viewer),
            FeatGlobalSearch => on(f.global_search),
            FeatInfoPanel => on(f.info_panel),
            FeatShellPane => on(f.shell_pane),
            FeatMouse => on(f.mouse),
            FeatNotifications => on(f.notifications),
            FeatSoftDelete => on(f.soft_delete),
            FeatVersionCheck => on(f.version_check),
            FeatAutoUpdate => on(f.auto_update),
            NotifAlsoOnWaiting => on(n.also_on_waiting),
            NotifSuppressForActive => on(n.suppress_for_active),
            NotifSound => on(n.sound),
            NotifMinInterval => n.min_interval_secs.to_string(),
            ScrollbackLines => self.draft.scrollback_lines.to_string(),
            TwoPanelMinCols => self.draft.two_panel_min_cols.to_string(),
            ThreePanelMinCols => self.draft.three_panel_min_cols.to_string(),
            AuditRetentionDays => self.draft.audit_retention_days.to_string(),
        }
    }

    /// True once any restart-required field differs from the opened state.
    /// Drives the "some changes apply after restart" toast and indicator.
    pub fn restart_required_changed(&self) -> bool {
        self.draft.restart_only_differs(&self.original)
    }

    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> EditorOutcome {
        let scalar = self.field.is_scalar();
        match code {
            KeyCode::Esc => EditorOutcome::Cancel,
            // Explicit save only (Ctrl+S) — keeps Enter free to toggle a bool.
            KeyCode::Char('s') | KeyCode::Char('S') if mods.contains(KeyModifiers::CONTROL) => {
                EditorOutcome::Save
            }
            KeyCode::Tab | KeyCode::Down => {
                self.next_field();
                EditorOutcome::Continue
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.prev_field();
                EditorOutcome::Continue
            }
            KeyCode::Left if scalar => {
                self.adjust(-1);
                EditorOutcome::Continue
            }
            KeyCode::Right if scalar => {
                self.adjust(1);
                EditorOutcome::Continue
            }
            KeyCode::Char(' ') if scalar => {
                self.adjust(1);
                EditorOutcome::Continue
            }
            KeyCode::Char(' ') | KeyCode::Enter => {
                self.toggle();
                EditorOutcome::Continue
            }
            _ => EditorOutcome::Continue,
        }
    }
}

/// Step `current` by `delta * step`, clamped to `[min, max]`.
fn step_clamp(current: i64, delta: i32, step: i64, min: i64, max: i64) -> i64 {
    (current + i64::from(delta) * step).clamp(min, max)
}

/// Single, discriminated union replacing boolean flags for modal state.
/// Only one modal can be active at a time, making invalid states unrepresentable.
#[derive(Debug, Clone, Default)]
pub enum Modal {
    #[default]
    None,
    Help(HelpModal),
    BranchSelector(BranchSelectorModal),
    WorktreeName(WorktreeNameModal),
    AgentPicker(crate::ui::agent_picker_modal::AgentPickerState),
    HostPicker(crate::ui::host_picker_modal::HostPickerState),
    RestoreSessions(RestoreSessionsModal),
    AutomationEditor(AutomationEditorModal),
    AutomationsList(AutomationsListModal),
    RepoPicker(RepoPickerModal),
    SessionName(SessionNameModal),
    ThemePicker(ThemePickerModal),
    TaskActionPicker(TaskActionPickerModal),
    ConfirmDelete(ConfirmDeleteModal),
    ConfirmRestore(ConfirmRestoreModal),
    Settings(SettingsModal),
}

impl Modal {
    pub fn close(&mut self) {
        *self = Modal::None;
    }

    pub fn is_open(&self) -> bool {
        !matches!(self, Modal::None)
    }

    /// For a modal with a selectable list, a mutable handle to its selection
    /// cursor plus the key a row-click replays to activate that row (`Enter` for
    /// the selectors and the F1 editor, `Space` for the repo picker — where a
    /// row's action is toggle/fold and `Enter` would confirm the whole modal on
    /// a misclick). This is the **single** match over the selector modals, so the
    /// read path (`App::modal_selected_index`) and the write path
    /// (`App::select_modal_row`) can never drift onto different modal sets — a new
    /// selectable modal is wired into both at once by adding one arm here.
    pub(super) fn list_selection(&mut self) -> Option<(&mut usize, KeyCode)> {
        match self {
            Modal::Help(h) => Some((&mut h.selected, KeyCode::Enter)),
            Modal::ThemePicker(tp) => Some((&mut tp.index, KeyCode::Enter)),
            Modal::AgentPicker(ap) => Some((&mut ap.selected_index, KeyCode::Enter)),
            Modal::HostPicker(hp) => Some((&mut hp.selected_index, KeyCode::Enter)),
            Modal::BranchSelector(bs) => Some((&mut bs.index, KeyCode::Enter)),
            Modal::TaskActionPicker(p) => Some((&mut p.selected, KeyCode::Enter)),
            Modal::AutomationsList(al) => Some((&mut al.index, KeyCode::Enter)),
            Modal::RestoreSessions(rs) => Some((&mut rs.index, KeyCode::Enter)),
            Modal::RepoPicker(rp) => Some((&mut rp.list_index, KeyCode::Char(' '))),
            _ => None,
        }
    }
}

// ── TaskActionPickerModal ────────────────────────────────────────────────

/// A trigger-time action for a task, chosen from the picker. Nothing is stored
/// on the task — the choice runs immediately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskActionChoice {
    /// Send the task title to an already-running session (id + display name).
    Send(crate::session::SessionId, String),
    /// Spawn a new session via the normal repo→agent flow, seeded with the title.
    SpawnNew,
}

impl TaskActionChoice {
    /// One-line label for the picker list.
    pub fn label(&self) -> String {
        match self {
            TaskActionChoice::Send(_, name) => format!("Send → {name}"),
            TaskActionChoice::SpawnNew => "Spawn new session…".to_string(),
        }
    }
}

/// Picker shown when triggering a task (`r`): pick where to run it.
#[derive(Debug, Clone)]
pub struct TaskActionPickerModal {
    pub task_id: i64,
    pub title: String,
    pub choices: Vec<TaskActionChoice>,
    pub selected: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::GitStats;

    fn stats(dirty: bool, files: usize, ins: usize, dels: usize, ahead: usize) -> GitStats {
        GitStats {
            files_changed: files,
            insertions: ins,
            deletions: dels,
            untracked: 0,
            dirty,
            ahead,
            behind: 0,
        }
    }

    #[test]
    fn delete_risk_clean_known_is_none() {
        // A single known-clean worktree → delete silently.
        assert_eq!(
            DeleteRisk::from_stats(&[Some(stats(false, 0, 0, 0, 0))]),
            None
        );
        // No worktrees at all is also "nothing at risk".
        assert_eq!(DeleteRisk::from_stats(&[]), None);
    }

    #[test]
    fn delete_risk_dirty_triggers() {
        let risk = DeleteRisk::from_stats(&[Some(stats(true, 2, 5, 1, 0))]).unwrap();
        assert!(risk.dirty);
        assert_eq!(risk.files_changed, 2);
        assert!(!risk.unknown);
    }

    #[test]
    fn delete_risk_ahead_triggers_even_when_clean() {
        let risk = DeleteRisk::from_stats(&[Some(stats(false, 0, 0, 0, 3))]).unwrap();
        assert_eq!(risk.ahead, 3);
        assert!(!risk.dirty);
    }

    #[test]
    fn delete_risk_untracked_only_triggers() {
        // Untracked files don't show in `diff HEAD` (files_changed == 0), so
        // exercise the untracked count as the sole trigger (dirty == false).
        let mut s = stats(false, 0, 0, 0, 0);
        s.untracked = 2;
        let risk = DeleteRisk::from_stats(&[Some(s)]).unwrap();
        assert_eq!(risk.untracked, 2);
        assert_eq!(risk.files_changed, 0);
        assert!(!risk.dirty, "untracked alone triggers even without dirty");
    }

    #[test]
    fn delete_risk_none_entry_forces_unknown() {
        // An uninspectable worktree (None) is treated as "can't prove clean".
        let risk = DeleteRisk::from_stats(&[None]).unwrap();
        assert!(risk.unknown);
        assert_eq!(DeleteRisk::unknown(), risk);
    }

    #[test]
    fn delete_risk_accumulates_across_worktrees() {
        let risk = DeleteRisk::from_stats(&[
            Some(stats(true, 1, 10, 2, 1)),
            Some(stats(false, 2, 30, 5, 4)),
        ])
        .unwrap();
        assert!(risk.dirty);
        assert_eq!(risk.files_changed, 3);
        assert_eq!(risk.insertions, 40);
        assert_eq!(risk.deletions, 7);
        assert_eq!(risk.ahead, 5);
        assert!(!risk.unknown);
    }

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
        let none = KeyModifiers::NONE;
        let mut input = TextInput::new();
        input.set("ab");
        assert!(apply_text_input_key(
            Some(&mut input),
            KeyCode::Char('c'),
            none
        ));
        assert_eq!(input.value(), "abc");
        assert!(apply_text_input_key(
            Some(&mut input),
            KeyCode::Backspace,
            none
        ));
        assert_eq!(input.value(), "ab");
        // Non-text keys are not handled.
        assert!(!apply_text_input_key(
            Some(&mut input),
            KeyCode::Enter,
            none
        ));
        assert!(!apply_text_input_key(Some(&mut input), KeyCode::Tab, none));
        // A text key with no focused field is still "handled" (a no-op).
        assert!(apply_text_input_key(None, KeyCode::Char('x'), none));
    }

    #[test]
    fn ctrl_chords_edit_text_and_never_insert_the_letter() {
        let ctrl = KeyModifiers::CONTROL;
        let mut input = TextInput::new();
        input.set("hello world");

        // Ctrl+W deletes the word before the cursor (and is reported handled).
        assert!(apply_text_input_key(
            Some(&mut input),
            KeyCode::Char('w'),
            ctrl
        ));
        assert_eq!(input.value(), "hello ");

        // Ctrl+U kills to the start of the line.
        input.set("hello world");
        apply_text_input_key(Some(&mut input), KeyCode::Char('u'), ctrl);
        assert_eq!(input.value(), "");

        // Any other Ctrl+letter is swallowed — it must never insert the letter.
        input.set("ab");
        assert!(apply_text_input_key(
            Some(&mut input),
            KeyCode::Char('x'),
            ctrl
        ));
        assert_eq!(input.value(), "ab");

        // A Ctrl+digit is likewise swallowed, never typed as text.
        assert!(apply_text_input_key(
            Some(&mut input),
            KeyCode::Char('1'),
            ctrl
        ));
        assert_eq!(input.value(), "ab");
    }

    #[test]
    fn ctrl_non_letter_chords_fall_through_to_cursor_moves() {
        // Ctrl+<non-letter> (arrows, Home/End) is not a line-edit chord: it
        // must behave like the unmodified key so every text field is
        // consistent (a single-line modal moves the cursor just like the
        // repo-search field does).
        let ctrl = KeyModifiers::CONTROL;
        let mut input = TextInput::new();
        input.set("abc"); // cursor parked at the end

        assert!(apply_text_input_key(Some(&mut input), KeyCode::Left, ctrl));
        assert_eq!(input.cursor_pos(), 2);
        assert_eq!(input.value(), "abc", "Ctrl+Left must not delete text");

        assert!(apply_text_input_key(Some(&mut input), KeyCode::Home, ctrl));
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn text_input_word_and_line_deletes() {
        let mut input = TextInput::new();
        input.set("foo bar baz");
        input.delete_word_before();
        assert_eq!(input.value(), "foo bar ");
        input.delete_word_before();
        assert_eq!(input.value(), "foo ");
        input.set("trailing   ");
        input.delete_word_before(); // skips trailing spaces, then the word
        assert_eq!(input.value(), "");

        input.set("keep this");
        input.move_left(); // cursor before the final 's'
        input.kill_to_line_start();
        assert_eq!(input.value(), "s");
    }

    #[test]
    fn text_area_word_and_line_deletes_respect_lines() {
        let mut area = TextArea::new();
        area.set("first\nsecond word");
        // Ctrl+U deletes only to the start of the current (second) line.
        area.kill_to_line_start();
        assert_eq!(area.value(), "first\n");
        // Ctrl+W at the start of a line treats the newline as whitespace and
        // deletes back through it plus the previous word (readline behavior).
        area.set("first\nsecond");
        area.home(); // start of "second"
        area.delete_word_before();
        assert_eq!(area.value(), "second");
    }

    #[test]
    fn ctrl_chords_move_cursor_like_a_terminal() {
        // Ctrl+A/E to line ends, Ctrl+B/F by one char — emacs/readline moves
        // that must edit the cursor without ever inserting the bare letter.
        let ctrl = KeyModifiers::CONTROL;
        let mut input = TextInput::new();
        input.set("abcd"); // cursor at end (4)

        assert!(apply_text_input_key(
            Some(&mut input),
            KeyCode::Char('a'),
            ctrl
        ));
        assert_eq!(input.cursor_pos(), 0);
        assert!(apply_text_input_key(
            Some(&mut input),
            KeyCode::Char('e'),
            ctrl
        ));
        assert_eq!(input.cursor_pos(), 4);
        assert!(apply_text_input_key(
            Some(&mut input),
            KeyCode::Char('b'),
            ctrl
        ));
        assert_eq!(input.cursor_pos(), 3);
        assert!(apply_text_input_key(
            Some(&mut input),
            KeyCode::Char('f'),
            ctrl
        ));
        assert_eq!(input.cursor_pos(), 4);
        // Uppercase (Shift+Ctrl) maps the same way.
        assert!(apply_text_input_key(
            Some(&mut input),
            KeyCode::Char('A'),
            ctrl
        ));
        assert_eq!(input.cursor_pos(), 0);
        assert_eq!(input.value(), "abcd", "cursor chords must not change text");
    }

    #[test]
    fn ctrl_chords_delete_chars_and_kill_to_line_end() {
        let ctrl = KeyModifiers::CONTROL;
        let mut input = TextInput::new();

        // Ctrl+H deletes the char before the cursor (like Backspace).
        input.set("abc"); // cursor at end
        assert!(apply_text_input_key(
            Some(&mut input),
            KeyCode::Char('h'),
            ctrl
        ));
        assert_eq!(input.value(), "ab");

        // Ctrl+D deletes the char under the cursor.
        input.set("abc");
        input.home(); // cursor before 'a'
        assert!(apply_text_input_key(
            Some(&mut input),
            KeyCode::Char('d'),
            ctrl
        ));
        assert_eq!(input.value(), "bc");

        // Ctrl+K kills from the cursor to the end of the line.
        input.set("hello world");
        input.home();
        input.move_right(); // after 'h'
        input.kill_to_line_end();
        assert_eq!(input.value(), "h");
    }

    #[test]
    fn text_area_kill_to_line_end_respects_lines() {
        let mut area = TextArea::new();
        // Ctrl+K mid-line kills only the rest of the current line.
        area.set("first line\nsecond");
        area.home(); // start of line 1 ("second")
        area.move_up(); // start of line 0 ("first line")
        for _ in 0..5 {
            area.move_right(); // park after "first"
        }
        area.kill_to_line_end();
        assert_eq!(area.value(), "first\nsecond");

        // At the end of a line (text below), Ctrl+K joins the next line.
        area.set("first\nsecond");
        area.home(); // start of line 1 ("second")
        area.move_up(); // line 0
        area.end(); // end of "first", before the newline
        area.kill_to_line_end();
        assert_eq!(area.value(), "firstsecond");
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
        let modal = Modal::Help(HelpModal::default());
        assert!(!matches!(modal, Modal::None));
    }

    #[test]
    fn test_modal_close() {
        let mut modal = Modal::Help(HelpModal::default());
        assert!(!matches!(modal, Modal::None));
        modal.close();
        assert!(matches!(modal, Modal::None));
    }

    #[test]
    fn test_text_input_with_unicode() {
        let mut input = TextInput::new();
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

        modal = Modal::Help(HelpModal::default());
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
        // The action cycles Send → Spawn → Exec → Send.
        modal.toggle_action();
        assert_eq!(modal.action, AutomationActionKind::Exec);
        assert!(modal.visible_fields().contains(&AutomationField::Command));
        // Exec has no prompt; it runs a command.
        assert!(!modal.visible_fields().contains(&AutomationField::Prompt));
        modal.toggle_action();
        assert_eq!(modal.action, AutomationActionKind::Send);
        assert!(!modal.visible_fields().contains(&AutomationField::Repo));
    }

    #[test]
    fn visible_fields_track_trigger_kind() {
        use AutomationField::*;
        let fields = |trigger| {
            AutomationEditorModal {
                trigger_kind: trigger,
                action: AutomationActionKind::Send,
                ..Default::default()
            }
            .visible_fields()
        };
        // A one-shot delay never shows a wall-clock timezone field.
        assert_eq!(
            fields(TriggerKind::Once),
            vec![Name, Trigger, Delay, Action, Target, Prompt]
        );
        assert!(!fields(TriggerKind::Once).contains(&Timezone));
        // Wall-clock schedules carry a timezone and their time steppers.
        assert_eq!(
            fields(TriggerKind::Hourly),
            vec![Name, Trigger, Minute, Timezone, Action, Target, Prompt]
        );
        assert_eq!(
            fields(TriggerKind::Daily),
            vec![Name, Trigger, Hour, Minute, Timezone, Action, Target, Prompt]
        );
        assert_eq!(
            fields(TriggerKind::Weekdays),
            vec![Name, Trigger, Hour, Minute, Timezone, Action, Target, Prompt]
        );
        assert_eq!(
            fields(TriggerKind::Weekly),
            vec![Name, Trigger, Weekday, Hour, Minute, Timezone, Action, Target, Prompt]
        );
        assert_eq!(
            fields(TriggerKind::Cron),
            vec![Name, Trigger, CronExpr, Timezone, Action, Target, Prompt]
        );
    }

    #[test]
    fn test_automations_list_modal_default() {
        let modal = AutomationsListModal::default();
        assert_eq!(modal.index, 0);
        assert!(modal.entries.is_empty());
    }

    #[test]
    fn test_repo_picker_clear_search_resets_filter() {
        let mut rp = RepoPickerModal::default();
        rp.push_row("/a".into(), false, false, false);
        rp.push_row("/b".into(), true, false, false);
        rp.push_row("/c".into(), false, false, false);
        rp.list_index = 1;
        rp.filtered_indices = vec![1]; // simulating an active filter
        rp.search_input.set("b");

        rp.clear_search();

        assert_eq!(rp.search_input.value(), "");
        assert_eq!(rp.filtered_indices, vec![0, 1, 2]);
        assert_eq!(rp.list_index, 0);
    }

    #[test]
    fn test_repo_picker_push_row_keeps_vectors_in_lockstep() {
        let mut rp = RepoPickerModal::default();
        rp.push_row("/repo".into(), true, false, false);
        rp.push_row("/parent".into(), false, true, false);
        rp.push_row("/parent/child".into(), false, false, true);

        let n = rp.bookmarks.len();
        assert_eq!(n, 3);
        assert_eq!(rp.selected.len(), n);
        assert_eq!(rp.worktree.len(), n);
        assert_eq!(rp.is_header.len(), n);
        assert_eq!(rp.is_child.len(), n);
        assert_eq!(rp.is_header, vec![false, true, false]);
        assert_eq!(rp.is_child, vec![false, false, true]);
    }

    #[test]
    fn test_repo_picker_toggle_collapsed_ignores_non_header_rows() {
        let mut rp = RepoPickerModal::default();
        rp.push_row("/repo".into(), false, false, false); // standalone, not a header
        rp.toggle_collapsed(0);
        assert!(
            rp.collapsed.is_empty(),
            "non-header rows can't be collapsed"
        );
        // An out-of-range index is also a no-op (no panic).
        rp.toggle_collapsed(99);
        assert!(rp.collapsed.is_empty());
    }

    #[test]
    fn test_repo_picker_search_overrides_collapse() {
        let mut rp = RepoPickerModal::default();
        rp.push_row("/parent".into(), false, true, false); // header
        rp.push_row("/parent/foo".into(), false, false, true);
        rp.push_row("/parent/bar".into(), false, false, true);
        rp.collapsed.insert("/parent".into());
        rp.recompute_filter();
        // Collapsed: only the header is visible.
        assert_eq!(rp.filtered_indices, vec![0]);

        // An active search expands all so matching children are findable.
        rp.search_input.set("foo");
        rp.recompute_filter();
        // Header (always shown) + the matching child `foo`.
        assert_eq!(rp.filtered_indices, vec![0, 1]);
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
                extra_repos: Vec::new(),
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
    fn test_humanize_cron_presets_and_raw() {
        assert_eq!(humanize_cron("5 * * * *").as_deref(), Some("hourly :05"));
        assert_eq!(humanize_cron("0 9 * * *").as_deref(), Some("daily 09:00"));
        assert_eq!(
            humanize_cron("30 17 * * 1-5").as_deref(),
            Some("weekdays 17:30")
        );
        assert_eq!(humanize_cron("0 9 * * 1").as_deref(), Some("Mondays 09:00"));
        assert_eq!(
            humanize_cron("15 8 * * 0").as_deref(),
            Some("Sundays 08:15")
        );
        // Unrecognized shapes return None so the caller keeps the raw expression.
        assert_eq!(humanize_cron("*/5 * * * *"), None);
        assert_eq!(humanize_cron("0 9 1 * *"), None);
    }

    #[test]
    fn test_format_duration_short() {
        assert_eq!(format_duration_short(1_800_000), "30m");
        assert_eq!(format_duration_short(5_400_000), "1h30m");
        assert_eq!(format_duration_short(90_000_000), "1d1h");
        assert_eq!(format_duration_short(0), "0m");
    }

    #[test]
    fn task_editor_visible_fields_are_title_description_status() {
        let m = TaskEditorModal::new();
        assert_eq!(
            m.visible_fields(),
            vec![TaskField::Title, TaskField::Description, TaskField::Status]
        );
    }

    #[test]
    fn task_editor_save_and_cancel_outcomes() {
        let mut m = TaskEditorModal::new();
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
        let mut m = TaskEditorModal::new();
        m.field = TaskField::Status;
        assert_eq!(m.status, crate::session::TaskStatus::Todo);
        m.adjust(1);
        assert_eq!(m.status, crate::session::TaskStatus::InProgress);
        m.adjust(-1);
        assert_eq!(m.status, crate::session::TaskStatus::Todo);
    }

    #[test]
    fn task_editor_active_field_tracks_focus_and_caret() {
        let mut m = TaskEditorModal::new();

        // Title is the default focus: typing moves its caret, and active_field()
        // reports that same field so the renderer can draw the cursor in place.
        m.handle_key(KeyCode::Char('h'), KeyModifiers::NONE);
        m.handle_key(KeyCode::Char('i'), KeyModifiers::NONE);
        assert_eq!(m.active_field().map(|f| f.value()), Some("hi"));
        assert_eq!(m.active_field().map(|f| f.cursor_pos()), Some(2));
        // Moving left mid-text is reflected by the caret (the rendering bug).
        m.handle_key(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(m.active_field().map(|f| f.cursor_pos()), Some(1));

        // Selector fields are not text inputs: active_field() is None.
        m.field = TaskField::Status;
        assert!(m.active_field().is_none());
    }

    #[test]
    fn textarea_inserts_newlines_and_moves_vertically() {
        let mut ta = TextArea::new();
        for c in "ab".chars() {
            ta.insert(c);
        }
        ta.insert_newline();
        for c in "cde".chars() {
            ta.insert(c);
        }
        assert_eq!(ta.value(), "ab\ncde");
        assert_eq!(ta.cursor_line_col(), (1, 3));

        // Up keeps the column, clamped to the shorter first line ("ab" → col 2).
        ta.move_up();
        assert_eq!(ta.cursor_line_col(), (0, 2));
        // Down returns to the second line at the clamped column.
        ta.move_down();
        assert_eq!(ta.cursor_line_col(), (1, 2));
    }

    #[test]
    fn textarea_backspace_joins_lines() {
        let mut ta = TextArea::new();
        ta.set("ab\ncd");
        // Cursor is at the end; home to start of line 2, then backspace joins.
        ta.home();
        assert_eq!(ta.cursor_line_col(), (1, 0));
        ta.backspace();
        assert_eq!(ta.value(), "abcd");
        assert_eq!(ta.cursor_line_col(), (0, 2));
    }

    #[test]
    fn task_editor_description_enter_inserts_newline_no_save() {
        let mut m = TaskEditorModal::new();
        m.field = TaskField::Description;
        m.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(
            m.handle_key(KeyCode::Enter, KeyModifiers::NONE),
            EditorOutcome::Continue
        );
        m.handle_key(KeyCode::Char('b'), KeyModifiers::NONE);
        assert_eq!(m.description.value(), "a\nb");
    }

    #[test]
    fn task_editor_ctrl_s_saves_from_any_field() {
        let mut m = TaskEditorModal::new();
        m.field = TaskField::Description;
        assert_eq!(
            m.handle_key(KeyCode::Char('s'), KeyModifiers::CONTROL),
            EditorOutcome::Save
        );
    }

    #[test]
    fn task_editor_from_task_populates_description() {
        let task = crate::session::Task {
            id: 1,
            title: "t".into(),
            description: Some("notes".into()),
            status: crate::session::TaskStatus::Todo,
            action: None,
            source: crate::session::SOURCE_LOCAL.into(),
            external_id: None,
            external_url: None,
            created_at: 0,
            updated_at: 0,
            deleted_at: None,
        };
        let m = TaskEditorModal::from_task(&task);
        assert_eq!(m.description.value(), "notes");
    }

    // ── TextInput: remaining cursor/edge cases ───────────────────────────

    #[test]
    fn text_input_edits_are_noops_at_buffer_edges() {
        let mut input = TextInput::new();
        input.set("ab");
        input.home();
        input.backspace(); // at start → no-op
        assert_eq!(input.value(), "ab");
        input.end();
        input.delete(); // at end → no-op
        assert_eq!(input.value(), "ab");
    }

    #[test]
    fn text_input_clear_resets_buffer_and_cursor() {
        let mut input = TextInput::new();
        input.set("hello");
        input.clear();
        assert_eq!(input.value(), "");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn text_input_backspace_and_delete_multibyte() {
        let mut input = TextInput::new();
        input.set("añé");
        input.backspace();
        assert_eq!(input.value(), "añ");
        input.home();
        input.move_right();
        input.delete(); // removes the multi-byte 'ñ' at the cursor
        assert_eq!(input.value(), "a");
    }

    #[test]
    fn text_input_insert_str_drops_control_chars_and_advances_cursor() {
        let mut input = TextInput::new();
        input.set("a");
        input.insert_str("bc\nd\te"); // newline + tab are dropped
        assert_eq!(input.value(), "abcde");
        assert_eq!(input.cursor_pos(), 5);
    }

    #[test]
    fn textarea_insert_str_keeps_newlines_drops_carriage_returns() {
        let mut ta = TextArea::new();
        ta.insert_str("one\r\ntwo\tthree");
        // `\r` dropped, `\n` kept, `\t` (control) dropped.
        assert_eq!(ta.value(), "one\ntwothree");
    }

    #[test]
    fn text_input_move_right_clamps_at_end_and_insert_mid_string() {
        let mut input = TextInput::new();
        input.set("ab");
        // At the end already: move_right is a no-op (no overrun past len).
        input.move_right();
        assert_eq!(input.cursor_pos(), 2);
        // Insert in the middle keeps later chars intact.
        input.move_left();
        input.insert('X');
        assert_eq!(input.value(), "aXb");
        assert_eq!(input.cursor_pos(), 2);
    }

    // ── TextArea: methods not exercised by the existing happy-path test ──

    #[test]
    fn textarea_delete_removes_char_at_cursor() {
        let mut ta = TextArea::new();
        ta.set("abc");
        ta.home();
        ta.delete();
        assert_eq!(ta.value(), "bc");
        // Delete at end-of-buffer is a no-op.
        ta.end();
        ta.delete();
        assert_eq!(ta.value(), "bc");
    }

    #[test]
    fn textarea_horizontal_cursor_clamps_at_both_ends() {
        let mut ta = TextArea::new();
        ta.set("ab");
        ta.end();
        ta.move_right(); // already at end → no-op
        assert_eq!(ta.cursor_line_col(), (0, 2));
        ta.home();
        ta.move_left(); // already at start → no-op
        assert_eq!(ta.cursor_line_col(), (0, 0));
    }

    #[test]
    fn textarea_vertical_moves_clamp_at_first_and_last_line() {
        let mut ta = TextArea::new();
        ta.set("a\nbb");
        // On the last line: move_down stays put.
        ta.end();
        assert_eq!(ta.cursor_line_col(), (1, 2));
        ta.move_down();
        assert_eq!(ta.cursor_line_col(), (1, 2));
        // On the first line: move_up stays put.
        ta.set("a\nbb");
        ta.home(); // start of line 2
        ta.move_up(); // → line 1
        assert_eq!(ta.cursor_line_col(), (0, 0));
        ta.move_up(); // already top → no-op
        assert_eq!(ta.cursor_line_col(), (0, 0));
    }

    #[test]
    fn textarea_home_and_end_act_per_line() {
        let mut ta = TextArea::new();
        ta.set("abc\nde");
        // Cursor is at the end of line 2.
        ta.home();
        assert_eq!(ta.cursor_line_col(), (1, 0));
        ta.end();
        assert_eq!(ta.cursor_line_col(), (1, 2));
        // home/end stay within the current line, not the whole buffer.
        ta.move_up();
        ta.home();
        assert_eq!(ta.cursor_line_col(), (0, 0));
        ta.end();
        assert_eq!(ta.cursor_line_col(), (0, 3));
    }

    #[test]
    fn textarea_clear_resets_buffer_and_cursor() {
        let mut ta = TextArea::new();
        ta.set("a\nb");
        ta.clear();
        assert_eq!(ta.value(), "");
        assert_eq!(ta.cursor_line_col(), (0, 0));
    }

    // ── AutomationEditorModal::handle_key (the shared key state machine) ──

    #[test]
    fn automation_editor_handle_key_save_cancel_and_text() {
        let mut m = AutomationEditorModal::default();
        // Default field is Name (a text field): chars edit it, no save/cancel.
        assert_eq!(
            m.handle_key(KeyCode::Char('h'), KeyModifiers::NONE),
            EditorOutcome::Continue
        );
        m.handle_key(KeyCode::Char('i'), KeyModifiers::NONE);
        assert_eq!(m.name.value(), "hi");
        assert_eq!(
            m.handle_key(KeyCode::Enter, KeyModifiers::NONE),
            EditorOutcome::Save
        );
        assert_eq!(
            m.handle_key(KeyCode::Esc, KeyModifiers::NONE),
            EditorOutcome::Cancel
        );
    }

    #[test]
    fn automation_editor_handle_key_tab_navigates_and_ctrl_e_toggles_enabled() {
        let mut m = AutomationEditorModal::default(); // Daily + Send, field=Name
        assert!(m.enabled);
        m.handle_key(KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert!(!m.enabled, "Ctrl+E flips enabled");
        // Tab / Down advance; BackTab / Up retreat (wrapping).
        m.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.field, AutomationField::Trigger);
        m.handle_key(KeyCode::BackTab, KeyModifiers::NONE);
        assert_eq!(m.field, AutomationField::Name);
        m.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(
            m.field,
            AutomationField::Prompt,
            "Up from first wraps to last"
        );
    }

    #[test]
    fn automation_editor_prompt_enter_inserts_newline_not_save() {
        let mut m = AutomationEditorModal {
            field: AutomationField::Prompt,
            ..Default::default()
        };
        m.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(
            m.handle_key(KeyCode::Enter, KeyModifiers::NONE),
            EditorOutcome::Continue,
            "Enter on the prompt inserts a newline, never saves"
        );
        m.handle_key(KeyCode::Char('b'), KeyModifiers::NONE);
        assert_eq!(m.prompt.value(), "a\nb");
    }

    #[test]
    fn automation_editor_ctrl_s_saves_from_prompt() {
        let mut m = AutomationEditorModal {
            field: AutomationField::Prompt,
            ..Default::default()
        };
        assert_eq!(
            m.handle_key(KeyCode::Char('s'), KeyModifiers::CONTROL),
            EditorOutcome::Save
        );
    }

    #[test]
    fn automation_editor_prompt_up_down_move_within_text() {
        let mut m = AutomationEditorModal {
            field: AutomationField::Prompt,
            ..Default::default()
        };
        m.prompt.set("a\nbb"); // cursor parks at the end → line 1
        assert_eq!(m.prompt.cursor_line_col().0, 1);
        m.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(
            m.prompt.cursor_line_col().0,
            0,
            "Up moves to the line above"
        );
        m.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(m.prompt.cursor_line_col().0, 1, "Down moves back");
        // Field navigation is unchanged: still on Prompt (Up/Down didn't nav).
        assert_eq!(m.field, AutomationField::Prompt);
    }

    #[test]
    fn automation_editor_prompt_tab_navigates_fields() {
        let mut m = AutomationEditorModal {
            field: AutomationField::Prompt,
            ..Default::default()
        }; // Daily + Send: Prompt is the last visible field.
        m.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(
            m.field,
            AutomationField::Name,
            "Tab from the last field wraps"
        );
        m.field = AutomationField::Prompt;
        m.handle_key(KeyCode::BackTab, KeyModifiers::NONE);
        assert_eq!(
            m.field,
            AutomationField::Target,
            "BackTab steps to the prior field"
        );
    }

    #[test]
    fn automation_editor_ctrl_e_still_toggles_on_prompt() {
        let mut m = AutomationEditorModal {
            field: AutomationField::Prompt,
            ..Default::default()
        };
        assert!(m.enabled);
        assert_eq!(
            m.handle_key(KeyCode::Char('e'), KeyModifiers::CONTROL),
            EditorOutcome::Continue
        );
        assert!(
            !m.enabled,
            "Ctrl+E toggles enabled even while editing the prompt"
        );
        assert_eq!(m.prompt.value(), "", "Ctrl+E is not inserted as text");
    }

    #[test]
    fn automation_editor_handle_key_adjusts_selector_fields() {
        let mut m = AutomationEditorModal {
            field: AutomationField::Trigger,
            ..Default::default()
        }; // Daily
           // Right / Space step forward, Left steps back.
        m.handle_key(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(m.trigger_kind, TriggerKind::Weekdays);
        m.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(m.trigger_kind, TriggerKind::Weekly);
        m.handle_key(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(m.trigger_kind, TriggerKind::Weekdays);
        // On a text field, Left/Right move the caret rather than adjusting.
        m.field = AutomationField::Name;
        m.name.set("ab");
        m.handle_key(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(m.name.cursor_pos(), 1);
    }

    #[test]
    fn automation_editor_set_target_sessions_selects_and_falls_back() {
        let a = crate::session::SessionId::default();
        let b = crate::session::SessionId::default();
        let mut m = AutomationEditorModal::default();
        m.set_target_sessions(vec![(a, "A".into()), (b, "B".into())], Some(b));
        assert_eq!(m.target_index, 1);
        assert_eq!(m.selected_target().map(|(id, _)| *id), Some(b));
        // An id that isn't present falls back to the first session.
        let absent = crate::session::SessionId::default();
        m.set_target_sessions(vec![(a, "A".into())], Some(absent));
        assert_eq!(m.target_index, 0);
        // No sessions → no selected target.
        m.set_target_sessions(vec![], None);
        assert!(m.selected_target().is_none());
    }

    #[test]
    fn automation_editor_timezone_trims_and_blanks_to_none() {
        let mut m = AutomationEditorModal::default();
        assert!(m.timezone().is_none(), "empty timezone is None");
        m.timezone.set("  UTC  ");
        assert_eq!(m.timezone().as_deref(), Some("UTC"));
        m.timezone.set("   ");
        assert!(m.timezone().is_none(), "whitespace-only timezone is None");
    }

    // ── TaskEditorModal::handle_key field navigation + description keys ──

    #[test]
    fn task_editor_tab_navigates_fields() {
        let mut m = TaskEditorModal::new(); // field=Title
        m.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.field, TaskField::Description);
        m.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.field, TaskField::Status);
        m.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(
            m.field,
            TaskField::Title,
            "Tab wraps back to the first field"
        );
        // BackTab retreats and wraps.
        m.handle_key(KeyCode::BackTab, KeyModifiers::NONE);
        assert_eq!(m.field, TaskField::Status);
    }

    #[test]
    fn task_editor_description_editing_keys_and_tab_navigation() {
        let mut m = TaskEditorModal::new();
        m.field = TaskField::Description;
        for c in "ac".chars() {
            m.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        // Left then insert 'b' in the middle.
        m.handle_key(KeyCode::Left, KeyModifiers::NONE);
        m.handle_key(KeyCode::Char('b'), KeyModifiers::NONE);
        assert_eq!(m.description.value(), "abc");
        // Home + Delete removes the first char; End repositions to line end.
        m.handle_key(KeyCode::Home, KeyModifiers::NONE);
        m.handle_key(KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(m.description.value(), "bc");
        m.handle_key(KeyCode::End, KeyModifiers::NONE);
        m.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(m.description.value(), "b");
        // Tab still navigates out of the multi-line field (it is not inserted).
        assert_eq!(
            m.handle_key(KeyCode::Tab, KeyModifiers::NONE),
            EditorOutcome::Continue
        );
        assert_eq!(m.field, TaskField::Status);
    }

    #[test]
    fn task_editor_description_up_down_move_within_text() {
        let mut m = TaskEditorModal::new();
        m.field = TaskField::Description;
        for c in "ab".chars() {
            m.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        m.handle_key(KeyCode::Enter, KeyModifiers::NONE); // newline
        m.handle_key(KeyCode::Char('c'), KeyModifiers::NONE);
        assert_eq!(m.description.cursor_line_col(), (1, 1));
        m.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(m.description.cursor_line_col(), (0, 1));
        m.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(m.description.cursor_line_col(), (1, 1));
    }

    #[test]
    fn settings_order_lists_every_field_once() {
        assert_eq!(SettingsField::ORDER.len(), 19);
        for f in SettingsField::ORDER {
            assert_eq!(
                SettingsField::ORDER.iter().filter(|x| **x == f).count(),
                1,
                "{f:?} duplicated"
            );
        }
    }

    #[test]
    fn settings_space_toggles_bool_and_enter_does_too() {
        let mut m = SettingsModal::new(crate::session::settings::Settings::default());
        assert!(m.draft.features.tasks);
        m.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(!m.draft.features.tasks);
        m.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(m.draft.features.tasks);
    }

    #[test]
    fn settings_scalar_steps_and_clamps() {
        let mut m = SettingsModal::new(crate::session::settings::Settings::default());
        m.field = SettingsField::ScrollbackLines;
        m.handle_key(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(m.draft.scrollback_lines, 1500);
        m.handle_key(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(m.draft.scrollback_lines, 1000);
        // Space adjusts scalars (does not toggle).
        m.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(m.draft.scrollback_lines, 1500);
        // Clamp at the floor.
        m.field = SettingsField::TwoPanelMinCols;
        for _ in 0..100 {
            m.handle_key(KeyCode::Left, KeyModifiers::NONE);
        }
        assert_eq!(m.draft.two_panel_min_cols, 40);
    }

    #[test]
    fn settings_save_and_cancel_outcomes() {
        let mut m = SettingsModal::new(crate::session::settings::Settings::default());
        assert_eq!(
            m.handle_key(KeyCode::Char('s'), KeyModifiers::CONTROL),
            EditorOutcome::Save
        );
        assert_eq!(
            m.handle_key(KeyCode::Esc, KeyModifiers::NONE),
            EditorOutcome::Cancel
        );
    }

    #[test]
    fn settings_restart_required_tracks_only_restart_fields() {
        let mut m = SettingsModal::new(crate::session::settings::Settings::default());
        // A live flag changing does not arm the restart note.
        m.field = SettingsField::FeatTasks;
        m.toggle();
        assert!(!m.restart_required_changed());
        // A restart-required flag does.
        m.field = SettingsField::FeatMouse;
        m.toggle();
        assert!(m.restart_required_changed());
    }

    #[test]
    fn settings_restart_required_classification() {
        use SettingsField::*;
        for f in [
            FeatTasks,
            FeatFileViewer,
            FeatGlobalSearch,
            FeatInfoPanel,
            FeatShellPane,
            FeatSoftDelete,
        ] {
            assert!(!f.restart_required(), "{f:?} should be live");
        }
        for f in [
            FeatMouse,
            FeatNotifications,
            FeatAutomations,
            ScrollbackLines,
        ] {
            assert!(f.restart_required(), "{f:?} should need restart");
        }
    }

    /// The per-field `restart_required` flag (UI marker) and
    /// `Settings::restart_only_differs` (the comparison driving the toast) are
    /// two encodings of the same live/restart split — toggling any
    /// restart-required field must register as a restart-only difference, and a
    /// live field must not.
    #[test]
    fn settings_restart_classifications_agree() {
        for field in SettingsField::ORDER {
            let mut m = SettingsModal::new(crate::session::settings::Settings::default());
            m.field = field;
            if field.is_scalar() {
                m.adjust(1);
            } else {
                m.toggle();
            }
            assert_eq!(
                m.draft.restart_only_differs(&m.original),
                field.restart_required(),
                "{field:?}: restart_required and restart_only_differs disagree",
            );
        }
    }
}
