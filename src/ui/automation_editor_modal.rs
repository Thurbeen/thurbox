use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::{AutomationActionKind, AutomationField, TriggerKind};

use super::theme::Theme;
use super::{centered_fixed_height_rect, render_modal_frame};

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
    pub prompt: &'a str,
    /// Display name of the Send target session, if any.
    pub target_session: Option<&'a str>,
    /// Human summary of when this will next fire (computed by the caller).
    pub preview: &'a str,
    /// Whether the editor currently has keyboard focus. When `false` (an in-pane
    /// preview), the active-field cursor/highlight is suppressed and the border
    /// is drawn unfocused.
    pub focused: bool,
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
    }
    fields.push(Prompt);
    fields
}

/// Render the editor as a centered modal overlay (the `Ctrl+P` list path).
pub fn render_automation_editor_modal(frame: &mut Frame, state: &AutomationEditorState<'_>) {
    let fields = visible_fields(state.trigger_kind, state.action);
    // One row per field + status + preview + spacing inside the modal frame.
    let height = fields.len() as u16 + 5;
    let area = centered_fixed_height_rect(60, height.min(22), frame.area());

    let inner = render_modal_frame(frame, area, editor_title(state));
    frame.render_widget(Paragraph::new(editor_lines(state)), inner);
}

/// Render the editor inline into a given area (the automations-pane central
/// view), framed by a border whose style reflects [`AutomationEditorState::focused`].
pub fn render_automation_editor_into(
    frame: &mut Frame,
    area: Rect,
    state: &AutomationEditorState<'_>,
) {
    let border = if state.focused {
        Theme::border_focused()
    } else {
        Theme::border_unfocused()
    };
    let block = Block::default()
        .title(format!(" {} ", editor_title(state)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(editor_lines(state)), inner);
}

fn editor_title(state: &AutomationEditorState<'_>) -> &'static str {
    if state.editing {
        "Edit Automation"
    } else {
        "New Automation"
    }
}

/// The editor body: one line per visible field, then the live schedule preview,
/// enabled status, and the key-hint footer.
fn editor_lines<'a>(state: &AutomationEditorState<'a>) -> Vec<Line<'a>> {
    let fields = visible_fields(state.trigger_kind, state.action);
    let mut lines: Vec<Line> = fields
        .iter()
        .map(|f| field_line(*f, state, *f == state.field))
        .collect();

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

    lines.push(Line::from(vec![
        Span::styled("Tab/↑↓", Theme::keybind()),
        Span::styled(" move  ", Theme::keybind_desc()),
        Span::styled("←→", Theme::keybind()),
        Span::styled(" adjust  ", Theme::keybind_desc()),
        Span::styled("^E", Theme::keybind()),
        Span::styled(" enable  ", Theme::keybind_desc()),
        Span::styled("Enter", Theme::keybind()),
        Span::styled(" save  ", Theme::keybind_desc()),
        Span::styled("Esc", Theme::keybind()),
        Span::styled(" cancel", Theme::keybind_desc()),
    ]));

    lines
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
        AutomationField::Prompt => ("prompt", state.prompt.to_string(), false),
    };

    let prefix = if focused { "▸ " } else { "  " };
    let value_style = if focused {
        Style::default()
            .fg(Theme::border_focused())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Theme::text_primary())
    };

    let display = if selector {
        format!("‹ {value} ›")
    } else if focused {
        // A trailing cursor block on the focused text field.
        format!("{value}_")
    } else {
        value
    };

    Line::from(vec![
        Span::styled(format!("{prefix}{label:<9}"), Theme::label()),
        Span::styled(display, value_style),
    ])
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
    }
}

/// The selected Send target session, or a hint when none are running.
fn target_display(state: &AutomationEditorState<'_>) -> String {
    state
        .target_session
        .unwrap_or("(no running sessions)")
        .to_string()
}
