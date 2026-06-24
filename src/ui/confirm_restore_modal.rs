use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
    Frame,
};

use super::theme::Theme;

const MODAL_WIDTH_PCT: u16 = 60;

pub struct ConfirmRestoreState<'a> {
    pub session_name: &'a str,
}

/// Render the best-effort restore confirmation prompt for a force-deleted
/// session. Force-delete removed the worktree directory + tmux window (and any
/// uncommitted work), but the git branch and its committed history survive, so
/// restore reattaches what's left. This warns that only committed state comes
/// back before acting.
pub fn render_confirm_restore_modal(
    frame: &mut Frame,
    state: &ConfirmRestoreState<'_>,
) -> super::ModalButtons {
    let body: Vec<Line> = vec![
        Line::from(vec![
            Span::raw("Restore force-deleted "),
            Span::styled(
                format!("'{}'", state.session_name),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("?"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "This is best-effort: committed work on the branch is reattached,",
            Style::default().fg(Theme::text_secondary()),
        )),
        Line::from(Span::styled(
            "but uncommitted and untracked changes were lost on delete.",
            Style::default().fg(Theme::danger()),
        )),
    ];

    super::render_confirm_modal(
        frame,
        MODAL_WIDTH_PCT,
        "Restore session",
        false,
        body,
        (
            "Restore",
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn rendered_text(name: &str) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_confirm_restore_modal(frame, &ConfirmRestoreState { session_name: name });
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn warns_about_lost_work_and_shows_footer() {
        let out = rendered_text("my-session");
        assert!(out.contains("my-session"), "{out}");
        assert!(out.contains("best-effort"), "{out}");
        assert!(out.contains("uncommitted"), "{out}");
        assert!(out.contains("Restore"), "{out}");
        assert!(out.contains("Cancel"), "{out}");
    }
}
