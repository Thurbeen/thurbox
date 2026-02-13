use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind};

use thurbox::app::{App, AppMessage};
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
    let log_dir = log_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let file_appender = tracing_appender::rolling::daily(log_dir, "thurbox.log");
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("thurbox=debug".parse().unwrap()),
        )
        .with_writer(file_appender)
        .with_ansi(false)
        .init();

    let project_configs = project::load_project_configs();

    let mut terminal = ratatui::init();
    let size = terminal.size()?;

    let mut app = App::new(size.height, size.width, project_configs);
    app.spawn_session();

    let res = run_loop(&mut terminal, &mut app).await;

    app.shutdown();
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

/// Return the data-local directory for log files.
fn log_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| {
        let mut p = std::path::PathBuf::from(h);
        p.push(".local");
        p.push("share");
        p.push("thurbox");
        std::fs::create_dir_all(&p).ok();
        p
    })
}
