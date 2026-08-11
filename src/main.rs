use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
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
    // `swap` so a second restore (guard drop after an explicit restore, or the
    // panic hook racing the guard) can't pop a level we never pushed.
    if KEYBOARD_ENHANCEMENT_PUSHED.swap(false, Ordering::SeqCst) {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
}

/// Undo every terminal mutation we made on startup, in reverse order: pop the
/// kitty flags, disable bracketed paste + mouse capture, then leave the
/// alternate screen / raw mode (`ratatui::restore()`). Idempotent, so the
/// callers can safely overlap: the normal quit path calls it explicitly (before
/// the slow session detach) *and* again via the `TerminalGuard` drop, and the
/// panic hook may race that guard on unwind. The single source of truth shared
/// by all three so the restore paths can't drift.
fn restore_terminal() {
    pop_keyboard_enhancement();
    let _ = execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture
    );
    ratatui::restore();
}

/// RAII guard that restores the terminal when it drops. Held from just after
/// `ratatui::init()`, it covers the **error paths too**: a `?` failure during
/// terminal setup or `run_loop` returns through this drop, so the user's shell
/// is never left in raw mode (which would garble it) before the error is
/// printed. The panic hook restores independently; a double restore on unwind
/// is harmless.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Re-establish the TUI's terminal mutations after a suspend, mirroring the
/// startup sequence in `main` (alt screen + raw mode first, then the optional
/// kitty flags, bracketed paste, and mouse capture). The inverse of
/// [`restore_terminal`], used by [`run_pending_editor`]'s suspend path so the
/// editor's exit leaves the TUI clean.
fn resume_terminal() {
    use crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
    let _ = execute!(std::io::stdout(), EnterAlternateScreen);
    let _ = enable_raw_mode();
    push_keyboard_enhancement();
    let _ = execute!(std::io::stdout(), EnableBracketedPaste);
    if thurbox::session::settings::global().features.mouse {
        let _ = execute!(std::io::stdout(), EnableMouseCapture);
    }
}

/// Run a terminal editor staged by `Ctrl+O`, giving it a real controlling
/// TTY. Inside tmux (`$TMUX` set, on a tmux ≥ 3.2 that has `display-popup`)
/// the editor floats in a `tmux display-popup` with its own PTY — the TUI
/// keeps its state underneath and the popup closes on editor exit. Otherwise
/// the TUI is suspended (alt screen/raw mode/mouse torn down), the editor
/// inherits the terminal, and the TUI resumes on exit — the git/sudoedit
/// pattern. Either way the render loop is blocked for the editor's lifetime,
/// which is correct (the user is editing).
fn run_pending_editor(
    terminal: &mut ratatui::DefaultTerminal,
    inv: &thurbox::app::EditorInvocation,
) -> Result<()> {
    let use_popup = std::env::var_os("TMUX").is_some() && outer_tmux_supports_popup();
    let popup_ok = if use_popup {
        match run_editor_popup(inv) {
            Ok(()) => true,
            Err(e) => {
                // `tmux` itself wouldn't spawn (not on PATH, etc.) — fall back
                // to suspend rather than doing nothing.
                tracing::warn!("tmux popup unavailable, suspending TUI instead: {e}");
                false
            }
        }
    } else {
        false
    };
    if !popup_ok {
        run_editor_suspended(terminal, inv)?;
    }
    Ok(())
}

/// Cached check that the *outer* tmux (the one `$TMUX` points at, whose client
/// owns this process's terminal) supports `display-popup` (tmux ≥ 3.2). `tmux
/// -V` prints the binary version, which matches the running server (a tmux
/// client must match its server), so this is a reliable one-shot gate. Returns
/// `false` when not inside tmux or the version can't be read.
static OUTER_TMUX_POPUP_OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
fn outer_tmux_supports_popup() -> bool {
    *OUTER_TMUX_POPUP_OK.get_or_init(|| {
        let output = match std::process::Command::new("tmux").arg("-V").output() {
            Ok(o) => o,
            Err(_) => return false,
        };
        let v = String::from_utf8_lossy(&output.stdout);
        // `tmux 3.4` → parse major.minor.
        let nums: Vec<u32> = v
            .split_whitespace()
            .nth(1)
            .map(|s| s.split('.').filter_map(|n| n.parse().ok()).collect())
            .unwrap_or_default();
        matches!(nums.as_slice(), [major, minor, ..] if (*major, *minor) >= (3, 2))
    })
}

