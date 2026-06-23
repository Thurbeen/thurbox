use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{AutomationActionKind, AutomationField, TriggerKind};

use super::theme::Theme;
use super::{centered_fixed_height_rect, render_modal_frame};

/// Maximum prompt text rows shown at once (the label row is extra). Keeps the
/// editor tight: a taller prompt scrolls within this window instead of growing
/// the modal / pane. Applies to both the centered overlay and the in-pane editor.
const MAX_PROMPT_VISIBLE_ROWS: usize = 10;

pub struct AutomationEditorState<'a> {
    pub editing: bool,
    pub field: AutomationField,
    pub trigger_kind: TriggerKind,
    pub action: AutomationActionKind,
    pub enabled: bool,
    pub name: &'a str,
    pub delay: &'a str,
    pub weekday: u32,
    pub hour: u32,
    pub minute: u32,
    pub cron_expr: &'a str,
    pub timezone: &'a str,
    pub repo: &'a str,
    pub worktree: &'a str,
    pub agent: &'a str,
    pub command: &'a str,
    pub prompt: &'a str,
    /// Caret `(line, col)` within the prompt, for drawing the block cursor while
    /// the multi-line `Prompt` field is active.
    pub prompt_cursor: (usize, usize),
    /// Display name of the Send target session, if any.
    pub target_session: Option<&'a str>,
    /// Human summary of when this will next fire (computed by the caller).
    pub preview: &'a str,
    /// Whether the editor currently has keyboard focus. When `false` (an in-pane
    /// preview), the active-field cursor/highlight is suppressed and the border
    /// is drawn unfocused.
    pub focused: bool,
}

impl<'a> AutomationEditorState<'a> {
    /// Borrow view data from an editor modal. Shared by the centered-overlay and
    /// in-pane render paths so the 18-field projection lives in one place.
    pub fn from_modal(
        m: &'a crate::app::modals::AutomationEditorModal,
        preview: &'a str,
        focused: bool,
    ) -> Self {
        Self {
            editing: m.editing_id.is_some(),
            field: m.field,
            trigger_kind: m.trigger_kind,
            action: m.action,
            enabled: m.enabled,
            name: m.name.value(),
            delay: m.delay.value(),
            weekday: m.weekday,
            hour: m.hour,
            minute: m.minute,
            cron_expr: m.cron_expr.value(),
            timezone: m.timezone.value(),
            repo: m.repo.value(),
            worktree: m.worktree.value(),
            agent: m.agent.value(),
            command: m.command.value(),
            prompt: m.prompt.value(),
            prompt_cursor: m.prompt.cursor_line_col(),
            target_session: m.selected_target().map(|(_, name)| name.as_str()),
            preview,
            focused,
        }
    }
}

/// Fields shown for the current trigger kind + action, in order. Mirrors
/// `AutomationEditorModal::visible_fields`.
fn visible_fields(trigger: TriggerKind, action: AutomationActionKind) -> Vec<AutomationField> {
    use AutomationField::*;
    let mut fields = vec![Name, Trigger];
    match trigger {
        TriggerKind::Once => fields.push(Delay),
        TriggerKind::Hourly => fields.push(Minute),
        TriggerKind::Daily | TriggerKind::Weekdays => fields.extend([Hour, Minute]),
        TriggerKind::Weekly => fields.extend([Weekday, Hour, Minute]),
        TriggerKind::Cron => fields.push(CronExpr),
    }
    if trigger != TriggerKind::Once {
        fields.push(Timezone);
    }
    fields.push(Action);
    match action {
        AutomationActionKind::Send => fields.push(Target),
        AutomationActionKind::Spawn => fields.extend([Repo, Worktree, Agent]),
        AutomationActionKind::Exec => fields.push(Command),
    }
    if action != AutomationActionKind::Exec {
        fields.push(Prompt);
    }
    fields
}

