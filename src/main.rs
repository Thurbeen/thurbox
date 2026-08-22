//! thurbox v2 — a session engine with a Lua-driven renderer.
//!
//! The kernel owns no pane. It resolves rects, calls plugins, paints what they
//! return, and refreshes a snapshot of the session engine on its own schedule.
//! Every surface you see — the session list included — is a file under `ui/`
//! that you can edit while this is running.
//!
//! This file holds the loop's state — `App`, whose every field is documented
//! with what broke without it — plus startup, terminal setup and the free
//! helpers. `App`'s behaviour lives in [`coordinator`], split by what each
//! group of methods is for. See `openspec/changes/archive/*-v2-plugin-kernel/`.

mod coordinator;

use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::{DefaultTerminal, Frame};

use thurbox::agent::input::key_to_bytes;
use thurbox::kernel::bands::{self, Band, BandState, Level};
use thurbox::kernel::command::CommandBus;
use thurbox::kernel::diff::DiffStore;
use thurbox::kernel::host::{Click, KeyPress, LuaHost, PluginError, RenderContext};
use thurbox::kernel::layout::{resolve, SlotMode};
use thurbox::kernel::metrics::{Metrics, Subject};
use thurbox::kernel::modals::{ModalKind, Modals};
use thurbox::kernel::node::{Axis, ClickVerb, Identity};
use thurbox::kernel::notify::Notifier;
use thurbox::kernel::paint;
use thurbox::kernel::perf::Counters;
use thurbox::kernel::registry::{canonical_chord, is_ctrl_letter_chord, Registry};
use thurbox::kernel::snapshot::SnapshotStore;
use thurbox::kernel::terminal::Terminals;
use thurbox::kernel::theme::Themes;
use thurbox::kernel::watch::Watcher;
use thurbox::session::selection::{PaneBounds, Selection, TermPos};

/// How long a frame waits for input before looping. Plugins animate off
/// `ctx.elapsed`, so this is also the animation rate.
/// How long the loop blocks waiting for input.
///
/// v1 uses 10ms (`src/main.rs`). At 50ms a keystroke could sit unnoticed for a
/// twentieth of a second before the loop even looked at it, which is felt as
/// lag however fast the frame that follows is. Polling this often is only
/// affordable because the expensive per-frame work below is gated on a paint
/// actually being due.
const TICK: Duration = Duration::from_millis(10);

/// The input poll's timeout once nothing has happened for [`QUIESCENT_AFTER`].
///
/// `event::poll` returns the instant an event arrives, so lengthening this costs
/// **no** input latency — a keystroke wakes the thread either way. What it slows
/// is noticing things that do not wake it: new agent output, a worker result, a
/// row another process wrote. At rest there is by definition none of the first,
/// and a 50ms delay on the others is not perceptible; the first sign of activity
/// puts the loop straight back on [`TICK`].
///
/// Worth 94 wakes a second against 20 on an idle interface, which was half its
/// entire cost.
const IDLE_TICK: Duration = Duration::from_millis(50);

/// How long nothing must happen before the loop slows its poll to
/// [`IDLE_TICK`]. Longer than a keypress-to-repaint round trip, so typing never
/// crosses into the slow poll and back.
const QUIESCENT_AFTER: Duration = Duration::from_millis(500);
/// Editors save in bursts (write, rename, chmod); wait for the dust to settle.
const DEBOUNCE: Duration = Duration::from_millis(120);
/// Longest a frame may go unpainted when nothing has changed.
///
/// Covers time-driven content the diff cannot see — a spinner, a clock — and is
/// what turns an idle app from ~20 fps into ~4. v1 uses the same floor.
const FORCE_REDRAW_INTERVAL: Duration = Duration::from_millis(250);

/// Iterations between two `perf_window` log lines (~10s at the 10ms tick).
const PERF_WINDOW_TICKS: u64 = 1000;

/// How often the JSON snapshot is written while timing is active. Slower than
/// the log line because it is a database write every other thurbox connection
/// pays for with a `data_version` bump.
const PERF_PUBLISH_INTERVAL: Duration = Duration::from_secs(5);

/// The floor between two paints.
///
/// The poll above runs every 10ms so input is noticed at once, but a frame here
/// costs far more than v1's -- every visible pane is rebuilt through Lua and
/// converted back. Without a cap, an agent streaming output marks the screen
/// dirty on every poll (`Terminals::output_generation`, checked in the loop) and
/// drives 100 paints a second for a terminal nobody can read that fast. 60fps
/// keeps typing and output feeling immediate while bounding the cost of a chatty
/// agent.
///
/// This cap only bites once output *causes* a frame at all. It did not until
/// the generation check was hoisted into the loop — before that a printing agent
/// was drawn at the `FORCE_REDRAW_INTERVAL` floor, four times a second, which is
/// what made v2 feel less responsive than v1.
const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// The floor when the only thing owed a frame is new agent output.
///
/// Typing has to feel instant; watching a log scroll does not, and applying the
/// 16ms floor to both meant a chatty agent drove ~60 paints a second to show 30
/// lines. Measured across the interval (`docs/PERFORMANCE.md`, ADR-P17): 62fps
/// costs 21.2% of a core, 30fps costs 14.4% and 20fps 13.1% — most of the saving
/// arrives by 30, and below it the scroll starts to look stepped rather than
/// smooth. So: 30fps for output, and a keystroke still repaints on the next
/// frame.
const OUTPUT_FRAME_INTERVAL: Duration = Duration::from_millis(33);

/// Consecutive input-read failures tolerated before the loop gives up.
///
/// One is a terminal handing crossterm bytes it cannot parse, which is a
/// keystroke to drop rather than a reason to quit. A run of them is a stream
/// that has gone away, and polling it forever would spin at full speed.
const INPUT_FAILURE_LIMIT: u32 = 64;
/// How long an outcome message stays up. v1's `STATUS_MESSAGE_TTL`.
const STATUS_TTL: Duration = Duration::from_secs(5);