/// Float the editor in a `tmux display-popup` over the TUI. The popup gets its
/// own PTY (the editor's termios init is independent of thurbox's), and `-E`
/// closes it on editor exit. Returns `Ok(())` whenever tmux actually launched
/// the command — the editor's own exit code is ignored (a nonzero editor exit
/// must NOT trigger a fallback that would re-launch it). The command string is
/// POSIX-quoted and run via tmux's shell, so paths/flags with spaces survive.
/// Only called after [`outer_tmux_supports_popup`].
fn run_editor_popup(inv: &thurbox::app::EditorInvocation) -> std::io::Result<()> {
    let mut shell_cmd = String::new();
    shell_cmd.push_str(&thurbox::shell::posix_quote(&inv.program));
    for arg in &inv.args {
        shell_cmd.push(' ');
        shell_cmd.push_str(&thurbox::shell::posix_quote(arg));
    }
    // `-E` closes the popup on command exit; sized to 90% so the editor gets
    // most of the screen while the TUI border stays visible.
    let _status = std::process::Command::new("tmux")
        .args([
            "display-popup",
            "-E",
            "-w",
            "90%",
            "-h",
            "90%",
            "-T",
            "thurbox editor",
        ])
        .arg(&shell_cmd)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()?;
    Ok(())
}