/// Render the editor as a centered modal overlay (the `Ctrl+P` list path).
pub fn render_automation_editor_modal(frame: &mut Frame, state: &AutomationEditorState<'_>) {
    let frame_area = frame.area();
    let fields = visible_fields(state.trigger_kind, state.action);
    // One row per field + status + preview + spacing inside the modal frame.
    // The multi-line Prompt field spans more than one row, so grow the height by
    // its wrapped row count (the old `+ 5` already counted Prompt as one row).
    let extra = if fields.contains(&AutomationField::Prompt) {
        // Inner text width of the 60%-wide overlay (minus the 1-col borders).
        let inner_w = (frame_area.width * 60 / 100).saturating_sub(2);
        prompt_display_rows(state.prompt, inner_w).saturating_sub(1)
    } else {
        0
    };
    let height = fields.len() as u16 + 5 + extra;
    let area = centered_fixed_height_rect(60, height.min(frame_area.height), frame_area);

    let inner = render_modal_frame(frame, area, editor_title(state));
    let lines = windowed_editor_lines(state, inner.width, inner.height);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Render the editor inline into a given area (the automations-pane central
/// view), framed by a border whose style reflects [`AutomationEditorState::focused`].
pub fn render_automation_editor_into(
    frame: &mut Frame,
    area: Rect,
    state: &AutomationEditorState<'_>,
) {
    let inner = super::render_editor_frame(frame, area, editor_title(state), state.focused);
    let lines = windowed_editor_lines(state, inner.width, inner.height);
    frame.render_widget(Paragraph::new(lines), inner);
}

fn editor_title(state: &AutomationEditorState<'_>) -> &'static str {
    if state.editing {
        "Edit Automation"
    } else {
        "New Automation"
    }
}

/// The full editor body (every field, the prompt expanded into wrapped rows)
/// plus the pinned footer (preview / status / key hints), scroll-windowed to
/// `height` rows around the active field's row so the cursor stays visible when
/// the prompt is taller than the available area. Shared by the centered overlay
/// and the in-pane editor; mirrors the settings modal's scroll-windowing.
fn windowed_editor_lines<'a>(
    state: &AutomationEditorState<'a>,
    width: u16,
    height: u16,
) -> Vec<Line<'a>> {
    let (body, active_row) = editor_body_lines(state, width);
    let footer = editor_footer_lines(state);

    // The footer is always pinned at the bottom; the body scrolls in the rest.
    let visible = (height as usize).saturating_sub(footer.len());
    let mut lines = if body.len() <= visible || visible == 0 {
        body
    } else {
        // Keep the active row centered in the window where possible.
        let active = active_row.unwrap_or(0);
        let start = active.saturating_sub(visible / 2).min(body.len() - visible);
        body.into_iter().skip(start).take(visible).collect()
    };
    lines.extend(footer);
    lines
}

/// The body rows (one line per field, the prompt expanded into wrapped rows) and
/// the display index of the active field's row — the focus point the caller
/// scroll-windows around.
fn editor_body_lines<'a>(
    state: &AutomationEditorState<'a>,
    width: u16,
) -> (Vec<Line<'a>>, Option<usize>) {
    let fields = visible_fields(state.trigger_kind, state.action);
    let mut lines: Vec<Line> = Vec::new();
    let mut active_row = None;
    for f in &fields {
        let active = *f == state.field && state.focused;
        // The multi-line prompt expands into a label row + wrapped text rows;
        // every other field is a single row.
        if *f == AutomationField::Prompt {
            let base = lines.len();
            let (rows, cursor_idx) = prompt_lines(state, active, width);
            if let Some(idx) = cursor_idx {
                active_row = Some(base + idx);
            }
            lines.extend(rows);
        } else {
            if active {
                active_row = Some(lines.len());
            }
            lines.push(field_line(*f, state, active));
        }
    }
    (lines, active_row)
}

/// The pinned footer: the live schedule preview, the enabled status, and the
/// key-hint line.
fn editor_footer_lines<'a>(state: &AutomationEditorState<'a>) -> Vec<Line<'a>> {
    let mut lines = Vec::new();

    // Live preview of the resulting schedule.
    lines.push(Line::from(vec![
        Span::styled("  next     ", Theme::label()),
        Span::styled(state.preview, Style::default().fg(Theme::accent())),
    ]));

    // Enabled status.
    let enabled_label = if state.enabled {
        Span::styled("enabled", Style::default().fg(Theme::accent()))
    } else {
        Span::styled("disabled", Style::default().fg(Theme::text_muted()))
    };
    lines.push(Line::from(vec![
        Span::styled("  status   ", Theme::label()),
        enabled_label,
    ]));

    lines.push(super::key_hint_line(&[
        ("Tab/↑↓", " move  "),
        ("←→", " adjust  "),
        ("^E", " enable  "),
        ("Enter", " save/newline  "),
        ("^S", " save  "),
        ("Esc", " cancel"),
    ]));

    lines
}