// A tokio runtime is required, not decorative: adopting a session spawns its
// reader on `spawn_blocking` and its writer on `tokio::spawn`. The render loop
// below stays synchronous — as v1's does — and those tasks run on the worker
// pool.
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Put the terminal back before the panic message prints.
    //
    // v2 had no hook at all, where v1 has always installed one
    // (`src/main.rs`). Without it a panic — or a kill — leaves the terminal in
    // raw mode with mouse reporting still on, and the terminal then streams
    // reports at whatever shell comes next, which is why moving the pointer
    // afterwards printed `\x1b[<35;…M` into the prompt. Restoring here is also
    // why the message is readable at all: on the alternate screen it would be
    // wiped the moment the shell repainted.
    //
    // The message is also written to the log, because stderr is where a panic
    // is *least* likely to be read: a worker thread's panic does not end the
    // process, so the pane it printed on is scrolled away long before anyone
    // looks — which is how a reader thread dying, and taking one session's
    // terminal with it for the rest of the run, left no trace to report.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        tracing::error!(
            "panicked at {}: {}",
            info.location()
                .map(ToString::to_string)
                .unwrap_or_else(|| "an unknown location".to_string()),
            info.payload()
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "a payload of an unknown type".to_string()),
        );
        original_hook(info);
    }));

    // Extensions, before the interface takes the terminal.
    //
    // Two things, both idempotent and both best-effort: re-create any
    // session/automation an active extension declares but that has since been
    // deleted, and auto-activate the built-in `hooks` extension. The second is
    // load-bearing rather than a nicety — it is what patches `agents.toml` so an
    // agent reports working/blocked/done at all, so without it a fresh profile
    // shows every session as permanently idle. Run here for the same reason v1
    // runs it here: tmux spawn output would otherwise land on the alternate
    // screen. Opt out with `thurbox-cli extension deactivate hooks`.
    // The user's settings, published process-wide BEFORE anything reads one.
    // `Database::open` below reads a restart-only value — it prunes the audit log
    // to `audit_retention_days` — and v1 loads them at the same point for the same
    // reason. Without this call `settings::global()` hands out `Settings::default`
    // and the whole file is ignored, however carefully it was written.
    // File-based logging: stdout belongs to the TUI, so every `tracing` call in
    // this process — the panic hook, a worker's warning, the perf lines below —
    // has nowhere else to go. Without a subscriber they are not merely
    // unformatted but *dropped*, which is how a reader thread could die and
    // leave no trace anywhere. The guard is deliberately leaked: the appender's
    // worker must outlive every later log call, including the panic hook's, and
    // this runs once per process.
    let log_dir = thurbox::paths::log_directory().unwrap_or_else(|| std::path::PathBuf::from("."));
    std::fs::create_dir_all(&log_dir).ok();
    let (writer, guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::daily(log_dir, "thurbox.log"));
    Box::leak(Box::new(guard));
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("thurbox=debug".parse().unwrap_or_default()),
        )
        .with_writer(writer)
        .with_ansi(false)
        .init();

    // Startup phases are timed unconditionally: this runs once, so the two
    // `Instant` reads per phase cost nothing measurable, and the numbers are
    // only *reported* when timing is active.
    let process_start = Instant::now();
    let mut startup = thurbox::kernel::perf::Startup::default();

    let phase = Instant::now();
    let (config, config_warnings) = thurbox::kernel::config::Config::load();
    startup.config_init_ms = phase.elapsed().as_millis() as u64;

    let mut startup_notices: Vec<String> = config_warnings;
    let phase = Instant::now();
    if let Some(db) = snapshots_db() {
        startup_notices.extend(thurbox::session_ops::heal_active_extensions(&db));
        startup_notices.extend(thurbox::session_ops::ensure_builtin_extensions(&db));
        for notice in &startup_notices {
            tracing::info!("{notice}");
        }
    }
    startup.extension_heal_ms = phase.elapsed().as_millis() as u64;

    // The tmux heartbeat keeper, so a schedule keeps firing after this exits —
    // and, while it runs, at the keeper's 60s cadence rather than not at all.
    // Best-effort: a missing or old tmux just means no headless firing. Skipped
    // when the feature is off, exactly as v1 skips it.
    let phase = Instant::now();
    if thurbox::session::settings::global().features.automations {
        let cli = thurbox::agent::tmux::resolve_cli_binary();
        if let Err(e) = thurbox::agent::tmux::ensure_automation_heartbeat(&cli) {
            tracing::warn!("could not arm the automation heartbeat: {e}");
        }
    }
    startup.heartbeat_ms = phase.elapsed().as_millis() as u64;

    // The gate. v2 replaces v1 under the same binary name, so auto-update moves
    // people to a different interface without their asking -- and several surfaces
    // they may use daily are gone. A profile with v1 history is asked once, before
    // the interface takes the terminal so it shows even if the interface would fail
    // to build. Declining cannot load v1 (it is not in this binary), so it turns
    // auto-update off and says how to reinstall the 1.x line.
    if let Some(db) = snapshots_db() {
        if thurbox::kernel::consent::consent_gate(&db)?
            == thurbox::kernel::consent::Decision::Declined
        {
            return Ok(());
        }
    }

    let (ui_dir, ui_notices) = resolve_ui_dir()?;
    startup_notices.extend(ui_notices);

    // A user copy that will not load must not cost the interface, but the falling
    // back lives in `App::reload_interface` — called once below and on every
    // reload after. Deciding it twice is how the floor became a one-way door:
    // startup installed the fallback and every later reload rebuilt *that*.
    let ui_phase = Instant::now();
    let host = LuaHost::new(&ui_dir);

    // Resolved before the move, since `focus` indexes into the host.
    let initial_focus = focus_index_of(&host, "agent");

    // Hoisted out of the struct literal below so each phase can be timed
    // separately; the construction order is unchanged.
    let phase = Instant::now();
    let snapshots = SnapshotStore::open();
    startup.db_open_ms = phase.elapsed().as_millis() as u64;

    let phase = Instant::now();
    let themes = Themes::load(snapshots_db().as_ref());
    startup.theme_activate_ms = phase.elapsed().as_millis() as u64;

    let mut app = App {
        host,
        sources: thurbox::kernel::bundled::sources(&ui_dir),
        watcher: Watcher::new(&ui_dir)?,
        ui_dir,
        snapshots,
        terminals: Terminals::new(),
        commands: CommandBus::new(),
        diffs: DiffStore::new(),
        repos: thurbox::kernel::repos::RepoStore::new(),
        metrics: Metrics::new(),
        clipboard: arboard::Clipboard::new().ok(),
        perf: Counters::default(),
        timings: thurbox::kernel::perf::Timings::default(),
        startup,
        perf_log: std::env::var_os("THURBOX_PERF_LOG").is_some(),
        perf_window_base: thurbox::kernel::perf::Snapshot::default(),
        perf_window_tick: 0,
        perf_published_at: None,
        first_frame_logged: false,
        process_start,
        data_epoch: 0,
        last_activity: Instant::now(),
        input_dirty: true,
        animation_tick: 0,
        animation_step: 0,
        selection: None,
        notifier: {
            let settings = thurbox::session::settings::global();
            Notifier::new(settings.features.notifications, settings.notifications)
        },
        themes,
        updates: thurbox::kernel::updates::Updates::start(config.features()),
        slot_selection: std::collections::HashMap::new(),
        visible_slots: std::collections::HashSet::new(),
        pending_focus: None,
        click_targets: Vec::new(),
        last_area: Rect::new(0, 0, 0, 0),
        screen_size: crossterm::terminal::size().unwrap_or((80, 24)),
        selected_text: None,
        hovered: None,
        mouse: config.features().mouse,
        config,
        registry: Registry::load(),
        modals: Modals::default(),
        // v1 boots with the TERMINAL focused, not the session list: the point
        // of the app is the agent, and you should be able to type at it without
        // a keystroke of navigation first. Falls back to the first focusable
        // plugin when the agent pane is absent (a user who removed it).
        focus: initial_focus,
        focus_return: initial_focus,
        reload_at: None,
        errors: Vec::new(),
        links: std::collections::HashMap::new(),
        link_stamps: std::collections::HashMap::new(),
        content: std::collections::HashMap::new(),
        content_generation: None,
        trust: std::collections::HashMap::new(),
        trust_stale: true,
        layout_error: None,
        floor: None,
        status: None,
        reported_failures: std::collections::HashSet::new(),
        tracked_commands: std::collections::HashMap::new(),
        band_targets: Vec::new(),
        focused_session: None,
        focused_surface: None,
        last_selected_session: None,
        started: Instant::now(),
        frames: 0,
        last_trees: Vec::new(),
        last_bands: std::collections::HashMap::new(),
        last_floats: std::collections::HashMap::new(),
        drawn_floats: std::collections::HashSet::new(),
        last_paint: Instant::now(),
        last_placed: Vec::new(),
        dirty: true,
        changed_this_frame: false,
        last_output_painted: std::collections::HashMap::new(),
        last_output_gen: 0,
        grabbed: None,
        runs: thurbox::kernel::runs::RunStore::new(),
        inventory: Vec::new(),
        respawned: std::collections::HashSet::new(),
        reaper: thurbox::kernel::reaper::Reaper::default(),
        bookmark_in_flight: false,
        hud: false,
        quit: false,
    };

    // The user's decisions have to reach the host before the interface it will
    // run is built: `LuaHost::new` already built one, from every file on disk,
    // so a plugin turned off would be loaded for exactly one frame. Told, then
    // rebuilt.
    app.publish_disabled();
    app.reload_interface();
    // Declarations are collected once up front and again on every reload, so
    // a newly added plugin's keys appear in help with nothing else edited.
    app.collect_declarations();
    // Everything from `LuaHost::new` to here is what it costs to have an
    // interface at all — a v2-only startup phase, and the one a slow plugin
    // shows up in.
    app.startup.ui_build_ms = ui_phase.elapsed().as_millis() as u64;
    // Non-empty only on a profile's first wire-up or on a failure, so this is a
    // real signal rather than noise on every launch.
    if let Some(notice) = startup_notices.first() {
        app.toast(notice.clone());
    }

    let terminal = ratatui::init();

    // The kitty keyboard protocol, ported from v1's `main`. It disambiguates
    // escape codes, which is what makes a bound `cmd+…` chord distinguishable at
    // all (iTerm2 3.5+, kitty, WezTerm, Ghostty -- not Terminal.app) and what
    // separates `ctrl+/` from the bytes a legacy terminal sends for it. Pushed
    // after the terminal is taken and popped by `restore_terminal`, which
    // `ratatui::restore()` does not do for us.
    push_keyboard_enhancement();
    // Drag-to-select and click targeting need mouse reporting. Enabled after
    // init so the panic hook ratatui installed still restores the terminal if
    // anything below goes wrong, and only when `[features] mouse` is on — v1
    // gates the same escape at the same point, so turning it off leaves the
    // terminal's own selection and scrolling behaving natively.
    if app.mouse {
        enable_mouse_clicks();
    }
    // Bracketed paste, so text pasted with the TERMINAL's own chord arrives as
    // one `Event::Paste` rather than as a stream of keystrokes — without it the
    // first newline in a multi-line paste submits the prompt mid-paste. It is
    // also the only paste route that works over SSH, where there is no local
    // clipboard to read. v1 enables it unconditionally, mouse or not.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste);
    let result = app.run(terminal);
    restore_terminal();
    result
}

