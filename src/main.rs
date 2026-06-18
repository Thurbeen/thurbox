use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind, KeyboardEnhancementFlags, MouseButton, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;

use thurbox::agent::tmux::{LocalTmuxBackend, TmuxBackend};
use thurbox::agent::{BackendRegistry, SessionBackend};
use thurbox::app::{App, AppMessage};
use thurbox::storage::Database;

/// Whether we pushed kitty keyboard-protocol flags onto the terminal. The
/// panic hook is installed before the push happens, so it reads this to know
/// whether a matching pop is needed.
static KEYBOARD_ENHANCEMENT_PUSHED: AtomicBool = AtomicBool::new(false);

/// Enable the kitty keyboard protocol where the terminal supports it: with
/// DISAMBIGUATE_ESCAPE_CODES, Cmd/Super-modified keys are reported at all
/// (otherwise the terminal never delivers them), while plain keys keep their
/// legacy encodings and no Release/Repeat events arrive — the
/// `KeyEventKind::Press` filter in `run_loop` stays correct. The support
/// query needs raw mode, so call this only after `ratatui::init()`.
fn push_keyboard_enhancement() {
    if matches!(
        crossterm::terminal::supports_keyboard_enhancement(),
        Ok(true)
    ) && execute!(
        std::io::stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
    .is_ok()
    {
        KEYBOARD_ENHANCEMENT_PUSHED.store(true, Ordering::SeqCst);
    }
}

/// Pop the kitty flags if (and only if) we pushed them — `ratatui::restore()`
/// does not, so both the shutdown path and the panic hook call this before
/// leaving raw mode.
fn pop_keyboard_enhancement() {
    if KEYBOARD_ENHANCEMENT_PUSHED.load(Ordering::SeqCst) {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Set up panic hook that restores terminal before printing the panic
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        pop_keyboard_enhancement();
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

    // Initialize session backends, load every config file, and open the DB.
    let (backends, agents, hosts, mut config_warnings) = init_backends_and_config()?;
    let db = open_database();
    activate_persisted_theme(&db);

    // Self-heal active extensions: re-create any session/automation a managed
    // extension declares but that has since been deleted. Runs before the
    // session restore below (so healed sessions are adopted like any other) and
    // before the TUI takes over the terminal (so tmux spawn output can't corrupt
    // it). Deleting an active extension's resources is therefore a no-op — they
    // come back; `thurbox-cli extension deactivate <name>` is the real off-switch.
    let heal_messages = thurbox::session_ops::heal_active_extensions(&db);
    for m in &heal_messages {
        tracing::info!("{m}");
    }
    config_warnings.extend(heal_messages);

    // Silent auto-update (opt-in via [features] auto_update). Runs here — after
    // config load, before the TUI takes the terminal — so download output can't
    // corrupt the alternate screen and the self-replace can't race the render.
    if let Some(msg) = maybe_auto_update() {
        tracing::info!("{msg}");
        eprintln!("{msg}");
        config_warnings.push(msg);
    }

    let mut terminal = ratatui::init();
    enable_terminal_features()?;
    push_keyboard_enhancement();
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

    arm_automation_heartbeat();

    let res = run_loop(&mut terminal, &mut app).await;

    app.shutdown();
    pop_keyboard_enhancement();
    execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture
    )?;
    ratatui::restore();

    res
}

/// Bring up the session backends and load every config file (settings, hosts,
/// agents, custom themes), publishing the process-wide state each reader needs.
/// Returns the backend registry, the agent registry, the host registry, and the
/// accumulated load warnings (logged here and later surfaced in the status bar).
#[allow(clippy::type_complexity)]
fn init_backends_and_config() -> Result<(
    BackendRegistry,
    thurbox::session::AgentRegistry,
    thurbox::session::HostRegistry,
    Vec<String>,
)> {
    // Initialize session backends and agent provider.
    let local_tmux: Arc<dyn SessionBackend> = Arc::new(LocalTmuxBackend::new());
    local_tmux.check_available()?;
    local_tmux.ensure_ready()?;
    let mut backends = BackendRegistry::new(local_tmux);

    // Load (or seed) the settings and publish them process-wide before
    // anything reads them (Database::open prunes the audit log; layout and
    // terminal wiring read breakpoints/scrollback).
    let (settings, mut config_warnings) =
        thurbox::agent::settings_config::load_or_seed_with_warnings();
    thurbox::session::settings::init(settings);

    // Register one SSH backend per configured remote host
    // (~/.config/thurbox/hosts.toml). These are registered lazily: a down or
    // slow host must not block TUI startup, so check_available()/ensure_ready()
    // are deferred to first spawn/restore (see App::backend_for).
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

    Ok((backends, agents, hosts, config_warnings))
}

/// Open the SQLite database for persistent state, falling back to the default
/// XDG location (dev vs. prod build) when the path can't be resolved.
fn open_database() -> Database {
    let db_path = thurbox::paths::database_file().unwrap_or_else(|| {
        let mut p = std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        p.push(if cfg!(dev_build) {
            ".local/share/thurbox-dev/thurbox.db"
        } else {
            ".local/share/thurbox/thurbox.db"
        });
        p
    });
    Database::open(&db_path).expect("Failed to open database")
}

/// Activate the persisted theme — built-in or custom — falling back to the
/// default when unset or unknown.
fn activate_persisted_theme(db: &Database) {
    if let Ok(Some(name)) = db.get_active_theme() {
        thurbox::ui::theme::apply_theme_by_name(&name);
    } else {
        thurbox::ui::theme::ensure_initialized();
    }
}

/// Enable bracketed paste and (opt-in via `[features] mouse`) mouse capture on
/// the terminal. Without mouse capture the terminal keeps its native mouse
/// behavior and no mouse events ever reach the app.
fn enable_terminal_features() -> Result<()> {
    if thurbox::session::settings::global().features.mouse {
        execute!(std::io::stdout(), EnableMouseCapture)?;
    }
    execute!(std::io::stdout(), EnableBracketedPaste)?;
    Ok(())
}

/// Arm the tmux heartbeat keeper so automations keep firing after the TUI is
/// closed (best-effort: a missing/old tmux just means TUI-only firing). Skipped
/// when the `automations` feature flag is off — `thurbox-cli automation create`
/// still arms it, since that's explicit user intent.
fn arm_automation_heartbeat() {
    if thurbox::session::settings::global().features.automations {
        let cli = thurbox::agent::tmux::resolve_cli_binary();
        if let Err(e) = thurbox::agent::tmux::ensure_automation_heartbeat(&cli) {
            tracing::warn!("Failed to arm automation heartbeat: {e}");
        }
    }
}

/// Silently auto-update the installed binaries when `[features] auto_update` is
/// on and this is not a dev build. Best-effort: any failure is logged and
/// startup continues on the current binary. Returns a one-line "updated" message
/// to surface in the status bar, or `None`.
///
/// Deliberately **not** gated on the version-check cache staleness: that cache is
/// shared with the `version_check` badge, which rewrites it (resetting the 24h
/// window) on every launch it refreshes. Gating auto-update on it let the badge
/// starve the updater — the cache was almost always "fresh", so `perform_update`
/// never ran. Instead we fetch on every launch (one cheap network call when the
/// feature is opted into); `perform_update(false)` short-circuits to `UpToDate`
/// after that single fetch when already current.
fn maybe_auto_update() -> Option<String> {
    let features = &thurbox::session::settings::global().features;
    if !features.auto_update {
        return None;
    }
    if thurbox::agent::extension_config::is_dev_build() {
        return None;
    }
    match thurbox::agent::self_update::perform_update(false) {
        Ok(outcome) => {
            // The update ran, so freshen the version-check cache too — the badge
            // stays accurate and reflects the newest release on the next launch.
            let _ = thurbox::agent::version_check::refresh_cache();
            match outcome {
                thurbox::agent::self_update::UpdateOutcome::Updated { to, .. } => {
                    Some(format!("Updated to v{to} — restart thurbox to apply."))
                }
                _ => None,
            }
        }
        Err(e) => {
            tracing::warn!("auto-update failed: {e}");
            None
        }
    }
}

async fn run_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| app.view(f))?;

        if event::poll(Duration::from_millis(10))? {
            if let Some(msg) = event_to_message(event::read()?) {
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

/// Translate a crossterm `Event` into the matching `AppMessage`, or `None` for
/// events the app ignores (key release/repeat, unhandled mouse kinds, …).
fn event_to_message(event: Event) -> Option<AppMessage> {
    match event {
        Event::Key(k) if k.kind == KeyEventKind::Press => {
            Some(AppMessage::KeyPress(k.code, k.modifiers))
        }
        Event::Mouse(m) => mouse_to_message(m),
        Event::Paste(text) => Some(AppMessage::Paste(text)),
        Event::Resize(cols, rows) => Some(AppMessage::Resize(cols, rows)),
        _ => None,
    }
}

/// Translate a crossterm mouse event into the matching `AppMessage`, or `None`
/// for mouse kinds the app does not handle.
fn mouse_to_message(m: event::MouseEvent) -> Option<AppMessage> {
    match m.kind {
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
        MouseEventKind::Moved => Some(AppMessage::MouseMove {
            x: m.column,
            y: m.row,
        }),
        _ => None,
    }
}