/// Render the prompt as a `▸ prompt` label row followed by its indented,
/// hard-wrapped text rows, drawing a block cursor at `state.prompt_cursor` when
/// active. An empty, inactive prompt shows a dimmed `(none)`. Mirrors the task
/// editor's `description_lines`, plus width-aware wrapping so long lines never
/// overflow and a [`MAX_PROMPT_VISIBLE_ROWS`] cap so a tall prompt scrolls
/// within a tight window instead of growing the editor.
///
/// Returns the display rows and, when `active`, the index (within those rows) of
/// the row carrying the block cursor — so the caller can scroll-window the whole
/// body to keep it visible too.
fn prompt_lines<'a>(
    state: &AutomationEditorState<'a>,
    active: bool,
    width: u16,
) -> (Vec<Line<'a>>, Option<usize>) {
    let prefix = if active { "▸ " } else { "  " };
    let mut lines = vec![Line::from(Span::styled(
        format!("{prefix}{:<9}", "prompt"),
        Theme::label(),
    ))];

    let text_style = if active {
        Style::default()
            .fg(Theme::border_focused())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Theme::text_primary())
    };

    if state.prompt.is_empty() && !active {
        lines.push(Line::from(Span::styled(
            "    (none)",
            Style::default().fg(Theme::text_secondary()),
        )));
        return (lines, None);
    }

    // Build every wrapped text row first, tracking which one carries the cursor.
    let wrap_w = prompt_wrap_width(width);
    let (cur_line, cur_col) = state.prompt_cursor;
    let mut text_rows: Vec<Line> = Vec::new();
    let mut text_cursor = None;
    for (li, logical) in state.prompt.split('\n').enumerate() {
        let chars: Vec<char> = logical.chars().collect();
        let rows = wrap_chars(&chars, wrap_w);
        let cursor_row = if active && li == cur_line {
            let (row, col) = wrapped_cursor(cur_col, wrap_w, rows.len());
            Some((row, col))
        } else {
            None
        };
        for (ri, chunk) in rows.iter().enumerate() {
            match cursor_row {
                Some((row, col)) if row == ri => {
                    text_cursor = Some(text_rows.len());
                    text_rows.push(prompt_cursor_row(chunk, col, text_style));
                }
                _ => text_rows.push(prompt_plain_row(chunk, text_style)),
            }
        }
    }

    // Cap the visible text rows so the editor stays tight; a taller prompt
    // scrolls within the window, kept centered on the cursor.
    let (visible, start) =
        window_rows(text_rows, text_cursor.unwrap_or(0), MAX_PROMPT_VISIBLE_ROWS);
    let cursor_idx = text_cursor.map(|tc| 1 + tc.saturating_sub(start));
    lines.extend(visible);
    (lines, cursor_idx)
}

/// Window `rows` to at most `max` entries, centered on `focus`, returning the
/// kept rows and the start offset (so a caller can re-map an index into them).
fn window_rows<'a>(rows: Vec<Line<'a>>, focus: usize, max: usize) -> (Vec<Line<'a>>, usize) {
    if rows.len() <= max || max == 0 {
        return (rows, 0);
    }
    let start = focus.saturating_sub(max / 2).min(rows.len() - max);
    let kept = rows.into_iter().skip(start).take(max).collect();
    (kept, start)
}

/// Usable width for wrapped prompt text rows: the inner width minus the 4-space
/// indent, floored to at least 1 so wrapping never divides by zero.
fn prompt_wrap_width(width: u16) -> usize {
    (width as usize).saturating_sub(4).max(1)
}

/// Total display rows the prompt occupies (1 label row + the wrapped text rows,
/// capped at [`MAX_PROMPT_VISIBLE_ROWS`]), so the centered overlay's height and
/// body agree on the count and the modal never grows past the tight cap.
fn prompt_display_rows(prompt: &str, width: u16) -> u16 {
    let wrap_w = prompt_wrap_width(width);
    let text_rows: usize = prompt
        .split('\n')
        .map(|logical| {
            let chars: Vec<char> = logical.chars().collect();
            wrap_chars(&chars, wrap_w).len()
        })
        .sum();
    (1 + text_rows.min(MAX_PROMPT_VISIBLE_ROWS)).min(u16::MAX as usize) as u16
}