/// A connection for reading the persisted theme choice at startup.
///
/// Separate from the snapshot store's: this is read once, and opening a second
/// short-lived connection is cheaper than threading one through construction.
fn snapshots_db() -> Option<thurbox::storage::Database> {
    thurbox::paths::database_file().and_then(|path| thurbox::storage::Database::open(&path).ok())
}

/// Can a link actually be opened here?
///
/// On a remote session or a bare tty there is no browser, and spawning an
/// opener goes nowhere silently — which is why v1 learned to copy instead and
/// say so. Published to plugins so a pane can label its key "open" or "copy"
/// *before* you press it.
///
/// v1's `has_browser_target`: any of the three set and non-blank is enough, and
/// `BROWSER` overrides the display check because a terminal browser is a valid
/// target on a machine with no X or Wayland session.
fn browser_available() -> bool {
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        return true;
    }
    ["BROWSER", "DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty()))
}

/// Hand a URL to the platform's opener.
///
/// v1's `helpers::open_url`. The child is spawned and **not** waited on: a
/// launcher can take seconds to come up and the render loop must not park in
/// `waitpid`, so a successful spawn is reported as opened.
fn open_url(url: &str) -> Result<(), String> {
    let (program, args) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(target_os = "windows") {
        // The empty string is `start`'s window-title argument; without it
        // `start` swallows the URL as the title and opens nothing.
        ("cmd", vec!["/C", "start", "", url])
    } else {
        if !browser_available() {
            return Err("No display to open a browser on".to_string());
        }
        ("xdg-open", vec![url])
    };

    std::process::Command::new(program)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("No URL opener ({program} not installed)"),
            _ => format!("Could not run {program}: {e}"),
        })
}

/// Turn a written chord back into the keystroke it names.
///
/// The inverse of `registry::canonical_chord`, needed only by
/// [`ClickVerb::Key`]: replaying a click as a real key event is what stops a
/// modal button and its letter from ever diverging. Built on
/// `registry::normalise_chord` so the spellings a plugin may write (`Ctrl+D`,
/// `command+j`) stay the registry's vocabulary rather than becoming a second
/// one.
fn key_event_from_chord(chord: &str) -> Option<KeyEvent> {
    let normalised = thurbox::kernel::registry::normalise_chord(chord);
    let mut modifiers = KeyModifiers::NONE;
    let mut name = "";
    for part in normalised.split('+') {
        match part {
            "ctrl" => modifiers |= KeyModifiers::CONTROL,
            "alt" => modifiers |= KeyModifiers::ALT,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            "cmd" => modifiers |= KeyModifiers::SUPER,
            other => name = other,
        }
    }

    let code = match name {
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "space" => KeyCode::Char(' '),
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        other => match other.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
            Some(n) if (1..=12).contains(&n) => KeyCode::F(n),
            // A bare character, which is most of them. `chars().count()`
            // rather than `len()`, so a non-ASCII key is not read as several.
            _ => {
                let mut chars = other.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => KeyCode::Char(c),
                    _ => return None,
                }
            }
        },
    };
    Some(KeyEvent::new(code, modifiers))
}

/// The editor to open a session's directory with.
///
/// v1's chain, from `resolve_editor` (`src/cli/config.rs`): the DB setting
/// `thurbox-cli editor set` writes, then `$VISUAL`, then `$EDITOR`.
fn editor_command() -> Option<String> {
    snapshots_db()
        .and_then(|db| db.get_editor_command().ok().flatten())
        .or_else(|| std::env::var("VISUAL").ok())
        .or_else(|| std::env::var("EDITOR").ok())
        .filter(|command| !command.trim().is_empty())
}

/// How the editor should be launched, as configured (`thurbox-cli editor mode`).
///
/// `Auto` — the default — leaves the decision to the name-based classification.
fn editor_mode() -> thurbox::session::settings::EditorMode {
    snapshots_db()
        .and_then(|db| db.get_editor_mode().ok())
        .unwrap_or_default()
}

