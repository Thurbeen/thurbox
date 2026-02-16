use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseEventKind,
};
use crossterm::execute;

use thurbox::app::{App, AppMessage};
use thurbox::claude::tmux::LocalTmuxBackend;
use thurbox::claude::SessionBackend;
use thurbox::project;

#[tokio::main]
async fn main() -> Result<()> {
    // Set up panic hook that restores terminal before printing the panic
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        ratatui::restore();
        original_hook(panic_info);
    }));

    // File-based logging (stdout is owned by the TUI)
    let log_dir = thurbox::paths::log_directory().unwrap_or_else(|| std::path::PathBuf::from("."));
    std::fs::create_dir_all(&log_dir).ok();
    let file_appender = tracing_appender::rolling::daily(log_dir, "thurbox.log");
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("thurbox=debug".parse().unwrap()),
        )
        .with_writer(file_appender)
        .with_ansi(false)
        .init();

    // Initialize the session backend (local tmux).
    let backend: Arc<dyn SessionBackend> = Arc::new(LocalTmuxBackend::new());
    backend.check_available()?;
    backend.ensure_ready()?;

    let project_configs = project::load_project_configs();

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let size = terminal.size()?;

    let mut app = App::new(size.height, size.width, project_configs, backend);

    let state = project::load_session_state();
    if state.sessions.is_empty() {
        app.spawn_session();
    } else {
        app.restore_sessions(state);
    }

    let res = run_loop(&mut terminal, &mut app).await;

    app.shutdown();
    execute!(std::io::stdout(), DisableMouseCapture)?;
    ratatui::restore();

    res
}

async fn run_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| app.view(f))?;

        if event::poll(Duration::from_millis(10))? {
            let msg = match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    Some(AppMessage::KeyPress(k.code, k.modifiers))
                }
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::ScrollUp => Some(AppMessage::MouseScrollUp),
                    MouseEventKind::ScrollDown => Some(AppMessage::MouseScrollDown),
                    _ => None,
                },
                Event::Resize(cols, rows) => Some(AppMessage::Resize(cols, rows)),
                _ => None,
            };
            if let Some(msg) = msg {
                app.update(msg);
            }
        }

        app.tick();

        if app.should_quit() {
            break;
        }
    }

    Ok(())
}