/// Hard-wrap one logical line into chunks of at most `wrap_w` chars. An empty
/// line yields one empty chunk so it still occupies a row.
fn wrap_chars(line: &[char], wrap_w: usize) -> Vec<Vec<char>> {
    if line.is_empty() {
        return vec![Vec::new()];
    }
    let wrap_w = wrap_w.max(1);
    line.chunks(wrap_w).map(|c| c.to_vec()).collect()
}

/// Map a logical column on a logical line to `(wrapped_row, wrapped_col)` for
/// hard char wrapping. A caret past the last produced row (end of a line whose
/// length is a multiple of `wrap_w`) is clamped to the last row's tail so the
/// block cursor stays visible just after the wrap boundary.
fn wrapped_cursor(col: usize, wrap_w: usize, n_rows: usize) -> (usize, usize) {
    let wrap_w = wrap_w.max(1);
    let row = col / wrap_w;
    if row >= n_rows {
        (n_rows.saturating_sub(1), wrap_w)
    } else {
        (row, col % wrap_w)
    }
}

/// One indented prompt row with a block cursor drawn at char index `caret`.
fn prompt_cursor_row<'a>(chunk: &[char], caret: usize, text_style: Style) -> Line<'a> {
    let caret = caret.min(chunk.len());
    let mut spans = vec![Span::raw("    ")];
    let before: String = chunk[..caret].iter().collect();
    if !before.is_empty() {
        spans.push(Span::styled(before, text_style));
    }
    let cursor_char = chunk
        .get(caret)
        .map(|c| c.to_string())
        .unwrap_or_else(|| " ".to_string());
    spans.push(Span::styled(cursor_char, Theme::cursor()));
    let after: String = chunk
        .get(caret + 1..)
        .map(|s| s.iter().collect())
        .unwrap_or_default();
    if !after.is_empty() {
        spans.push(Span::styled(after, text_style));
    }
    Line::from(spans)
}

/// One indented prompt row without a cursor.
fn prompt_plain_row<'a>(chunk: &[char], text_style: Style) -> Line<'a> {
    let mut spans = vec![Span::raw("    ")];
    if !chunk.is_empty() {
        let text: String = chunk.iter().collect();
        spans.push(Span::styled(text, text_style));
    }
    Line::from(spans)
}

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

fn field_line<'a>(
    field: AutomationField,
    state: &AutomationEditorState<'a>,
    is_active_field: bool,
) -> Line<'a> {
    // Only show the active-field cursor/highlight when the editor itself is
    // focused; an unfocused in-pane preview renders every field plainly.
    let focused = is_active_field && state.focused;
    // (label, value, is_selector). Selector/stepper values are wrapped in
    // ‹ › guillemets to signal they are adjusted with ←/→, not typed.
    let (label, value, selector): (&str, String, bool) = match field {
        AutomationField::Name => ("name", state.name.to_string(), false),
        AutomationField::Trigger => ("trigger", state.trigger_kind.label().to_string(), true),
        AutomationField::Delay => ("in", delay_display(state.delay), false),
        AutomationField::Weekday => (
            "weekday",
            WEEKDAYS[(state.weekday % 7) as usize].to_string(),
            true,
        ),
        AutomationField::Hour => ("hour", format!("{:02}", state.hour), true),
        AutomationField::Minute => ("minute", format!("{:02}", state.minute), true),
        AutomationField::CronExpr => ("cron", cron_display(state.cron_expr), false),
        AutomationField::Timezone => ("timezone", tz_display(state.timezone), false),
        AutomationField::Action => ("action", action_display(state), true),
        AutomationField::Target => ("target", target_display(state), true),
        AutomationField::Repo => ("repo", state.repo.to_string(), false),
        AutomationField::Worktree => ("worktree", optional_display(state.worktree), false),
        AutomationField::Agent => ("agent", optional_display(state.agent), false),
        AutomationField::Command => ("command", state.command.to_string(), false),
        // The multi-line prompt is rendered by `prompt_lines`; `editor_body_lines`
        // never routes it here.
        AutomationField::Prompt => unreachable!("Prompt is rendered via prompt_lines"),
    };

    super::editor_field_line(label, value, selector, focused)
}

fn delay_display(delay: &str) -> String {
    if delay.is_empty() {
        "(e.g. 30m, 2h, 1h30m)".to_string()
    } else {
        delay.to_string()
    }
}