/// Run the configured editor over a session's directories.
///
/// Every directory, not just the first: a multi-repo session's whole point is
/// that its repositories are worked together, and opening one of them is the
/// same bug as forgetting the others exist.
///
/// A **terminal** editor gets a real tty, which mirrors v1's
/// `run_pending_editor` (`src/main.rs`): inside tmux it floats in a
/// `display-popup` with a pty of its own and the TUI keeps its screen
/// underneath; elsewhere the terminal is handed over for the editor's lifetime
/// and taken back afterwards — the git/sudoedit pattern. Blocking the render
/// loop while someone edits is correct, not a bug.
///
/// A **GUI** editor is spawned detached instead. Handing one a tty is not
/// harmless: a launcher told to wait (`code --wait`) holds the terminal for the
/// whole editing session with nothing drawn in it.
fn open_editor(
    terminal: &mut DefaultTerminal,
    dirs: &[std::path::PathBuf],
) -> Result<String, String> {
    let first = dirs
        .first()
        .ok_or("that session has no directory to open")?;
    let configured = editor_command()
        .ok_or("no editor configured — set one with `thurbox-cli editor set <command>`")?;
    let (program, mut args) = thurbox::session::editor::parse_editor_command(&configured)
        .map_err(|e| format!("the configured editor command is unusable: {e}"))?;
    let terminal_editor =
        thurbox::session::editor::is_terminal_editor(&program, &args, editor_mode());
    args.extend(dirs.iter().map(|dir| dir.display().to_string()));

    let opened = if dirs.len() == 1 {
        format!("opened {}", first.display())
    } else {
        format!("opened {} directories", dirs.len())
    };

    if !terminal_editor {
        // Detached: no tty, no wait, and deliberately no report of how it went —
        // a GUI editor's exit status arrives long after anyone is looking.
        return match std::process::Command::new(&program).args(&args).spawn() {
            Ok(_) => Ok(format!("{opened} in {program}")),
            Err(e) => Err(format!("could not run {program}: {e}")),
        };
    }

    if std::env::var_os("TMUX").is_some() {
        // Quoted and run through tmux's shell, so a path or flag with a space
        // in it survives being flattened into one command string.
        let mut script = thurbox::shell::posix_quote(&program);
        for arg in &args {
            script.push(' ');
            script.push_str(&thurbox::shell::posix_quote(arg));
        }
        // `-E` closes the popup when the editor exits; the editor's own exit
        // code is ignored, since a non-zero edit must not trigger a retry.
        // A tmux that would not run at all falls through to the suspend path
        // rather than leaving the key doing nothing.
        let launched = std::process::Command::new("tmux")
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
            .arg(&script)
            .status();
        if launched.is_ok() {
            return Ok(format!("{opened} in {program}"));
        }
    }

    // Stand the interface down so the editor inherits a normal cooked
    // terminal, then put everything back and force a full repaint — the
    // editor overwrote the cells ratatui thinks are on screen.
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::terminal::LeaveAlternateScreen
    );
    let _ = crossterm::terminal::disable_raw_mode();
    let status = std::process::Command::new(&program).args(&args).status();
    let _ = crossterm::terminal::enable_raw_mode();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen);
    // Mirrors the disable above; harmless when the feature is off, since the
    // loop drops mouse events either way.
    enable_mouse_clicks();
    let _ = terminal.clear();

    match status {
        Ok(_) => Ok(format!("closed {program}")),
        Err(e) => Err(format!("could not run {program}: {e}")),
    }
}

/// Find the plugin directory.
///
/// Two rules, in order: `THURBOX_UI_DIR`, then the user's own copy —
/// materialized from the embedded interface on first run, preserving anything
/// they edited. A missing or unwritable config directory is not fatal: the
/// embedded copies are written somewhere throwaway and used from there, because
/// no interface at all is the one outcome worth avoiding (design.md D11).
/// Index into the host's focusable plugins for `name`, or 0.
///
/// `App::focus` indexes the FOCUSABLE list, not the plugin list, so this cannot
/// just be `plugins.position(...)`.
/// Ask the terminal for clicks, motion, and SGR coordinates.
///
/// Three modes, each earning its keep:
///
/// * `?1000` — presses and releases, what the click registry needs.
/// * `?1003` — motion, **whether or not a button is down**. Both of thurbox's
///   pointer features need it and neither worked without it: a drag reports
///   nothing between press and release (so dragging a selection selected
///   nothing), and a *hover* highlight has no event at all to fire on, which is
///   why every button stayed unlit. `?1002` covers only the first of those, so it
///   is not enough. The flood is real — one report per cell crossed — and is
///   absorbed where it arrives rather than by asking for less: the loop drains
///   every queued event per iteration, and a `Moved` that does not change the
///   identity under the pointer is dropped without touching `dirty`.
/// * `?1006` — SGR coordinates, so columns past 223 survive.
///
/// v1 asks for `?1000`+`?1002` (crossterm's `EnableMouseCapture`), which is why
/// its hover highlight is likewise limited to drags.
///
/// `DisableMouseCapture` still turns everything off, so teardown is unchanged.
/// Undo everything `main` did to the terminal, in reverse.
///
/// Safe to call twice and safe to call when some of it was never enabled —
/// every step is best-effort, because this runs on the panic path where
/// failing to clean up is worse than a redundant escape.
fn restore_terminal() {
    // Reverse order of setup, so the kitty flags come off while raw mode is still
    // on -- popping them after `ratatui::restore()` would write the escape to a
    // cooked terminal.
    pop_keyboard_enhancement();
    // Unconditional: cheaper than tracking whether capture was on, and a
    // terminal left reporting is the failure this exists to prevent.
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste
    );
    ratatui::restore();
}

