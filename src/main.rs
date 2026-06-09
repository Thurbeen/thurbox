use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind, MouseButton, MouseEventKind,
};
use crossterm::execute;

use thurbox::agent::tmux::{LocalTmuxBackend, TmuxBackend};
use thurbox::agent::{BackendRegistry, SessionBackend};
use thurbox::app::{App, AppMessage};
use thurbox::storage::Database;

#[tokio::main]
async fn main() -> Result<()> {
    // Set up panic hook that restores terminal before printing the panic
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = execute!(
            std::io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture
        );
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

    // Initialize session backends and agent provider.
    let local_tmux: Arc<dyn SessionBackend> = Arc::new(LocalTmuxBackend::new());
    local_tmux.check_available()?;
    local_tmux.ensure_ready()?;
    let mut backends = BackendRegistry::new(local_tmux);

    // Register one SSH backend per configured remote host
    // (~/.config/thurbox/hosts.toml). These are registered lazily: a down or
    // slow host must not block TUI startup, so check_available()/ensure_ready()
    // are deferred to first spawn/restore (see App::backend_for).
    // Load (or seed) the scalar settings and publish them process-wide before
    // anything reads them (Database::open prunes the audit log; layout and
    // terminal wiring read breakpoints/scrollback).
    let (settings, mut config_warnings) =
        thurbox::agent::settings_config::load_or_seed_with_warnings();
    thurbox::session::settings::init(settings);

    let (hosts, host_warnings) = thurbox::agent::host_config::load_or_seed_with_warnings();
    config_warnings.extend(host_warnings);
    for host in &hosts.hosts {
        tracing::debug!(host = %host.name, dest = %host.destination, "Registering SSH backend");
        backends.register(Arc::new(TmuxBackend::from_host(host)));
    }

    // Load (or seed) the coding-agent registry from ~/.config/thurbox/agents.toml.
    let (agents, agent_warnings) = thurbox::agent::agent_config::load_or_seed_with_warnings();
    config_warnings.extend(agent_warnings);

    // Load (or seed) custom themes and publish them so the picker and the
    // persisted-theme lookup below can resolve them by name.
    let (custom_themes, theme_warnings) =
        thurbox::agent::themes_config::load_or_seed_with_warnings();
    config_warnings.extend(theme_warnings);
    thurbox::ui::theme::set_custom_themes(custom_themes);

    for w in &config_warnings {
        tracing::warn!("{w}");
    }

    // Open SQLite database for persistent state
    let db_path = thurbox::paths::database_file().unwrap_or_else(|| {
        let mut p = std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        p.push(if cfg!(dev_build) {
            ".local/share/thurbox-dev/thurbox.db"
        } else {
            ".local/share/thurbox/thurbox.db"
        });
        p
    });
    let db = Database::open(&db_path).expect("Failed to open database");

    // Activate the persisted theme — built-in or custom — falling back to
    // default when unset/unknown.
    if let Ok(Some(name)) = db.get_active_theme() {
        thurbox::ui::theme::apply_theme_by_name(&name);
    } else {
        thurbox::ui::theme::ensure_initialized();
    }

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let size = terminal.size()?;

    let mut app = App::new(size.height, size.width, backends, agents, db);
    app.set_hosts(hosts);
    // Surface agents.toml/hosts.toml load problems in the status bar — the
    // tracing::warn above only reaches the log file the TUI hides.
    app.report_config_warnings(config_warnings);

    // Load session state from DB and restore
    if let Some((sessions, counter)) = app.load_persisted_state_from_db() {
        app.restore_sessions(sessions, counter);
    }

    // Arm the tmux heartbeat keeper so automations keep firing after the TUI is
    // closed (best-effort: a missing/old tmux just means TUI-only firing).
    {
        let cli = thurbox::agent::tmux::resolve_cli_binary();
        if let Err(e) = thurbox::agent::tmux::ensure_automation_heartbeat(&cli) {
            tracing::warn!("Failed to arm automation heartbeat: {e}");
        }
    }

    let res = run_loop(&mut terminal, &mut app).await;

    app.shutdown();
    execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture
    )?;
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
                    MouseEventKind::ScrollUp => Some(AppMessage::MouseScrollUp {
                        x: m.column,
                        y: m.row,
                    }),
                    MouseEventKind::ScrollDown => Some(AppMessage::MouseScrollDown {
                        x: m.column,
                        y: m.row,
                    }),
                    MouseEventKind::Down(MouseButton::Left) => Some(AppMessage::MouseClick {
                        x: m.column,
                        y: m.row,
                        modifiers: m.modifiers,
                    }),
                    MouseEventKind::Drag(MouseButton::Left) => Some(AppMessage::MouseDrag {
                        x: m.column,
                        y: m.row,
                    }),
                    MouseEventKind::Up(MouseButton::Left) => Some(AppMessage::MouseUp {
                        x: m.column,
                        y: m.row,
                    }),
                    _ => None,
                },
                Event::Paste(text) => Some(AppMessage::Paste(text)),
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
