use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    widgets::{Block, Borders, Paragraph},
    DefaultTerminal, Frame,
};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let mut terminal = ratatui::init();
    let res = run_app(&mut terminal);
    ratatui::restore();

    res
}

fn run_app(terminal: &mut DefaultTerminal) -> Result<()> {
    loop {
        terminal.draw(render)?;

        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press && should_quit(key) {
                return Ok(());
            }
        }
    }
}

fn render(frame: &mut Frame) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let title = Paragraph::new("Welcome to Thurbox!")
        .block(Block::default().borders(Borders::ALL).title("Thurbox"));
    frame.render_widget(title, chunks[0]);

    let content = Paragraph::new("Press 'q' to quit")
        .block(Block::default().borders(Borders::ALL).title("Main"));
    frame.render_widget(content, chunks[1]);

    let footer = Paragraph::new("Status: Ready").block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, chunks[2]);
}

fn should_quit(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_keys_return_true() {
        assert!(should_quit(KeyEvent::from(KeyCode::Char('q'))));
        assert!(should_quit(KeyEvent::from(KeyCode::Esc)));
    }

    #[test]
    fn other_keys_return_false() {
        assert!(!should_quit(KeyEvent::from(KeyCode::Char('a'))));
        assert!(!should_quit(KeyEvent::from(KeyCode::Char('Q'))));
        assert!(!should_quit(KeyEvent::from(KeyCode::Enter)));
        assert!(!should_quit(KeyEvent::from(KeyCode::Backspace)));
    }
}