/// Whether we pushed the kitty flags, so only we pop them.
static KEYBOARD_ENHANCEMENT_PUSHED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Ask for `DISAMBIGUATE_ESCAPE_CODES`, if the terminal supports it.
fn push_keyboard_enhancement() {
    use crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
    if matches!(
        crossterm::terminal::supports_keyboard_enhancement(),
        Ok(true)
    ) && crossterm::execute!(
        std::io::stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
    .is_ok()
    {
        KEYBOARD_ENHANCEMENT_PUSHED.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Pop them if and only if we pushed.
///
/// `swap` so a second restore -- the panic hook racing the normal path -- cannot
/// pop a level we never pushed.
fn pop_keyboard_enhancement() {
    if KEYBOARD_ENHANCEMENT_PUSHED.swap(false, std::sync::atomic::Ordering::SeqCst) {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PopKeyboardEnhancementFlags
        );
    }
}

fn enable_mouse_clicks() -> bool {
    use std::io::Write;
    let mut out = std::io::stdout();
    out.write_all(b"\x1b[?1000h\x1b[?1003h\x1b[?1006h").is_ok() && out.flush().is_ok()
}

fn focus_index_of(host: &LuaHost, name: &str) -> usize {
    host.focusable()
        .iter()
        .position(|index| {
            host.plugins
                .get(*index)
                .is_some_and(|plugin| plugin.name == name)
        })
        .unwrap_or(0)
}

fn resolve_ui_dir() -> Result<(PathBuf, Vec<String>), Box<dyn Error>> {
    // The resolution itself lives in the library, so `thurbox-cli plugin dir`
    // reports the directory this will actually load. Writing the user's copy is
    // the interface's business, which is why it asks for it.
    let (dir, chosen, report) = thurbox::kernel::bundled::resolve(true)?;
    let mut notices = Vec::new();
    notices.extend(directory_notice(&dir, chosen));
    notices.extend(delivery_notice(&report));
    Ok((dir, notices))
}

/// Which interface just loaded — said only when there is a question.
///
/// Silent for a release build on its own copy, because there is nothing to
/// disambiguate and a greeting on every launch is noise. It speaks when an
/// override is in force (that is somebody's deliberate redirection, and the most
/// likely thing to have been forgotten), when the embedded fallback had to be
/// used, and on a **dev build** — where "which interface am I running" is a real
/// question, since the checkout beside you contains one too.
fn directory_notice(dir: &Path, chosen: thurbox::kernel::bundled::Chosen) -> Option<String> {
    use thurbox::kernel::bundled::Chosen;
    let shown = thurbox::paths::display_path(dir);
    match chosen {
        Chosen::UserCopy if cfg!(dev_build) => Some(format!(
            "interface from {shown} · set THURBOX_UI_DIR to use a checkout"
        )),
        Chosen::UserCopy => None,
        Chosen::Override => Some(format!("interface from {shown} (THURBOX_UI_DIR)")),
        Chosen::Checkout => Some(format!("interface from {shown} (checkout)")),
        // Not a preference: the user's copy could not be written, so the panes
        // are the embedded ones in a directory that will not survive the process.
        Chosen::Fallback => Some(format!(
            "interface from {shown} — your copy could not be written"
        )),
    }
}

/// What an upgrade did that the user did not ask for and should know about.
///
/// Only the two outcomes that are about THEIR files: an edit of theirs kept
/// where a newer version was available, and a file taken back because this
/// binary no longer ships it. Writes and updates are the ordinary case and say
/// nothing, so this stays a signal rather than a greeting.
fn delivery_notice(report: &thurbox::kernel::bundled::Report) -> Option<String> {
    let mut parts = Vec::new();
    if !report.preserved.is_empty() {
        parts.push(format!(
            "kept your version of {}",
            report.preserved.join(", ")
        ));
    }
    if !report.retired.is_empty() {
        parts.push(format!(
            "removed {} — no longer part of the interface",
            report.retired.join(", ")
        ));
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// A hitbox from the frame just painted, and the plugin that painted it.
///
/// v1's `App::ClickTarget`, with the crucial difference that its `action` is
/// not an enum the kernel has to know: the identity travels as the plugin wrote
/// it, and only the handful of [`ClickVerb`]s are the kernel's business.
///
/// An **empty** identity is the plugin's own rect — v1's `FocusPane` fallback,
/// recorded before the tree so anything inside it wins.
#[derive(Debug, Clone)]
struct ClickTarget {
    plugin: usize,
    rect: Rect,
    identity: Identity,
}

/// A command seen in flight, remembered so its outcome can be reported and its
/// session let go of when it finishes.
///
/// Remembered rather than read back at the end because a finished command simply
/// leaves the list, taking what it was about with it — a deleted session's row
/// is already gone by the time its delete reports.
#[derive(Clone)]
struct TrackedCommand {
    kind: &'static str,
    session: String,
    label: Option<String>,
    failed: bool,
}

struct App {
    host: LuaHost,
    /// The directory the interface was loaded from. Held because every command
    /// about a plugin file names a path relative to it.
    ui_dir: PathBuf,
    /// Where each file of the interface came from.
    ///
    /// Cached because answering it reads and digests every file: it changes
    /// only when the directory does, which is exactly when the host reloads.
    sources: std::collections::BTreeMap<String, thurbox::kernel::bundled::Source>,
    watcher: Watcher,
    snapshots: SnapshotStore,
    terminals: Terminals,
    commands: CommandBus,
    diffs: DiffStore,
    /// What the creation flow asks about: remembered repositories, directory
    /// listings, branch lists. Requests arrive through `store` and are served
    /// on workers, like every other read that touches the world.
    repos: thurbox::kernel::repos::RepoStore,
    metrics: Metrics,
    /// Native clipboard handle, when the platform has one.
    ///
    /// Built once and kept: `clipboard::copy`/`paste` take it by reference, and
    /// passing `None` means every paste reports an unreachable clipboard — which
    /// is exactly what happened while this field did not exist. v1 holds the
    /// same handle for the same reason.
    clipboard: Option<arboard::Clipboard>,
    notifier: Notifier,
    perf: Counters,
    /// Wall-clock stats, populated only while timing is active (ADR-P11).
    timings: thurbox::kernel::perf::Timings,
    /// How long each startup phase took; published and logged once.
    startup: thurbox::kernel::perf::Startup,
    /// `THURBOX_PERF_LOG` was set, read once at construction. The other half of
    /// [`Self::perf_timing_active`] is the HUD, which can be toggled.
    perf_log: bool,
    /// Counters as they stood when the current perf window opened, so the
    /// `perf_window` line reports deltas rather than lifetime totals.
    perf_window_base: thurbox::kernel::perf::Snapshot,
    /// Iteration count at which the current perf window opened.
    perf_window_tick: u64,
    /// When the JSON snapshot was last written to the database.
    perf_published_at: Option<Instant>,
    /// The one-shot `startup` line is logged after the first painted frame.
    first_frame_logged: bool,
    /// True process start, taken before any startup phase — `started` is taken
    /// during construction and so misses everything before it.
    process_start: Instant,
    /// Whether this frame is owed to something a person did — a keypress, a
    /// resize, a worker result they asked for — rather than to an agent
    /// printing. Only the first kind gets [`MIN_FRAME_INTERVAL`].
    input_dirty: bool,
    /// When anything last happened — input, output, a worker result, a repaint
    /// that changed something. Drives the poll timeout, nothing else.
    last_activity: Instant,
    /// The shared animation clock, advanced only while something is animating.
    ///
    /// Kept here rather than read from `ctx.elapsed` in the render, because
    /// whether anything is animating is the loop's knowledge: a spinner turns
    /// for a session that is *working*, and the creation flow's pending row for
    /// a command in flight. With neither, the clock stands still and a pure
    /// pane's tree survives — which is what lets an idle interface stop
    /// rebuilding anything at all (`frame-cost`).
    animation_tick: u64,
    /// The last `elapsed * ANIMATION_HZ` step the tick was advanced for, so a
    /// step is counted once however many frames fall inside it.
    animation_step: u64,
    /// Moves whenever data the loop owns and publishes does — a worker store
    /// that took a result, the links or screen text just re-scanned, the
    /// in-flight command list, an attach failure.
    ///
    /// The stores already answer "did anything land" from `poll`, so this reads
    /// that rather than duplicating a counter inside each one: a signal derived
    /// from the existing return value cannot drift from it. Combined with the
    /// versions the kernel sources carry into [`Self::publish_epoch`].
    data_epoch: u64,
    /// Active mouse text selection over a terminal surface, if any.
    selection: Option<Selection>,
    themes: Themes,
    /// Which occupant of each `switch` slot is visible, by slot name.
    ///
    /// Focusing a plugin in a switch slot makes it the visible one, which is
    /// both how switching is driven and how the spec's "focus never rests on a
    /// hidden pane" rule is satisfied without a second mechanism.
    slot_selection: std::collections::HashMap<String, usize>,
    /// Whether a newer release exists, and the silent update if it was allowed.
    updates: thurbox::kernel::updates::Updates,
    /// The user's settings: the live half re-read when the file changes, the
    /// restart-only half as published at startup. See `kernel::config`.
    config: thurbox::kernel::config::Config,
    registry: Registry,
    /// Help, settings and the theme picker: kernel-owned, overlaying, and
    /// outside both the layout and the focus ring. See `kernel::modals`.
    modals: Modals,
    /// Slots the arrangement actually placed on the last frame.
    ///
    /// A side column is only in here while it is toggled open, so this is what
    /// keeps Tab from parking focus on a pane nobody can see — v1's rule that a
    /// panel is "a cycle stop only while visible".
    visible_slots: std::collections::HashSet<String>,
    /// A focus request whose slot the arrangement had not placed yet.
    ///
    /// Held for exactly one layout and re-asked there. See
    /// `kernel::focus::defer_until_placed`: a pane that opens its own slot asks
    /// for focus a frame before the slot exists, and judging that request against
    /// the frame that already painted refuses the focus its chord existed to give.
    pending_focus: Option<usize>,
    /// Every identified node of the frame just painted, in paint order.
    ///
    /// Rebuilt each frame and scanned in reverse, so the innermost node under a
    /// point — and, across plugins, the one painted last — wins. That is how a
    /// tab pill on a pane's border beats the pane's own focus fallback.
    click_targets: Vec<ClickTarget>,
    /// The area the last frame was painted into.
    ///
    /// A selection outside every terminal is anchored to this, so a drag over
    /// the session list or a modal has a rect to clamp against.
    last_area: Rect,
    /// The terminal's size, seeded once at startup and updated from
    /// `Event::Resize`.
    ///
    /// Cached because `terminal::size()` is a syscall and its two consumers run
    /// on every iteration of a loop that polls every 10 ms — `readopt_shells`
    /// already refused to pay it per iteration, and this extends the same
    /// reasoning to the attach seed size.
    screen_size: (u16, u16),
    /// The text under the current selection, read while the frame that painted
    /// it is still in hand.
    ///
    /// v1 caches it the same way (`selected_text_cache`) and for the same
    /// reason: a selection outside a terminal can only be read off the painted
    /// buffer, and the buffer is gone by the time `Ctrl+C` arrives.
    selected_text: Option<String>,
    /// The identity under the pointer, for hover highlighting.
    ///
    /// Stored as the identity rather than the position so a move WITHIN the
    /// same affordance is free: the redraw is gated on this changing, not on
    /// the pointer moving. v1 keeps the position instead and re-resolves it
    /// every frame; this way a mouse crossing the screen costs one repaint per
    /// affordance rather than one per cell.
    hovered: Option<Identity>,
    /// `[features] mouse`. Off means no capture escape was ever sent, so the
    /// terminal keeps its native selection and scrolling.
    mouse: bool,
    /// The session shown by the focused plugin's surface, as of the last frame.
    /// Read off the tree that was just painted, so the kernel never needs to
    /// know which plugin is "the terminal".
    focused_session: Option<String>,
    /// The surface the focused pane is showing, of either kind — a session's
    /// terminal or a program a plugin owns.
    ///
    /// Distinct from `focused_session` because the two answer different questions:
    /// that one is "which session am I looking at", which a program pane has no
    /// answer to, and this one is "where do unclaimed keys go".
    focused_surface: Option<String>,
    /// The session the list had selected last frame, so moving off one can
    /// acknowledge the finished turn it was showing.
    last_selected_session: Option<String>,
    /// Index into the host's focusable plugins.
    focus: usize,
    /// Where focus was before the last deliberate move, so `Esc` can go back.
    ///
    /// v1's pickers and panels are modals: `Esc` closes them and focus returns
    /// to what you were doing. v2's are centre-slot occupants, so "closing" one
    /// IS returning focus — without this, `Esc` in the theme picker did nothing
    /// at all once the kernel stopped treating a bare `Esc` as quit.
    focus_return: usize,
    /// Set once a change is seen, fired after the debounce window.
    reload_at: Option<Instant>,
    /// Failures from this frame's render calls, one per failing plugin.
    errors: Vec<PluginError>,
    /// A failure from `ui/layout.lua`, cleared by the next arrangement that
    /// works.
    layout_error: Option<String>,
    /// Why the bundled interface is running instead of the user's copy.
    ///
    /// Sticky, and separate from `layout_error` for a reason that cost the notice
    /// entirely: `layout_error` is cleared on every frame whose arrangement
    /// resolves, and the fallback's arrangement always does — so a message put
    /// there was wiped before it could ever be painted. The floor is a state that
    /// lasts until the user's copy loads again, so it is recorded as one.
    floor: Option<String>,
    /// What just happened, and when. Separate from `layout_error` because that
    /// field is reset by every successful arrangement — which is once a frame —
    /// so a message sharing it was gone before it could be read.
    status: Option<(String, Level, Instant)>,
    /// Commands whose failure has already been reported, so the window in which
    /// a failure lingers for the panes does not re-raise it every poll.
    reported_failures: std::collections::HashSet<u64>,
    /// Commands seen in flight: `id → (verb, session, what it is about, failed)`.
    ///
    /// Kept because a finished command simply leaves the list — there is no
    /// "done" to observe — and because what it was about has to be captured
    /// while it still can be: a deleted session's row is gone by the time its
    /// delete reports.
    tracked_commands: std::collections::HashMap<u64, TrackedCommand>,
    /// Where the chrome bands drew their buttons this frame.
    ///
    /// Kept apart from `click_targets` because a band is not a plugin: a click
    /// on one must not focus a pane, and there is no plugin index to record.
    /// Same reason the system modals keep their own click path.
    band_targets: Vec<thurbox::kernel::bands::Hit>,
    started: Instant,
    frames: u64,
    /// The last painted trees, per plugin index. A frame is skipped when every
    /// plugin returns what it returned last time and nothing else moved — the
    /// plugin-model equivalent of v1's `needs_redraw`.
    last_trees: Vec<Option<thurbox::kernel::node::Node>>,
    /// The last float each plugin painted, and where.
    ///
    /// What each chrome band painted last frame, and where. Bands have no tree
    /// to diff, so their cells are compared instead — see `render_band`.
    last_bands: std::collections::HashMap<Band, (Rect, Vec<ratatui::buffer::Cell>)>,
    /// Kept apart from `last_trees` because a float is rendered in its own pass at
    /// its own rect, so the two would overwrite each other for a plugin that did
    /// both. Its purpose is the same: settle the loop when nothing moved.
    last_floats: std::collections::HashMap<usize, (Rect, thurbox::kernel::node::Node)>,
    /// Floats that actually painted on the last frame.
    ///
    /// Distinct from `last_floats`, which is a settle cache and deliberately
    /// KEEPS a closed float's last tree to compare against when it reopens. This
    /// is the live answer to "is it on screen", so it is rebuilt every frame —
    /// reading the cache instead is what reported a closed modal as visible.
    drawn_floats: std::collections::HashSet<usize>,
    last_paint: Instant,
    /// The slot rects the arrangement placed last frame — the signal that the
    /// screen owes a full repaint, because they moved.
    ///
    /// A pane opening or closing reflows every column beside it, and a cell the
    /// diff believes it already printed is a cell it will not print again. That
    /// is fine while ratatui's model of a cell's width matches the terminal's,
    /// and grapheme clusters exist where it cannot: a regional-indicator flag is
    /// two columns to `unicode-width` and a different number to several
    /// emulators, so glyphs from the pane that just closed survive in the column
    /// that replaced it. `normalize_ambiguous_width` removes the one such
    /// disagreement it can (see `kernel::paint`); this covers the rest by
    /// marking the reflowed frame `paint::force_full_repaint`, which prints
    /// every cell of it.
    ///
    /// Deliberately NOT `Terminal::clear`: erasing flushes a blank screen and
    /// leaves the repaint to the next flush, so every toggle blinks the whole
    /// interface. The frame is the same either way — only the empty one in
    /// between is avoided.
    last_placed: Vec<thurbox::kernel::layout::SlotRect>,
    /// Set by anything that invalidates the screen outside the tree diff:
    /// input, a reload, a resize, a completed command.
    dirty: bool,
    /// Set while drawing when any plugin's tree differed from last frame.
    changed_this_frame: bool,
    /// Output stamp each surface was last painted at, keyed by surface name.
    /// What makes a quiet terminal settle rather than repaint every frame.
    last_output_painted: std::collections::HashMap<String, u64>,
    /// Every live pane's last-output stamp, summed, as of the last check.
    ///
    /// Compared each iteration so that new agent output *causes* a frame. The
    /// per-surface map above only decides whether a frame that is already
    /// happening counts as a change — which is why, without this, a printing
    /// agent was drawn at the 250ms floor rather than at once.
    last_output_gen: u64,
    /// Plugin holding an exclusive key grab this frame, if any.
    grabbed: Option<usize>,
    /// Programs plugins asked to be run, and what they printed.
    runs: thurbox::kernel::runs::RunStore,
    /// Every file of the interface, as of the last painted frame.
    ///
    /// Computed for the plugins that used to list it and kept because the
    /// settings modal's Interface tab lists it too — one join per frame, read
    /// by both.
    inventory: Vec<thurbox::kernel::inventory::Row>,
    /// Sessions already asked to relaunch, so a respawn is attempted once per
    /// session per run rather than every frame its window is still missing.
    respawned: std::collections::HashSet<String>,
    /// Watches soft-deleted sessions' undo windows close, so their agents are
    /// let go rather than left running forever.
    reaper: thurbox::kernel::reaper::Reaper,
    /// Whether a bookmark command is still running.
    ///
    /// Repository memory is the one read the flow can *change*, so its cached
    /// rows have to be dropped when a write lands — and only then, since
    /// re-reading while the worker is mid-write would publish the old list and
    /// look like the add did nothing.
    bookmark_in_flight: bool,
    /// Links found on each live session's screen, keyed by session, and the
    /// output stamp each answer was found at.
    ///
    /// Rebuilt for a session only when that session printed something. Finding
    /// them walks the whole grid cell by cell, and this runs on every frame *and*
    /// every input event — so a held-down key used to rescan every terminal on
    /// the screen per repeat, for answers that cannot have changed.
    links: std::collections::HashMap<String, Vec<(String, usize, usize)>>,
    link_stamps: std::collections::HashMap<String, u64>,
    /// What each terminal was showing when a search last asked, and the output
    /// generation it was read at. Empty while nothing is searching.
    content: std::collections::HashMap<String, String>,
    content_generation: Option<u64>,
    /// Where each interface file stands with the user, and the lock the answer
    /// was resolved against.
    ///
    /// Answering it reads and digests every file in the interface directory and
    /// parses `plugins.lock`, which is the wrong price to pay per keystroke: the
    /// answer changes only when the directory or a grant does, and both say so.
    /// The rows themselves are still assembled every publish — those depend on
    /// what is on screen this frame, which is cheap and does change.
    trust: std::collections::HashMap<String, thurbox::kernel::inventory::Trust>,
    /// Set when the directory, a grant or the disabled set moved, so the trust
    /// answers above are re-read.
    trust_stale: bool,
    /// Whether the perf counters are painted over the interface (F12).
    hud: bool,
    quit: bool,
}

/// Clamp a span into `floor..=cap`, tolerating a `cap` below the `floor`.
///
/// `u16::clamp` asserts `min <= max`, and every rect below takes its cap from the
/// space available — which on a short terminal is smaller than the floor the
/// content wants. The cap wins, because a rect must never exceed its parent.
/// The next terminal event, or `None` if none arrived within `timeout`.
///
/// The two crossterm calls belong together: a `poll` that says yes is what makes
/// the `read` non-blocking, and either can fail the same way.
fn next_event(timeout: Duration) -> std::io::Result<Option<Event>> {
    if !event::poll(timeout)? {
        return Ok(None);
    }
    event::read().map(Some)
}

fn clamp_span(value: u16, floor: u16, cap: u16) -> u16 {
    value.clamp(floor.min(cap), cap)
}

/// Where the reload-failure panel goes: the bottom of the screen, sized to the
/// message but never more than half the height.
fn error_area(area: Rect) -> Rect {
    // Half the screen, but never less than the three rows the message needs and
    // never more than the screen has. This panel is what a broken plugin shows
    // through, so it is the last thing that may itself panic.
    let cap = (area.height / 2).max(3).min(area.height);
    let height = clamp_span(area.height.saturating_sub(2), 3, cap);
    Rect {
        x: area.x,
        y: area.y.saturating_add(area.height.saturating_sub(height)),
        width: area.width,
        height: height.min(area.height),
    }
}

/// Where the perf HUD sits: the top-right corner, clamped to what there is.
///
/// The corner the session list is not in, so the pane you are most likely to be
/// watching while measuring is the one it does not cover.
fn hud_area(area: Rect) -> Rect {
    let width = 34.min(area.width);
    let height = 15.min(area.height);
    Rect {
        x: area.x + area.width - width,
        y: area.y,
        width,
        height,
    }
}

/// The cells of one rect of the frame, for comparing against the last one.
///
/// Clipped to the buffer's own area: a rect the arrangement produced is trusted
/// to be inside the frame, but reading out of bounds would panic rather than
/// merely mis-compare, and this runs on every painted frame.
fn read_cells(frame: &mut Frame, rect: Rect) -> Vec<ratatui::buffer::Cell> {
    // `Frame` exposes only `buffer_mut`, hence the mutable borrow for a read.
    // Taken once rather than per cell: this runs for every band on every
    // painted frame.
    let buffer = frame.buffer_mut();
    let rect = rect.intersection(buffer.area);
    let mut cells = Vec::with_capacity(usize::from(rect.width) * usize::from(rect.height));
    for y in rect.top()..rect.bottom() {
        for x in rect.left()..rect.right() {
            if let Some(cell) = buffer.cell(ratatui::layout::Position::new(x, y)) {
                cells.push(cell.clone());
            }
        }
    }
    cells
}

/// Compact µs for the HUD's narrow columns; `cli::perf` formats the same way.
fn fmt_hud_us(us: u64) -> String {
    if us < 1_000 {
        format!("{us}us")
    } else if us < 1_000_000 {
        format!("{:.1}ms", us as f64 / 1_000.0)
    } else {
        format!("{:.1}s", us as f64 / 1_000_000.0)
    }
}

/// Paint the counters.
///
/// Counts rather than timings, which is the whole point of `kernel::perf`: a
/// number that says "an idle loop painted no frames" is exact, where one that
/// says "idle was fast" is a coin toss on shared hardware.
fn render_hud(
    frame: &mut Frame,
    area: Rect,
    counters: &thurbox::kernel::perf::Snapshot,
    timings: &thurbox::kernel::perf::Timings,
) {
    use ratatui::style::{Color, Style};
    use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

    if area.width == 0 || area.height == 0 {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" perf ");
    let inner = block.inner(area);
    // Counters first — they are the exact half. The timings below them are
    // wall-clock and so only ever indicative (ADR-P11).
    let mut text = format!(
        "iterations {}\nframes     {}\nskipped    {}\nrenders    {}\nreused r/g {}/{}\nfailures   {}\nreloads    {}\n",
        counters.iterations,
        counters.frames,
        counters.skipped,
        counters.renders,
        counters.renders_skipped,
        counters.groups_reused,
        counters.failures,
        counters.reloads,
    );
    for (label, histogram) in [
        ("frame", &timings.frame),
        ("republ", &timings.republish),
        ("tick", &timings.tick),
    ] {
        text.push_str(&format!(
            "{label:<6} {} / {}\n",
            fmt_hud_us(histogram.percentile_us(50)),
            fmt_hud_us(histogram.max_us()),
        ));
    }
    match timings.slow_ops.iter_recent().next() {
        Some(op) => text.push_str(&format!("slow   {}:{}ms", op.name, op.ms)),
        None => text.push_str("slow   none"),
    }
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::Yellow)),
        inner,
    );
}

/// Flatten a crossterm key into what Lua is told about it.
fn to_press(key: &KeyEvent) -> KeyPress {
    let name = match key.code {
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => c.to_lowercase().to_string(),
        KeyCode::F(n) => format!("f{n}"),
        other => format!("{other:?}").to_lowercase(),
    };
    KeyPress {
        name,
        ch: match key.code {
            KeyCode::Char(c) => Some(c),
            _ => None,
        },
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thurbox::kernel::host::Float;

    /// The three intervals only mean anything in relation to each other, and
    /// nothing enforces that at the definitions.
    ///
    /// Output must wait longer than input, or the split buys nothing; and both
    /// must stay under the forced-redraw floor, or the floor becomes the real
    /// cadence and the constant above it is silently dead — a setting that
    /// looks tuned while doing nothing (ADR-P17).
    #[test]
    fn the_frame_floors_stand_in_the_right_order() {
        assert!(
            OUTPUT_FRAME_INTERVAL > MIN_FRAME_INTERVAL,
            "output is paced no slower than input, so the split is a no-op"
        );
        assert!(
            OUTPUT_FRAME_INTERVAL < FORCE_REDRAW_INTERVAL,
            "output waits past the forced-redraw floor, which then sets the \
             cadence instead — the constant would be dead"
        );
        assert!(
            MIN_FRAME_INTERVAL < FORCE_REDRAW_INTERVAL,
            "input waits past the forced-redraw floor"
        );
    }

    /// Startup says which interface loaded, but only when there is a question.
    ///
    /// The silent case is the one that matters: a release build on its own copy has
    /// nothing to disambiguate, and a greeting on every launch is noise. Everything
    /// else speaks, because it is either somebody's deliberate redirection (the most
    /// likely thing to have been forgotten), a fallback the user did not choose, or
    /// a dev build — where the checkout beside you contains an interface too and
    /// "which one am I running" is a real question.
    #[test]
    fn the_interface_directory_is_announced_only_when_it_is_in_doubt() {
        use thurbox::kernel::bundled::Chosen;
        let dir = Path::new("/home/me/.config/thurbox/ui");

        let own = directory_notice(dir, Chosen::UserCopy);
        if cfg!(dev_build) {
            let said = own.expect("a dev build says which interface it loaded");
            assert!(
                said.contains("THURBOX_UI_DIR"),
                "and how to change it: {said}"
            );
        } else {
            assert!(own.is_none(), "a release build on its own copy stays quiet");
        }

        // These three speak on any build.
        for chosen in [Chosen::Override, Chosen::Checkout, Chosen::Fallback] {
            let said = directory_notice(dir, chosen)
                .unwrap_or_else(|| panic!("{chosen:?} must be announced"));
            assert!(
                said.contains("interface from"),
                "{chosen:?} names the directory: {said}"
            );
        }
        // The fallback is not a preference and must not read like one.
        let fallback = directory_notice(dir, Chosen::Fallback).expect("announced");
        assert!(
            fallback.contains("could not be written"),
            "it says why, or it reads as a choice: {fallback}"
        );
    }

    /// Every chrome rect is derived from the space available, so each one has to
    /// hold at a size smaller than its own content floor. The error panel matters
    /// most: it is what a broken plugin shows through, and a five-row pane plus a
    /// syntax error under `ui/` is the ordinary state while authoring one.
    #[test]
    fn chrome_rects_survive_a_terminal_too_small_for_them() {
        for height in 0..=10u16 {
            for width in [0u16, 1, 3, 8, 40] {
                let area = Rect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                };

                let error = error_area(area);
                assert!(
                    error.height <= height && error.bottom() <= area.bottom(),
                    "error_area({width}x{height}) escaped: {error:?}"
                );

                let hud = hud_area(area);
                assert!(
                    hud.right() <= area.right() && hud.bottom() <= area.bottom(),
                    "hud_area({width}x{height}) escaped: {hud:?}"
                );

                for float in [
                    Float::default(),
                    Float {
                        cols: Some(200),
                        rows: Some(200),
                        ..Float::default()
                    },
                    Float {
                        cols: Some(0),
                        rows: Some(0),
                        ..Float::default()
                    },
                ] {
                    let rect = App::float_rect(area, float);
                    assert!(
                        rect.right() <= area.right() && rect.bottom() <= area.bottom(),
                        "float_rect({width}x{height}) escaped: {rect:?}"
                    );
                }
            }
        }
    }

    /// The cap wins over the floor, so a rect never exceeds the space it was
    /// given even when the content wants more.
    #[test]
    fn clamp_span_prefers_the_cap_to_the_floor() {
        assert_eq!(clamp_span(9, 3, 5), 5);
        assert_eq!(clamp_span(1, 3, 5), 3);
        assert_eq!(clamp_span(9, 3, 2), 2);
        assert_eq!(clamp_span(9, 3, 0), 0);
    }
}
