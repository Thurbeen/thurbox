use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::centered_fixed_height_rect;
use super::render_modal_frame_danger;
use super::theme::Theme;
use crate::app::modals::DeleteRisk;

const MODAL_WIDTH_PCT: u16 = 60;

pub struct ConfirmDeleteState<'a> {
    pub session_name: &'a str,
    pub risk: &'a DeleteRisk,
}

/// `"{n} {noun}"`, pluralized (`1 file`, `3 files`).
fn pluralize(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// One muted `•` bullet whose leading count is bold, e.g. `  • 3 modified
/// files (+40 / -7)`. `detail` is appended muted (for the `+/-` line stats).
fn bullet(text: String, detail: Option<String>) -> Line<'static> {
    let mut spans = vec![
        Span::styled("  • ", Style::default().fg(Theme::text_muted())),
        Span::styled(text, Style::default().add_modifier(Modifier::BOLD)),
    ];
    if let Some(detail) = detail {
        spans.push(Span::styled(
            format!(" {detail}"),
            Style::default().fg(Theme::text_muted()),
        ));
    }
    Line::from(spans)
}

/// The "what will be lost" block: an unverifiable state collapses to a single
/// line; otherwise a header plus one bullet per risk category (modified /
/// untracked files, unmerged commits). Never empty — a risk only reaches the
/// modal when it has something to report.
fn risk_lines(risk: &DeleteRisk) -> Vec<Line<'static>> {
    if risk.unknown {
        return vec![Line::from(Span::styled(
            "Could not verify this session's worktree state.",
            Style::default().fg(Theme::danger()),
        ))];
    }

    let mut lines = vec![Line::from(Span::styled(
        "This session has unsaved work:",
        Style::default().fg(Theme::danger()),
    ))];
    if risk.files_changed > 0 {
        lines.push(bullet(
            pluralize(risk.files_changed, "modified file"),
            Some(format!("(+{} / -{})", risk.insertions, risk.deletions)),
        ));
    }
    if risk.untracked > 0 {
        lines.push(bullet(pluralize(risk.untracked, "untracked file"), None));
    }
    if risk.ahead > 0 {
        lines.push(bullet(pluralize(risk.ahead, "unmerged commit"), None));
    }
    // Fallback: dirty in a way none of the counts caught (e.g. a lone mode
    // change) — still tell the user something is uncommitted.
    if lines.len() == 1 {
        lines.push(bullet("uncommitted changes".to_string(), None));
    }
    lines
}

/// Render the hard-delete confirmation prompt. Shown only when the
/// `soft_delete` feature flag is off and the session has work at risk, where a
/// delete is irreversible (the tmux window + worktrees are torn down with no
/// Ctrl+Z undo), so it warns — and itemizes what would be lost — before acting.
pub fn render_confirm_delete_modal(
    frame: &mut Frame,
    state: &ConfirmDeleteState<'_>,
) -> super::ModalButtons {
    // Body, top to bottom: prompt, blank, risk block, blank, warning. The
    // footer is laid out separately below.
    let mut body: Vec<Line> = vec![
        Line::from(vec![
            Span::raw("Permanently delete "),
            Span::styled(
                format!("'{}'", state.session_name),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("?"),
        ]),
        Line::from(""),
    ];
    body.extend(risk_lines(state.risk));
    body.push(Line::from(""));
    body.push(Line::from(Span::styled(
        "This kills the tmux window and removes its worktrees — cannot be undone.",
        Style::default().fg(Theme::danger()),
    )));

    // Outer height: the body lines + a blank spacer + the footer row (2), plus
    // the top/bottom border (2).
    let height = body.len() as u16 + 2 + 2;
    let area = centered_fixed_height_rect(MODAL_WIDTH_PCT, height, frame.area());
    let inner = render_modal_frame_danger(frame, area, "Delete session");

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(body.len() as u16),
            Constraint::Length(1), // blank spacer
            Constraint::Length(1), // footer
        ])
        .split(inner);

    frame.render_widget(Paragraph::new(body), chunks[0]);

    super::render_action_footer(
        frame,
        chunks[2],
        (
            "Delete",
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ),
        "Cancel",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flatten a line's spans into its plain text.
    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn lines_text(risk: &DeleteRisk) -> String {
        risk_lines(risk)
            .iter()
            .map(text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn pluralize_handles_singular_and_plural() {
        assert_eq!(pluralize(1, "file"), "1 file");
        assert_eq!(pluralize(0, "file"), "0 files");
        assert_eq!(pluralize(3, "commit"), "3 commits");
    }

    #[test]
    fn risk_lines_unknown_is_single_line() {
        let lines = risk_lines(&DeleteRisk::unknown());
        assert_eq!(lines.len(), 1);
        assert!(text(&lines[0]).contains("Could not verify"));
    }

    #[test]
    fn risk_lines_itemizes_each_category() {
        let risk = DeleteRisk {
            dirty: true,
            files_changed: 3,
            insertions: 40,
            deletions: 7,
            untracked: 2,
            ahead: 1,
            unknown: false,
        };
        let out = lines_text(&risk);
        assert!(out.contains("3 modified files (+40 / -7)"), "{out}");
        assert!(out.contains("2 untracked files"), "{out}");
        assert!(out.contains("1 unmerged commit"), "{out}");
        // header + 3 bullets
        assert_eq!(risk_lines(&risk).len(), 4);
    }

    #[test]
    fn risk_lines_dirty_without_counts_uses_fallback() {
        // dirty but no diff/untracked/ahead (e.g. a lone mode change): the user
        // still gets a "uncommitted changes" bullet, never a lone header.
        let risk = DeleteRisk {
            dirty: true,
            ..Default::default()
        };
        let lines = risk_lines(&risk);
        assert_eq!(lines.len(), 2);
        assert!(text(&lines[1]).contains("uncommitted changes"));
    }
}