/// Suspend the TUI and run the editor with the inherited terminal (the
/// git/sudoedit pattern). The editor controls termios for the duration; on its
/// exit the TUI's terminal mutations are re-established and the frame buffer
/// cleared so the next `draw` repaints from scratch.
fn run_editor_suspended(
    terminal: &mut ratatui::DefaultTerminal,
    inv: &thurbox::app::EditorInvocation,
) -> Result<()> {
    // Tear down everything `main` set up (the same path as quit), so the
    // editor owns a normal cooked terminal — it then puts the terminal into its
    // own raw mode for the duration of editing.
    restore_terminal();
    let result = std::process::Command::new(&inv.program)
        .args(&inv.args)
        .status();
    resume_terminal();
    // Drop the stale frame buffer so the next paint is full-screen, not a diff
    // against the pre-suspend frame (which the editor overwrote).
    let _ = terminal.clear();
    let status = result?;
    if !status.success() {
        tracing::info!(
            program = %inv.program,
            code = ?status.code(),
            "editor exited non-zero"
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Process start, for the opt-in time-to-first-frame measurement (logged
    // once by `run_loop` when `THURBOX_PERF_LOG` is set). Captured first so it
    // covers config load, DB open, and session restore.
    let process_start = std::time::Instant::now();

    // Restore the terminal before the panic message prints (else it garbles).
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        restore_terminal();
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

    // Coarse, always-cheap startup phase marks (a handful of one-shot
    // `Instant::now()` calls, never in a loop). The breakdown is only *emitted*
    // when THURBOX_PERF_LOG is set; capturing it unconditionally keeps the code
    // simple at no measurable cost. See docs/PERFORMANCE.md (ADR-P5).
    let mut startup = StartupTimings::default();

    // Initialize session backends, load every config file, and open the DB.
    // `agents` is reloaded after the extension heal below (which may patch
    // agents.toml), so the initial copy here is only used for its warnings.
    let t_phase = std::time::Instant::now();
    let (backends, _agents, hosts, mut config_warnings) = init_backends_and_config()?;
    startup.config_init_ms = t_phase.elapsed().as_millis();

    let t_phase = std::time::Instant::now();
    let db = open_database()?;
    startup.db_open_ms = t_phase.elapsed().as_millis();

    let t_phase = std::time::Instant::now();
    activate_persisted_theme(&db);
    startup.theme_activate_ms = t_phase.elapsed().as_millis();

    let t_phase = std::time::Instant::now();

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

    // Auto-activate the built-in `hooks` extension so the default agent reports
    // its lifecycle state out of the box (working/blocked/done). Idempotent;
    // re-applies the agent hook wiring on every launch. Opt out with
    // `thurbox-cli extension deactivate hooks`.
    let hook_messages = thurbox::session_ops::ensure_builtin_hooks_extension(&db);
    for m in &hook_messages {
        tracing::info!("{m}");
    }
    // Surface hook-wiring outcomes in the status bar too (not just the log) so a
    // wiring failure is visible. Idempotent, so this is non-empty only on the
    // first wire-up of a profile or on an error — never noisy per launch.
    config_warnings.extend(hook_messages);
    // The hooks extension (and any other extension heal above) may have just
    // patched `agents.toml` on disk — e.g. injecting `--settings <hooks>` into
    // the `claude` agent so its lifecycle hooks fire. Reload the registry so the
    // in-memory copy App spawns from reflects that on the *first* run too
    // (otherwise a freshly-seeded profile would spawn agents without their hooks
    // and statuses would be stuck until the next launch).
    let agents = thurbox::agent::agent_config::load_or_seed();
    startup.extension_heal_ms = t_phase.elapsed().as_millis();

    // Silent auto-update (on by default for 1.0 via [features] auto_update). Kicked off on a
    // background thread *before* the TUI starts so a slow download never blocks
    // the first frame — the result is surfaced as a status toast once it lands
    // (App::poll_auto_update). The self-replace only swaps the on-disk binaries
    // (atomic renames); the running process is untouched, so it never races the
    // render, and the new version applies on the next launch.
    let auto_update_rx = spawn_auto_update();

    let mut terminal = ratatui::init();
    // Restore the terminal on every exit from here on — including an early `?`
    // return from terminal setup or `run_loop` — so a startup error can't leave
    // the shell in raw mode. Declared after `terminal` so it drops first.
    let _terminal_guard = TerminalGuard;
    enable_terminal_features()?;
    push_keyboard_enhancement();
    let size = terminal.size()?;

    let t_phase = std::time::Instant::now();
    let mut app = App::new(size.height, size.width, backends, agents, db);
    startup.app_new_ms = t_phase.elapsed().as_millis();
    app.set_hosts(hosts);
    if let Some(rx) = auto_update_rx {
        app.set_auto_update_receiver(rx);
    }
    // Surface agents.toml/hosts.toml load problems in the status bar — the
    // tracing::warn above only reaches the log file the TUI hides.
    app.report_config_warnings(config_warnings);

    let t_phase = std::time::Instant::now();
    if let Some((sessions, counter)) = app.load_persisted_state_from_db() {
        app.restore_sessions(sessions, counter);
    }
    startup.restore_ms = t_phase.elapsed().as_millis();

    let t_phase = std::time::Instant::now();
    arm_automation_heartbeat();
    startup.heartbeat_ms = t_phase.elapsed().as_millis();

    // Hand the phase breakdown to the app so the published perf snapshot
    // (`thurbox-cli perf`) can show boot cost alongside the runtime stats.
    app.set_startup_phases(startup.as_json());

    let res = run_loop(&mut terminal, &mut app, process_start, startup).await;

    // Restore the terminal *before* the (potentially slow) session detach:
    // `shutdown()` detaches tmux/SSH sessions, and while it runs the event loop
    // is no longer draining stdin. With mouse capture still on, any mouse motion
    // in that window queues SGR reports (`ESC[<b;x;yM`) in the tty buffer that
    // the shell then echoes as `51;82;30M`-style garbage once thurbox exits.
    // `restore_terminal` is idempotent, so the `_terminal_guard` drop below (and
    // early-error returns) still restore correctly.
    restore_terminal();
    app.shutdown();
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

    // Register one backend per off-local host: each configured SSH host in
    // ~/.config/thurbox/hosts.toml, plus every auto-discovered local WSL distro
    // (`wsl.exe -l -q`, Windows only). These are registered lazily: a down or
    // slow host must not block TUI startup, so check_available()/ensure_ready()
    // are deferred to first spawn/restore (see App::backend_for).
    let (hosts, host_warnings) = thurbox::agent::host_config::load_all_with_warnings();
    config_warnings.extend(host_warnings);
    for host in &hosts.hosts {
        tracing::debug!(host = %host.name, backend = %host.backend_name(), "Registering backend");
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
fn open_database() -> Result<Database> {
    let db_path = thurbox::paths::database_file().unwrap_or_else(fallback_database_path);
    Database::open(&db_path)
        .with_context(|| format!("failed to open database at {}", db_path.display()))
}

/// Degenerate-fallback database path, used only when
/// `paths::database_file()` couldn't resolve the platform data dir. Mirrors
/// `paths::data_base()` so the override still wins here: `$XDG_DATA_HOME` is
/// honored first on every platform (the previous fallback ignored it and jumped
/// straight to the home layout), otherwise we anchor under the platform-native
/// home data layout (`%USERPROFILE%\AppData\Local` on Windows,
/// `$HOME/.local/share` on Unix).
fn fallback_database_path() -> std::path::PathBuf {
    let app = if cfg!(dev_build) {
        "thurbox-dev"
    } else {
        "thurbox"
    };
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            #[cfg(windows)]
            {
                let mut p =
                    std::path::PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default());
                p.push("AppData");
                p.push("Local");
                p
            }
            #[cfg(not(windows))]
            {
                let mut p = thurbox::paths::home_dir().unwrap_or_default();
                p.push(".local");
                p.push("share");
                p
            }
        });
    base.join(app).join("thurbox.db")
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
/// on and this is not a dev build, **on a background thread** so the network
/// fetch + download never delays TUI startup. Returns the receiving end of a
/// one-shot channel that yields a "Updated …" message when binaries were
/// actually replaced (drained by `App::poll_auto_update`), or `None` when no
/// thread was spawned (feature off / dev build). Best-effort: any failure is
/// logged and the TUI keeps running on the current binary.
///
/// The on-disk swap is atomic renames against the install dir; the running
/// process keeps its already-loaded image, so doing this concurrently with the
/// render loop is safe — the new version applies on the next launch.
///
/// Deliberately **not** gated on the version-check cache staleness: that cache is
/// shared with the `version_check` badge, which rewrites it (resetting the 24h
/// window) on every launch it refreshes. Gating auto-update on it let the badge
/// starve the updater — the cache was almost always "fresh", so `perform_update`
/// never ran. Instead we fetch on every launch (one cheap network call when the
/// feature is opted into); `perform_update(false)` short-circuits to `UpToDate`
/// after that single fetch when already current.
fn spawn_auto_update() -> Option<std::sync::mpsc::Receiver<String>> {
    let features = &thurbox::session::settings::global().features;
    if !features.auto_update {
        return None;
    }
    if thurbox::agent::extension_config::is_dev_build() {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || run_auto_update(&tx));
    Some(rx)
}

/// The background auto-update worker: download + install the latest release,
/// freshen the version-check cache, and report an "Updated …" message over `tx`
/// only when binaries were actually replaced. Best-effort — failures are logged
/// and swallowed. A send error means the TUI already exited, so there is nothing
/// to surface; it is ignored.
fn run_auto_update(tx: &std::sync::mpsc::Sender<String>) {
    match thurbox::agent::self_update::perform_update(false) {
        Ok(outcome) => {
            // The update ran, so freshen the version-check cache too — the badge
            // stays accurate and reflects the newest release on the next launch.
            let _ = thurbox::agent::version_check::refresh_cache();
            if let thurbox::agent::self_update::UpdateOutcome::Updated { to, .. } = outcome {
                let msg = format!("Updated to v{to} — restart thurbox to apply.");
                tracing::info!("{msg}");
                let _ = tx.send(msg);
            }
        }
        Err(e) => tracing::warn!("auto-update failed: {e}"),
    }
}

/// Coarse one-shot startup phase durations (milliseconds), captured in `main`
/// and logged once after the first paint when `THURBOX_PERF_LOG` is set. The
/// phases sum to roughly `first_frame_ms`, so a slow boot can be attributed to
/// config/backend init, DB open, extension heal, or session restore rather than
/// guessed at. See docs/PERFORMANCE.md (ADR-P5).
#[derive(Default, Clone, Copy)]
struct StartupTimings {
    /// `init_backends_and_config`: config-file loads + local backend ready.
    config_init_ms: u128,
    /// `Database::open` (schema migrations included).
    db_open_ms: u128,
    /// `activate_persisted_theme`: the metadata read + custom-theme publish.
    theme_activate_ms: u128,
    /// Extension self-heal + built-in hooks wiring + agents.toml reload.
    extension_heal_ms: u128,
    /// `App::new` (keybindings JSON load, settings snapshot, channel setup).
    app_new_ms: u128,
    /// `load_persisted_state_from_db` + `restore_sessions` (sequential local
    /// adopt; remote backends restore on background threads, off this phase).
    restore_ms: u128,
    /// `arm_automation_heartbeat`: a synchronous tmux subprocess.
    heartbeat_ms: u128,
}

impl StartupTimings {
    /// The phase breakdown as JSON, mirroring the `startup` log line's fields
    /// (sans `first_frame_ms`, which isn't known until the first paint).
    fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "config_init_ms": self.config_init_ms as u64,
            "db_open_ms": self.db_open_ms as u64,
            "theme_activate_ms": self.theme_activate_ms as u64,
            "extension_heal_ms": self.extension_heal_ms as u64,
            "app_new_ms": self.app_new_ms as u64,
            "restore_ms": self.restore_ms as u64,
            "heartbeat_ms": self.heartbeat_ms as u64,
        })
    }
}

async fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    process_start: std::time::Instant,
    startup: StartupTimings,
) -> Result<()> {
    // Opt-in (THURBOX_PERF_LOG) time-to-first-frame measurement: logged once,
    // right after the first paint, so it never affects normal runs or the smoke
    // test. Read `~/.local/share/thurbox/thurbox.log` for the `startup` line
    // (phase breakdown + `first_frame_ms`). See docs/PERFORMANCE.md.
    let perf_log = std::env::var_os("THURBOX_PERF_LOG").is_some();
    let mut first_frame_logged = false;

    loop {
        // Redraw throttling: paint only when state changed since the last frame
        // or the forced-redraw floor elapsed. The loop still spins every ≤10ms
        // (cheap: poll + output check + tick), but the expensive layout/vt100
        // render is skipped when idle — see App::should_redraw / docs/PERFORMANCE.md.
        // Wall-clock timing is opt-in (THURBOX_PERF_LOG or the perf HUD): the
        // cached-bool gate keeps the default hot loop free of Instant reads.
        let timing = app.perf_timing_active();

        if app.should_redraw() {
            let draw_start = timing.then(std::time::Instant::now);
            // ratatui knows nothing about hyperlinks, so a rich-text link in an
            // agent's output reaches the outer terminal as a plain label. Hand
            // the links back to it once the frame is flushed, by re-printing
            // exactly the cells it just drew wrapped in OSC 8 — invisible, but
            // it lets the terminal open the user's own browser (the only route
            // to one when thurbox runs on a remote host).
            let frame = terminal.draw(|f| app.view(f))?;
            app.paint_terminal_hyperlinks(frame.buffer);
            if let Some(start) = draw_start {
                app.record_frame_time(start.elapsed());
            }
            app.mark_redrawn();

            if perf_log && !first_frame_logged {
                tracing::info!(
                    config_init_ms = startup.config_init_ms as u64,
                    db_open_ms = startup.db_open_ms as u64,
                    theme_activate_ms = startup.theme_activate_ms as u64,
                    extension_heal_ms = startup.extension_heal_ms as u64,
                    app_new_ms = startup.app_new_ms as u64,
                    restore_ms = startup.restore_ms as u64,
                    heartbeat_ms = startup.heartbeat_ms as u64,
                    first_frame_ms = process_start.elapsed().as_millis() as u64,
                    "startup"
                );
                first_frame_logged = true;
            }
        } else {
            app.note_redraw_skipped();
        }

        if event::poll(Duration::from_millis(10))? {
            if let Some(msg) = event_to_message(event::read()?) {
                let update_start = timing.then(std::time::Instant::now);
                app.update(msg); // marks the UI dirty
                if let Some(start) = update_start {
                    app.record_update_time(start.elapsed());
                }
            }
        }

        // Run a terminal editor staged by Ctrl+O. Blocks the loop for the
        // editor's lifetime (by design — the user is editing); inside tmux the
        // editor floats in a `display-popup` over the TUI, otherwise the TUI is
        // suspended and the editor owns the terminal. Either way a forced
        // repaint follows on return. See `App::take_pending_editor_run`.
        if let Some(inv) = app.take_pending_editor_run() {
            if let Err(e) = run_pending_editor(&mut *terminal, &inv) {
                tracing::warn!("editor run failed: {e:#}");
            }
            // The popup covered / the suspend tore down the frame; repaint fully.
            app.request_redraw();
        }

        // Cheap, lock-free check for new agent output (marks dirty on change).
        app.detect_output_redraw();

        let tick_start = timing.then(std::time::Instant::now);
        app.tick();
        if let Some(start) = tick_start {
            app.record_tick_time(start.elapsed());
        }

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
