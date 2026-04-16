use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind, MouseButton, MouseEventKind,
};
use crossterm::execute;

use thurbox::agent::claude::ClaudeProvider;
use thurbox::agent::tmux::LocalTmuxBackend;
use thurbox::agent::{
    detect_runtime, AgentProvider, BackendRegistry, ContainerManager, DevcontainerBackend,
    QemuVmBackend, SessionBackend, VmManager,
};
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
    let provider: Arc<dyn AgentProvider> = Arc::new(ClaudeProvider);

    // Register QEMU VM backend if available (optional — requires qemu-system-x86_64 + /dev/kvm).
    let data_dir = thurbox::paths::log_directory().unwrap_or_else(|| std::path::PathBuf::from("."));
    let vm_manager = Arc::new(std::sync::Mutex::new(VmManager::new(&data_dir)));
    let vm_backend: Arc<dyn SessionBackend> = Arc::new(QemuVmBackend::new(Arc::clone(&vm_manager)));
    let mut vm_manager_for_app = None;
    if vm_backend.check_available().is_ok() {
        if let Err(e) = vm_backend.ensure_ready() {
            tracing::warn!("QEMU VM backend available but setup failed: {e}");
        } else {
            tracing::info!("QEMU VM backend registered");
            backends.register(vm_backend);
            vm_manager_for_app = Some(vm_manager);
        }
    } else {
        tracing::debug!("QEMU VM backend not available (qemu-system-x86_64 not found or /dev/kvm not accessible)");
    }

    // Register container backend if available (optional — requires Docker or Podman).
    let mut container_manager_for_app = None;
    match detect_runtime() {
        Ok(runtime) => {
            let containerfiles_dir = thurbox::paths::containerfiles_directory()
                .unwrap_or_else(|| data_dir.join("containerfiles"));
            let container_manager = Arc::new(std::sync::Mutex::new(ContainerManager::new(
                &data_dir,
                containerfiles_dir,
                runtime,
            )));
            let dc_backend: Arc<dyn SessionBackend> = Arc::new(DevcontainerBackend::new(
                Arc::clone(&container_manager),
                runtime,
            ));
            if dc_backend.check_available().is_ok() {
                if let Err(e) = dc_backend.ensure_ready() {
                    tracing::warn!("Container backend available but setup failed: {e}");
                } else {
                    tracing::info!("Container backend registered (runtime: {runtime})");
                    backends.register(dc_backend);
                    container_manager_for_app = Some(container_manager);
                }
            }
        }
        Err(_) => {
            tracing::debug!("Container backend not available (no Docker or Podman found)");
        }
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

    // Activate the persisted theme (falls back to default if unset/unknown).
    if let Ok(Some(name)) = db.get_active_theme() {
        thurbox::ui::theme::apply_preset_by_name(&name);
    } else {
        thurbox::ui::theme::ensure_initialized();
    }

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let size = terminal.size()?;

    let mut app = App::new(
        size.height,
        size.width,
        backends,
        provider,
        db,
        vm_manager_for_app,
        container_manager_for_app,
    );

    // Ensure containerfile templates directory exists and is seeded
    app.ensure_containerfiles_dir();

    // Load session state from DB and restore
    if let Some((sessions, counter)) = app.load_persisted_state_from_db() {
        app.restore_sessions(sessions, counter);
    }

    // Ensure admin project exists and .mcp.json is up to date
    app.ensure_admin_setup();

    // Write the statusline script; the per-session statusLine config is
    // injected by skill_staging::prepare into each session's CLAUDE_CONFIG_DIR.
    app.ensure_statusline_script();

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
                    MouseEventKind::ScrollUp => Some(AppMessage::MouseScrollUp),
                    MouseEventKind::ScrollDown => Some(AppMessage::MouseScrollDown),
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