fn cron_display(expr: &str) -> String {
    if expr.is_empty() {
        "(e.g. 0 9 * * 1-5)".to_string()
    } else {
        expr.to_string()
    }
}

fn tz_display(tz: &str) -> String {
    if tz.is_empty() {
        "(local)".to_string()
    } else {
        tz.to_string()
    }
}

fn optional_display(value: &str) -> String {
    if value.is_empty() {
        "(none)".to_string()
    } else {
        value.to_string()
    }
}

fn action_display(state: &AutomationEditorState<'_>) -> String {
    match state.action {
        AutomationActionKind::Send => "send".to_string(),
        AutomationActionKind::Spawn => "spawn".to_string(),
        AutomationActionKind::Exec => "exec".to_string(),
    }
}

/// The selected Send target session, or a hint when none are running.
fn target_display(state: &AutomationEditorState<'_>) -> String {
    state
        .target_session
        .unwrap_or("(no running sessions)")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use AutomationField::*;

    fn state() -> AutomationEditorState<'static> {
        AutomationEditorState {
            editing: false,
            field: AutomationField::Name,
            trigger_kind: TriggerKind::Daily,
            action: AutomationActionKind::Send,
            enabled: true,
            name: "",
            delay: "",
            weekday: 0,
            hour: 0,
            minute: 0,
            cron_expr: "",
            timezone: "",
            repo: "",
            worktree: "",
            agent: "",
            command: "",
            prompt: "",
            prompt_cursor: (0, 0),
            target_session: None,
            preview: "",
            focused: true,
        }
    }

    #[test]
    fn visible_fields_once_has_delay_and_no_timezone() {
        let f = visible_fields(TriggerKind::Once, AutomationActionKind::Send);
        assert_eq!(f, vec![Name, Trigger, Delay, Action, Target, Prompt]);
        assert!(!f.contains(&Timezone), "Once never shows a timezone field");
    }

    #[test]
    fn visible_fields_schedule_kinds_carry_timezone_and_time_steppers() {
        assert_eq!(
            visible_fields(TriggerKind::Hourly, AutomationActionKind::Send),
            vec![Name, Trigger, Minute, Timezone, Action, Target, Prompt]
        );
        assert_eq!(
            visible_fields(TriggerKind::Daily, AutomationActionKind::Send),
            vec![Name, Trigger, Hour, Minute, Timezone, Action, Target, Prompt]
        );
        assert_eq!(
            visible_fields(TriggerKind::Weekdays, AutomationActionKind::Send),
            vec![Name, Trigger, Hour, Minute, Timezone, Action, Target, Prompt]
        );
        assert_eq!(
            visible_fields(TriggerKind::Weekly, AutomationActionKind::Send),
            vec![Name, Trigger, Weekday, Hour, Minute, Timezone, Action, Target, Prompt]
        );
    }

    #[test]
    fn visible_fields_cron_shows_cron_expr() {
        let f = visible_fields(TriggerKind::Cron, AutomationActionKind::Send);
        assert_eq!(
            f,
            vec![Name, Trigger, CronExpr, Timezone, Action, Target, Prompt]
        );
    }

    #[test]
    fn visible_fields_action_shapes_the_tail() {
        // Send → Target + Prompt.
        let send = visible_fields(TriggerKind::Daily, AutomationActionKind::Send);
        assert_eq!(&send[send.len() - 2..], &[Target, Prompt]);

        // Spawn → Repo/Worktree/Agent + Prompt.
        let spawn = visible_fields(TriggerKind::Daily, AutomationActionKind::Spawn);
        assert_eq!(&spawn[spawn.len() - 4..], &[Repo, Worktree, Agent, Prompt]);

        // Exec → Command and NO Prompt.
        let exec = visible_fields(TriggerKind::Daily, AutomationActionKind::Exec);
        assert!(exec.contains(&Command));
        assert!(
            !exec.contains(&Prompt),
            "Exec runs a shell command, never a prompt"
        );
        assert_eq!(*exec.last().unwrap(), Command);
    }

    #[test]
    fn formatters_show_placeholder_when_empty() {
        assert_eq!(delay_display(""), "(e.g. 30m, 2h, 1h30m)");
        assert_eq!(cron_display(""), "(e.g. 0 9 * * 1-5)");
        assert_eq!(tz_display(""), "(local)");
        assert_eq!(optional_display(""), "(none)");
    }

    #[test]
    fn formatters_pass_through_non_empty_values() {
        assert_eq!(delay_display("30m"), "30m");
        assert_eq!(cron_display("0 9 * * 1-5"), "0 9 * * 1-5");
        assert_eq!(tz_display("Europe/Zurich"), "Europe/Zurich");
        assert_eq!(optional_display("main"), "main");
    }

    #[test]
    fn action_and_target_display() {
        let mut s = state();
        s.action = AutomationActionKind::Exec;
        assert_eq!(action_display(&s), "exec");

        // No running sessions → hint.
        assert_eq!(target_display(&s), "(no running sessions)");
        s.target_session = Some("backend");
        assert_eq!(target_display(&s), "backend");
    }

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn wrap_chars_splits_at_width() {
        let thirty: Vec<char> = "x".repeat(30).chars().collect();
        let rows = wrap_chars(&thirty, 10);
        assert_eq!(
            rows.iter().map(|r| r.len()).collect::<Vec<_>>(),
            vec![10, 10, 10]
        );
        let twenty_five: Vec<char> = "x".repeat(25).chars().collect();
        let rows = wrap_chars(&twenty_five, 10);
        assert_eq!(
            rows.iter().map(|r| r.len()).collect::<Vec<_>>(),
            vec![10, 10, 5]
        );
    }

    #[test]
    fn wrap_chars_empty_line_is_one_empty_row() {
        assert_eq!(wrap_chars(&[], 10), vec![Vec::<char>::new()]);
    }

    #[test]
    fn wrap_chars_short_line_is_one_row() {
        assert_eq!(wrap_chars(&chars("hi"), 10), vec![chars("hi")]);
    }

    #[test]
    fn wrapped_cursor_maps_logical_col_to_display() {
        // start
        assert_eq!(wrapped_cursor(0, 10, 1), (0, 0));
        // mid-row
        assert_eq!(wrapped_cursor(5, 10, 1), (0, 5));
        // exactly on a wrap boundary moves to the next row's start
        assert_eq!(wrapped_cursor(10, 10, 2), (1, 0));
        // caret past the last produced row (end of a full final row) clamps to
        // the last row's tail so the block cursor stays visible
        assert_eq!(wrapped_cursor(10, 10, 1), (0, 10));
    }

    #[test]
    fn prompt_display_rows_counts_label_plus_wrapped_rows() {
        // Empty prompt: label + one (empty) text row.
        assert_eq!(prompt_display_rows("", 20), 2);
        // Two logical lines, second wraps to 2 rows at width 20 (wrap_w = 16).
        let p = format!("short\n{}", "y".repeat(20));
        assert_eq!(prompt_display_rows(&p, 20), 1 + 1 + 2);
    }

    fn has_cursor_span(line: &Line) -> bool {
        line.spans.iter().any(|s| s.style == Theme::cursor())
    }

    #[test]
    fn prompt_lines_wraps_and_draws_cursor() {
        let mut s = state();
        let long = "y".repeat(20);
        let text = format!("short\n{long}");
        s.prompt = &text;
        // Cursor on the second logical line, col 18 → second wrapped row.
        s.prompt_cursor = (1, 18);
        let (lines, cursor_idx) = prompt_lines(&s, true, 20); // wrap_w = 16
                                                              // label + 1 (short) + 2 (wrapped long) = 4 rows.
        assert_eq!(lines.len(), 4);
        // The cursor lands on the last row (col 18 → row 1 of the long line).
        assert_eq!(cursor_idx, Some(3));
        assert!(has_cursor_span(&lines[3]));
        assert!(!has_cursor_span(&lines[1]));
    }

    #[test]
    fn prompt_lines_empty_inactive_shows_none() {
        let s = state();
        let (lines, cursor_idx) = prompt_lines(&s, false, 40);
        assert_eq!(lines.len(), 2);
        assert_eq!(cursor_idx, None);
        let none_text: String = lines[1]
            .spans
            .iter()
            .map(|sp| sp.content.as_ref())
            .collect();
        assert!(none_text.contains("(none)"));
    }

    #[test]
    fn prompt_lines_empty_active_shows_single_cursor_row() {
        let mut s = state();
        s.prompt = "";
        s.prompt_cursor = (0, 0);
        let (lines, cursor_idx) = prompt_lines(&s, true, 40);
        // label + one cursor row.
        assert_eq!(lines.len(), 2);
        assert_eq!(cursor_idx, Some(1));
        assert!(has_cursor_span(&lines[1]));
    }

    #[test]
    fn window_rows_centers_and_clamps() {
        let rows = || -> Vec<Line<'static>> { (0..10).map(|_| Line::from("")).collect() };
        // Fits → returned whole, no offset.
        let (kept, start) = window_rows(rows(), 0, 20);
        assert_eq!((kept.len(), start), (10, 0));
        // Focus near the start clamps the window to the top.
        let (kept, start) = window_rows(rows(), 0, 4);
        assert_eq!((kept.len(), start), (4, 0));
        // Focus in the middle centers the window.
        let (kept, start) = window_rows(rows(), 5, 4);
        assert_eq!((kept.len(), start), (4, 3));
        // Focus near the end clamps to the bottom.
        let (kept, start) = window_rows(rows(), 9, 4);
        assert_eq!((kept.len(), start), (4, 6));
        // max == 0 is a no-op (avoids a zero-width window).
        let (kept, start) = window_rows(rows(), 5, 0);
        assert_eq!((kept.len(), start), (10, 0));
    }

    #[test]
    fn prompt_lines_caps_visible_rows_and_keeps_cursor() {
        let mut s = state();
        // 30 logical lines → 30 text rows, capped to MAX_PROMPT_VISIBLE_ROWS.
        let text = (0..30).fold(String::new(), |mut acc, i| {
            if !acc.is_empty() {
                acc.push('\n');
            }
            acc.push((b'a' + (i % 26) as u8) as char);
            acc
        });
        s.prompt = &text;
        s.prompt_cursor = (25, 0); // cursor near the end

        let (lines, cursor_idx) = prompt_lines(&s, true, 40);
        // label + at most MAX visible text rows.
        assert_eq!(lines.len(), 1 + MAX_PROMPT_VISIBLE_ROWS);
        // The cursor row is within the window and actually carries the cursor.
        let idx = cursor_idx.expect("active prompt has a cursor row");
        assert!(idx >= 1 && idx < lines.len());
        assert!(has_cursor_span(&lines[idx]));
    }

    #[test]
    fn prompt_display_rows_is_capped() {
        // 50 logical lines, but the reported height caps the text rows.
        let text = "x\n".repeat(50);
        assert_eq!(
            prompt_display_rows(&text, 40),
            1 + MAX_PROMPT_VISIBLE_ROWS as u16
        );
    }

    #[test]
    fn windowed_editor_lines_scrolls_to_keep_cursor_visible() {
        // A prompt taller than the available height must window so the cursor row
        // stays on screen, with the 3-line footer (preview/status/hints) pinned.
        let mut s = state();
        s.field = AutomationField::Prompt;
        // 40 single-char logical lines → 40 wrapped rows + 1 label row.
        let text = (0..40)
            .map(|i| (b'a' + (i % 26)) as char)
            .fold(String::new(), |mut acc, c| {
                if !acc.is_empty() {
                    acc.push('\n');
                }
                acc.push(c);
                acc
            });
        s.prompt = &text;
        s.prompt_cursor = (39, 0); // cursor on the last logical line

        let height = 12; // small modal: 3 footer + 9 body rows visible
        let lines = windowed_editor_lines(&s, 60, height);
        assert_eq!(lines.len(), height as usize, "fills exactly the height");
        // The window scrolled to the bottom, so the cursor row is present...
        assert!(
            lines.iter().any(has_cursor_span),
            "cursor row stays visible after scrolling"
        );
        // ...and the footer (the key-hint line) is pinned at the very bottom.
        let last: String = lines[lines.len() - 1]
            .spans
            .iter()
            .map(|sp| sp.content.as_ref())
            .collect();
        assert!(last.contains("save"), "footer key hints pinned at bottom");
    }

    #[test]
    fn windowed_editor_lines_no_scroll_when_it_fits() {
        // With ample height the whole body renders (no windowing): every field
        // row plus the prompt is present and the footer follows.
        let s = state();
        let lines = windowed_editor_lines(&s, 60, 40);
        // Daily+Send body = 8 field rows (prompt expands to label + 1 (none) row
        // when empty+inactive... here Name is active so prompt is inactive empty).
        // Just assert the footer is present and nothing was dropped.
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|sp| sp.content.as_ref())
            .collect();
        assert!(joined.contains("prompt"));
        assert!(joined.contains("save"));
    }
}
