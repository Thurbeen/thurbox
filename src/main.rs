//! thurbox v2 — a session engine with a Lua-driven renderer.
//!
//! The kernel owns no pane. It resolves rects, calls plugins, paints what they
//! return, and refreshes a snapshot of the session engine on its own schedule.
//! Every surface you see — the session list included — is a file under `ui/`
//! that you can edit while this is running.
//!
//! Runs alongside the v1 binary; nothing in `src/app/` or `src/ui/` is touched.
//! See `openspec/changes/v2-plugin-kernel/`.

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
/// Editors save in bursts (write, rename, chmod); wait for the dust to settle.
const DEBOUNCE: Duration = Duration::from_millis(120);
/// Longest a frame may go unpainted when nothing has changed.
///
/// Covers time-driven content the diff cannot see — a spinner, a clock — and is
/// what turns an idle app from ~20 fps into ~4. v1 uses the same floor.
const FORCE_REDRAW_INTERVAL: Duration = Duration::from_millis(250);

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
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
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
    let (config, config_warnings) = thurbox::kernel::config::Config::load();
    let mut startup_notices: Vec<String> = config_warnings;
    if let Some(db) = snapshots_db() {
        startup_notices.extend(thurbox::session_ops::heal_active_extensions(&db));
        startup_notices.extend(thurbox::session_ops::ensure_builtin_hooks_extension(&db));
        for notice in &startup_notices {
            tracing::info!("{notice}");
        }
    }

    // The tmux heartbeat keeper, so a schedule keeps firing after this exits —
    // and, while it runs, at the keeper's 60s cadence rather than not at all.
    // Best-effort: a missing or old tmux just means no headless firing. Skipped
    // when the feature is off, exactly as v1 skips it.
    if thurbox::session::settings::global().features.automations {
        let cli = thurbox::agent::tmux::resolve_cli_binary();
        if let Err(e) = thurbox::agent::tmux::ensure_automation_heartbeat(&cli) {
            tracing::warn!("could not arm the automation heartbeat: {e}");
        }
    }

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
    let host = LuaHost::new(&ui_dir);

    // Resolved before the move, since `focus` indexes into the host.
    let initial_focus = focus_index_of(&host, "agent");

    let mut app = App {
        host,
        sources: thurbox::kernel::bundled::sources(&ui_dir),
        watcher: Watcher::new(&ui_dir)?,
        ui_dir,
        snapshots: SnapshotStore::open(),
        terminals: Terminals::new(),
        commands: CommandBus::new(),
        diffs: DiffStore::new(),
        repos: thurbox::kernel::repos::RepoStore::new(),
        metrics: Metrics::new(),
        clipboard: arboard::Clipboard::new().ok(),
        perf: Counters::default(),
        selection: None,
        notifier: {
            let settings = thurbox::session::settings::global();
            Notifier::new(settings.features.notifications, settings.notifications)
        },
        themes: Themes::load(snapshots_db().as_ref()),
        updates: thurbox::kernel::updates::Updates::start(config.features()),
        slot_selection: std::collections::HashMap::new(),
        visible_slots: std::collections::HashSet::new(),
        click_targets: Vec::new(),
        last_area: Rect::new(0, 0, 0, 0),
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
        last_floats: std::collections::HashMap::new(),
        drawn_floats: std::collections::HashSet::new(),
        last_paint: Instant::now(),
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
/// In order: `THURBOX_UI_DIR`, a `./ui` dev checkout, then the user's own copy
/// — materialized from the embedded interface on first run, preserving anything
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
    /// Whether the perf counters are painted over the interface (F12).
    hud: bool,
    quit: bool,
}

impl App {
    fn run(&mut self, mut terminal: DefaultTerminal) -> Result<(), Box<dyn Error>> {
        while !self.quit {
            Counters::bump(&self.perf.iterations);
            if self.watcher.changed() {
                self.reload_at = Some(Instant::now() + DEBOUNCE);
            }
            // The settings file, edited from outside. A live change is already in
            // force by the time this returns; the other two outcomes are the ones
            // worth saying out loud.
            match self.config.poll() {
                Some(thurbox::kernel::config::Reloaded::Live) => {
                    self.toast("settings reloaded".to_string());
                    self.dirty = true;
                }
                Some(thurbox::kernel::config::Reloaded::NeedsRestart) => {
                    self.toast("settings reloaded — some changes apply on restart".to_string());
                    self.dirty = true;
                }
                Some(thurbox::kernel::config::Reloaded::Failed(error)) => {
                    self.report(format!("settings: {error}"), Level::Error);
                }
                None => {}
            }
            if self.reload_at.is_some_and(|at| Instant::now() >= at) {
                self.reload_at = None;
                self.reload_interface();
                self.refresh_sources();
                self.collect_declarations();
                self.clamp_focus();
                self.dirty = true;
            }

            // Refreshed here, on the kernel's schedule — never inside a plugin
            // call, which is what keeps every plugin read immediate.
            // Commands plugins issued last frame, handed to the bus. Draining
            // here rather than inside the render call is what keeps `command()`
            // a queue push and nothing more.
            for command in self.host.drain_commands() {
                // Theme is applied here rather than dispatched: it mutates
                // in-process state a worker thread cannot reach, and it is
                // instant, so nothing is gained by making it asynchronous.
                match &command {
                    thurbox::kernel::command::Command::Theme { name } => {
                        if let Err(e) = self.themes.select(name, snapshots_db().as_ref()) {
                            self.report(e, Level::Error);
                        }
                        continue;
                    }
                    // Copy needs the vt100 screen and the tty, neither of
                    // which a worker thread can reach.
                    // Open a link, or copy it where nothing can open one —
                    // a remote session or a bare tty has no browser, and
                    // spawning an opener there goes nowhere.
                    thurbox::kernel::command::Command::OpenLink { url } => {
                        let url = url.clone();
                        self.open_or_copy_link(&url);
                        continue;
                    }
                    // Editing the interface's own files. Applied here because
                    // it is two file operations and the watcher turns the
                    // write into a reload — a worker would only add a race.
                    thurbox::kernel::command::Command::Plugin { file, edit } => {
                        let (file, edit) = (file.clone(), *edit);
                        self.apply_plugin_edit(&file, edit);
                        continue;
                    }
                    // Focus is the loop's own state, so it is applied here.
                    thurbox::kernel::command::Command::Focus { plugin, toggle } => {
                        if let Some(index) = self.host.index_of(plugin) {
                            let position = self
                                .host
                                .focusable()
                                .iter()
                                .position(|candidate| *candidate == index);
                            // Already here, and asked to toggle: go back where the
                            // last focus change came from. That memory is
                            // `focus_return`, which is the same one `Esc` uses — so
                            // a pane reached by its own key leaves by either.
                            if *toggle && position == Some(self.focus) {
                                let back = self.focus_return;
                                if self.host.focusable().get(back).is_some() {
                                    self.focus_return = self.focus;
                                    self.focus = back;
                                    self.dirty = true;
                                }
                            } else {
                                // Through `focus_plugin`, not by assigning `focus`:
                                // it records where focus came from and refuses a
                                // pane focus cannot rest on. Assigning directly
                                // skipped both, so a pane reached from a plugin
                                // command could not be left with `Esc` — the user
                                // had to walk out with the focus cycle.
                                self.focus_plugin(index);
                                self.dirty = true;
                            }
                        }
                        continue;
                    }
                    thurbox::kernel::command::Command::Shell { session } => {
                        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
                        let session = session.clone();
                        // Resolved from the snapshot rather than the live
                        // `Session`: an adopted one carries no cwd of its own.
                        let cwd = self
                            .snapshots
                            .current()
                            .session(&session)
                            .and_then(|row| self.terminals.launch_cwd(row));
                        if let Err(e) =
                            self.terminals
                                .open_shell(&session, rows, cols, cwd.as_deref())
                        {
                            self.report(format!("could not open a shell: {e}"), Level::Error);
                        } else {
                            // Its window outlives this process, so the id has to
                            // as well — otherwise the next start forgets the
                            // shell and orphans the window it left running.
                            self.remember_shell(&session);
                        }
                        continue;
                    }
                    thurbox::kernel::command::Command::Program {
                        owner,
                        name,
                        program,
                        argv,
                        close,
                    } => {
                        self.apply_program(owner, name, program, argv, *close);
                        continue;
                    }
                    // The editor wants a controlling tty, which only this
                    // thread can hand it — see `Command::Editor`.
                    thurbox::kernel::command::Command::Editor { session } => {
                        let dirs = self
                            .snapshots
                            .current()
                            .session(session)
                            .map(|row| row.member_dirs.clone())
                            .unwrap_or_default();
                        self.toast(match open_editor(&mut terminal, &dirs) {
                            Ok(message) => message,
                            Err(e) => e,
                        });
                        continue;
                    }
                    thurbox::kernel::command::Command::Diff { session } => {
                        // Drop the answer; the request at the top of the next
                        // iteration recomputes it on the worker.
                        self.diffs.invalidate(session);
                        self.dirty = true;
                        continue;
                    }
                    thurbox::kernel::command::Command::Copy { session } => {
                        match self.terminals.visible_text(session) {
                            Some(text) => {
                                let outcome = thurbox::clipboard::copy(
                                    &text,
                                    self.clipboard.as_mut(),
                                    thurbox::session::settings::global().clipboard.provider,
                                );
                                self.toast(match outcome {
                                    Ok(route) => {
                                        format!(
                                            "copied {} lines{}",
                                            text.lines().count(),
                                            route.toast_suffix()
                                        )
                                    }
                                    Err(e) => format!("copy failed: {e}"),
                                });
                            }
                            None => self.report(
                                "nothing to copy — this session has no live terminal yet",
                                Level::Error,
                            ),
                        }
                        continue;
                    }
                    // Settings live in the registry, which is in-process too.
                    thurbox::kernel::command::Command::Setting { key, value } => {
                        let (plugin, id) = key.split_once('.').unwrap_or((key.as_str(), ""));
                        if let Err(e) = self.registry.set_setting(plugin, id, value.clone()) {
                            self.report(e, Level::Error);
                        }
                        continue;
                    }
                    // Repository memory is a read the flow also writes, so
                    // note the write and drop the cached rows when it lands.
                    thurbox::kernel::command::Command::Bookmark { .. } => {
                        self.bookmark_in_flight = true;
                    }
                    _ => {}
                }
                self.dispatch_tracked(command.clone());
            }
            // A completed command changes what the rows say, so refresh at once
            // rather than waiting out the interval — that is what makes a
            // delete feel immediate instead of arriving up to 400ms later.
            if self.commands.poll() {
                self.report_finished_commands();
                // Wait for the last one: two adds in quick succession would
                // otherwise re-read between them and publish a list missing the
                // second.
                if self.bookmark_in_flight
                    && !self
                        .commands
                        .inflight()
                        .iter()
                        .any(|item| item.kind == "bookmark")
                {
                    self.bookmark_in_flight = false;
                    self.repos.invalidate_bookmarks();
                }
                self.snapshots.refresh();
                self.dirty = true;
            } else {
                self.snapshots.refresh_if_due();
            }
            // Attach to any session that gained a pane, and drop the ones that
            // went away. Sized to the screen as a starting point; each surface
            // resizes its own pane to its real rect when it paints.
            let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
            self.terminals.sync(self.snapshots.current(), rows, cols);
            // A remote agent's hooks cannot call `thurbox-cli` — they set a tmux
            // pane option, which arrives here over the control-mode
            // subscription. Draining it into the same columns a local signal
            // writes is the whole of remote status: the dot, the acknowledgment
            // and the notification are all downstream of that one write.
            // New agent output, noticed the way v1 notices it: one lock-free sum
            // of atomics per iteration, marking the screen dirty. Without this a
            // printing agent is drawn only when the 250ms floor comes round.
            let output_gen = self.terminals.output_generation();
            if output_gen != self.last_output_gen {
                self.last_output_gen = output_gen;
                self.dirty = true;
            }
            // A session whose agent is gone gets it back, which is what v1 does
            // at restore and what makes a session survive a reboot.
            self.respawn_missing_agents();
            // And the shell it had open, whose window outlived the interface.
            self.readopt_shells();
            // Programs plugins asked for. Served here because resolving a
            // session to a directory and a machine is the loop's knowledge, not
            // a store's — the store owns the bounds and the queue.
            self.serve_runs();
            let events = self.terminals.drain_hook_events();
            if !events.is_empty() && self.snapshots.apply_hook_states(events, Instant::now()) > 0 {
                self.dirty = true;
            }
            // The selected session's diff, computed on a worker. Requesting is
            // idempotent, so calling it every frame costs nothing after the
            // first.
            //
            // Driven by the *selection*, not by `focused_session`: the latter is
            // re-derived each frame from the focused plugin's session surface, so
            // only a pane drawing a terminal ever asked for one. A pane that shows
            // a diff draws no terminal by definition, and would never be handed the
            // thing it exists to show. The selection is what "the session the user
            // is looking at" actually means, and the session list publishes it.
            let wanted = self
                .host
                .shared_string("selected")
                .or_else(|| self.focused_session.clone());
            if let Some(id) = wanted {
                if let Some(row) = self.snapshots.current().session(&id) {
                    if let Some(cwd) = row.cwd.clone() {
                        // `base_branch`, never `branch`. Against its own branch a
                        // session diffs to nothing, and publishing that as `ready`
                        // is a confident wrong answer.
                        self.diffs
                            .request(&id, cwd, row.base_branch.clone(), &row.backend);
                    }
                }
            }
            if self.diffs.poll() {
                self.dirty = true;
            }

            // The creation flow's reads. It asks by leaving a key in `store`,
            // which is written the moment its handler runs, so a request made
            // on a keystroke is served on the very next iteration. Requesting
            // is idempotent, so asking every iteration costs three lookups.
            let wants = self.repo_wants();
            self.repos.serve(&wants);
            if self.repos.poll() {
                self.dirty = true;
            }

            // A session that has stayed deleted past its undo window keeps its
            // worktrees and loses its agent — the reap v1 does when the undo
            // expires.
            for session in self
                .reaper
                .observe(self.snapshots.current(), Instant::now())
            {
                self.commands
                    .dispatch(thurbox::kernel::command::Command::Reap { session });
            }

            // An update that replaced binaries is worth saying once; a check that
            // merely found a release shows up in the header instead.
            if let Some(message) = self.updates.poll() {
                self.toast(message);
            }

            // Machine, statusline and account metrics. Both calls are gated
            // internally (an interval, and one sample in flight at a time), so
            // running them every iteration costs a comparison.
            self.metrics.sample(self.metric_subjects());
            if self.metrics.poll() {
                self.dirty = true;
            }

            // A focus request from another process: a clicked notification's
            // action callback, or `thurbox-cli session focus`, both of which
            // leave a row in the database for whoever is running the interface.
            // Taken atomically, so two instances cannot both claim it.
            if let Some(id) = self.snapshots.take_focus_request() {
                self.host.set_shared_string("focus_session", &id);
                if let Some(index) = self.host.index_of("agent") {
                    if let Some(position) = self.host.focusable().iter().position(|i| *i == index) {
                        self.focus = position;
                    }
                }
                self.dirty = true;
            }

            // Acknowledge a finished turn on the way out of it: moving the
            // selection off a `done` session is what retires its filled dot.
            // Read from the store because the selection belongs to the session
            // list, which is a plugin.
            let selected = self.host.shared_string("selected");
            if selected != self.last_selected_session {
                if let Some(left) = self.last_selected_session.take() {
                    self.snapshots.acknowledge(&left);
                }
                self.last_selected_session = selected;
                self.dirty = true;
            }

            // Notify on the blocked edge, observed where status is read so the
            // rule cannot drift from what the list shows.
            self.notifier
                .observe(self.snapshots.current(), self.focused_session.as_deref());

            // Demand-driven: paint when something changed, or when the floor
            // elapsed. `draw` diffs each plugin's tree and clears `dirty` when
            // every one matched, so an idle screen settles at the floor.
            let since_paint = self.last_paint.elapsed();
            let due = self.dirty && since_paint >= MIN_FRAME_INTERVAL;
            if due || since_paint >= FORCE_REDRAW_INTERVAL {
                // Published HERE rather than every iteration: a plugin only
                // reads `thurbox.*` while it renders, so rebuilding those
                // tables on a tick that paints nothing is pure waste. At the
                // 10ms poll above that was 100 rebuilds a second to feed a
                // screen that redraws four times.
                self.republish();
                let painted = terminal.draw(|frame| self.draw(frame))?;
                // Cloned so the borrow of `terminal` ends here: the two
                // corrections below both need it back.
                let buffer = painted.buffer.clone();
                // A modal captures input, so it owns the caret — or nothing
                // does. The panes underneath still draw, and one with a text
                // field claims the cursor as it goes; a frame that ends with a
                // cursor position SHOWS it, so it would blink behind the modal,
                // which reads as the screen refreshing wrongly rather than as a
                // misplaced cursor. Corrected after the frame because a `Frame`'s
                // cursor can be set but never unset, and the modal draws last.
                if self.modals.is_open() {
                    match self.modals.caret() {
                        Some(position) => {
                            terminal.show_cursor()?;
                            terminal.set_cursor_position(position)?;
                        }
                        None => terminal.hide_cursor()?,
                    }
                }
                // After the backend flushed, so the escapes cannot interleave
                // with ratatui's own output.
                self.paint_terminal_hyperlinks(&buffer);
                self.last_paint = Instant::now();
                self.frames += 1;
                Counters::bump(&self.perf.frames);
            } else {
                Counters::bump(&self.perf.skipped);
            }

            // Drain EVERY pending event, not one per iteration.
            //
            // Reading one event per 10ms poll cannot keep up with a mouse: a
            // single drag emits events far faster than 100/s, so the queue grew
            // without bound. That is felt as an unresponsive mouse, and the
            // backlog outlives the process — the leftover reports are what
            // printed `\x1b[<35;92;31M` into the terminal afterwards.
            let mut first = true;
            while if first {
                first = false;
                event::poll(TICK)?
            } else {
                event::poll(Duration::ZERO)?
            } {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        // A handler reads `thurbox.*` too, and input is rare
                        // enough that keeping it current costs nothing.
                        self.republish();
                        self.on_key(&key);
                        self.dirty = true;
                    }
                    // Dropped rather than merely uncaptured when the feature is
                    // off, so the flag stays authoritative even if a terminal
                    // reports mouse events unasked. v1 does the same in
                    // `App::update`.
                    Event::Mouse(mouse) if self.mouse => {
                        self.republish();
                        self.on_mouse(mouse);
                        self.dirty = true;
                    }
                    // A bracketed paste from the terminal itself. Routed to
                    // whatever has focus, exactly as `ctrl+v` is: a modal's
                    // text field if one is open, else the focused terminal.
                    Event::Paste(text) => {
                        self.republish();
                        self.on_paste(text);
                        self.dirty = true;
                    }
                    Event::Resize(..) => self.dirty = true,
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// What the metrics sampler should measure this round: every session in the
    /// snapshot, with the handle needed to resolve its pane's root process.
    fn metric_subjects(&self) -> Vec<Subject> {
        self.snapshots
            .current()
            .sessions
            .iter()
            .map(|row| Subject {
                session: row.id.clone(),
                agent_session_id: row.agent_session_id.clone(),
                pane: self.terminals.backend_handle(&row.id),
                agent: row.agent.clone(),
                host: row.remote_host.clone(),
            })
            .collect()
    }

    /// Re-read where each file of the interface came from.
    ///
    /// On reload, and after this process edits one — the two moments the answer
    /// can have changed. Not per frame: it digests every file.
    fn refresh_sources(&mut self) {
        self.sources = thurbox::kernel::bundled::sources(&self.ui_dir);
    }

    /// Rebuild everything a plugin can read.
    ///
    /// Called just before a paint and before dispatching input — the only two
    /// moments Lua runs — rather than once per loop iteration.
    fn republish(&mut self) {
        // Links are scanned out of each terminal's screen, so this is part of
        // publishing rather than a standing cost of the loop.
        let mut links = std::collections::HashMap::new();
        for row in &self.snapshots.current().sessions {
            let found = self.terminals.links(&row.id);
            if !found.is_empty() {
                links.insert(row.id.clone(), found);
            }
        }
        // Terminal text, but only while something is searching it. The scan is
        // the same walk `links` just made, so serving it always would not be
        // ruinous — it would simply be paid by every interface that never
        // searches, which is the argument for asking.
        let content = match self
            .host
            .shared_string(thurbox::kernel::terminal::WANT_CONTENT)
            .filter(|query| !query.is_empty())
        {
            Some(_) => {
                let ids: Vec<String> = self
                    .snapshots
                    .current()
                    .sessions
                    .iter()
                    .map(|row| row.id.clone())
                    .collect();
                self.terminals.screens(&ids)
            }
            None => std::collections::HashMap::new(),
        };
        // Generation-gated, so an idle session costs one atomic load.
        let meta = self.terminals.meta().clone();
        let attach_errors = self.terminals.failures();
        let inflight = self.commands.inflight();
        let focus = self
            .host
            .focusable()
            .get(self.focus)
            .and_then(|index| self.host.plugins.get(*index))
            .map(|plugin| plugin.name.to_string());
        // What is on screen is known from the frame just painted, which is what
        // makes "its slot is not in the arrangement" an observation rather than
        // a guess: placement depends on the terminal's current size.
        let visible: std::collections::HashSet<usize> = (0..self.host.plugins.len())
            .filter(|index| self.is_visible_plugin(*index))
            .collect();
        let ui_dir = self.ui_dir.clone();
        let registry = &self.registry;
        // Read once per inventory pass rather than per file: it is one small TOML,
        // and a per-file read would make the cost scale with the directory.
        let plugin_lock = thurbox::kernel::packages::read_lock(&ui_dir).unwrap_or_default();
        self.inventory = thurbox::kernel::inventory::rows(
            &self.host.plugins,
            &self.sources,
            &visible,
            &self.visible_slots,
            self.host.error.as_deref(),
            // Trust is keyed by absolute path, and drift is answered by reading
            // the file — cheap, and only for the handful that declare anything.
            // An INSTALLED file is judged differently (its `src@version` as well as
            // its contents), and `trust_of` owns that split so no caller can check
            // only one half of it.
            &|path| thurbox::kernel::packages::trust_of(&ui_dir, path, &plugin_lock, registry),
            &|path| registry.is_disabled(&ui_dir.join(path).to_string_lossy()),
        );
        let inventory = std::mem::take(&mut self.inventory);
        let ui_dir = self.ui_dir.display().to_string();
        if let Err(e) = self.host.publish(&thurbox::kernel::host::Published {
            snapshot: self.snapshots.current(),
            attach_errors: &attach_errors,
            inflight: &inflight,
            themes: &self.themes,
            registry: &self.registry,
            diffs: &self.diffs,
            links: &links,
            content: &content,
            meta: &meta,
            metrics: &self.metrics,
            status_rows: self.status_rows(),
            can_open: browser_available(),
            focus: focus.as_deref(),
            hovered: self.hovered.as_ref(),
            inventory: &inventory,
            ui_dir: &ui_dir,
            settings: self.config.in_force(),
            repos: &self.repos,
            wants: &self.repo_wants(),
        }) {
            self.layout_error = Some(format!("publishing the snapshot failed: {e}"));
        }
        // Put it back: the settings modal's Interface tab lists the same rows,
        // and recomputing them there would mean a second join against a frame
        // that has already been painted.
        self.inventory = inventory;
    }

    /// What the creation flow is asking about, read off the shared `store`.
    ///
    /// Absent means not asking, which is why the local machine is an *empty*
    /// host rather than an absent one: a closed flow must cost nothing, and
    /// asking about local has to be expressible (`kernel::repos::Wants`).
    fn repo_wants(&self) -> thurbox::kernel::repos::Wants {
        use thurbox::kernel::repos::{Wants, WANT_BOOKMARKS, WANT_BRANCHES, WANT_BROWSE};
        Wants::new(
            self.host.shared_string(WANT_BOOKMARKS),
            self.host.shared_string(WANT_BROWSE),
            self.host.shared_string(WANT_BRANCHES),
        )
    }

    /// Surface a command that failed, once.
    ///
    /// A failure lingers in the in-flight list briefly so a pane can draw it,
    /// which is not the same as telling the user: the row it appears on may not
    /// even be visible. The message band is where a failure is *reported*, at
    /// error severity, and each is reported once — the linger would otherwise
    /// re-raise it on every poll.
    fn report_finished_commands(&mut self) {
        // Failures, once each. The linger that lets a pane draw a failed row
        // would otherwise re-report it on every poll.
        let failures: Vec<(u64, String)> = self
            .commands
            .inflight()
            .iter()
            .filter(|item| !self.reported_failures.contains(&item.id))
            .filter_map(|item| item.error.clone().map(|error| (item.id, error)))
            .collect();
        for (id, error) in failures {
            self.reported_failures.insert(id);
            if let Some(tracked) = self.tracked_commands.get_mut(&id) {
                let (kind, label) = (tracked.kind, tracked.label.clone());
                tracked.failed = true;
                self.report(
                    thurbox::kernel::messages::failed(kind, label.as_deref(), &error),
                    Level::Error,
                );
            }
        }

        // Anything that left the list without having failed is a command that
        // worked. Reported because most of them change something the screen does
        // not obviously show — a sync, a restart, a worktree — and silence after
        // a keystroke reads as the key not having worked.
        let live: std::collections::HashSet<u64> = self
            .commands
            .inflight()
            .iter()
            .map(|item| item.id)
            .collect();
        let finished: Vec<(u64, TrackedCommand)> = self
            .tracked_commands
            .iter()
            .filter(|(id, _)| !live.contains(id))
            .map(|(id, tracked)| (*id, tracked.clone()))
            .collect();
        for (id, tracked) in finished {
            self.tracked_commands.remove(&id);
            if tracked.failed {
                continue;
            }
            // A command that replaced the session's pane must be followed by
            // letting go of the old one, or the interface keeps painting a pane
            // that no longer exists — a session that looks restarted and takes no
            // keys. This is told rather than detected: see `Terminals::forget`.
            if matches!(tracked.kind, "restart" | "restore") && !tracked.session.is_empty() {
                self.terminals.forget(&tracked.session);
                self.dirty = true;
            }
            if let Some(message) =
                thurbox::kernel::messages::done(tracked.kind, tracked.label.as_deref())
            {
                self.toast(message);
            }
        }
        self.reported_failures.retain(|id| live.contains(id));
    }

    /// `area` minus the rows the chrome bands were placed on.
    ///
    /// Bands are only ever full-width single rows at the top or the bottom, so
    /// this is a shrink rather than a general subtraction: a band placed between
    /// two panes would not be excluded, and nothing places one there.
    fn content_area(&self, area: Rect, placed: &[thurbox::kernel::layout::SlotRect]) -> Rect {
        let mut top = area.y;
        let mut bottom = area.y.saturating_add(area.height);
        for slot in placed {
            if Band::from_slot(&slot.slot).is_none() {
                continue;
            }
            let rect = slot.rect;
            if rect.y <= top {
                top = top.max(rect.y.saturating_add(rect.height));
            } else if rect.y.saturating_add(rect.height) >= bottom {
                bottom = bottom.min(rect.y);
            }
        }
        Rect {
            x: area.x,
            y: top,
            width: area.width,
            height: bottom.saturating_sub(top),
        }
    }

    /// Rows the message band needs: one while there is a live message or work in
    /// flight, none otherwise.
    fn status_rows(&self) -> u16 {
        let live_message = self
            .status
            .as_ref()
            .is_some_and(|(_, _, at)| at.elapsed() < STATUS_TTL);
        u16::from(live_message || !self.commands.inflight().is_empty())
    }

    fn draw(&mut self, frame: &mut Frame) {
        self.errors.clear();
        self.focused_session = None;
        self.focused_surface = None;
        self.changed_this_frame = false;
        self.click_targets.clear();
        self.band_targets.clear();
        // Surface rects are recorded while painting, so they are cleared with the
        // other per-frame hit-testing state rather than left to go stale.
        self.terminals.forget_rects();
        let area = frame.area();
        self.last_area = area;

        // 1. The arrangement decides where slots go — before any plugin runs.
        let region = match self.host.arrangement(area.width, area.height) {
            Ok(region) => {
                self.layout_error = None;
                region
            }
            Err(e) => {
                self.layout_error = Some(e);
                // A broken arrangement must not take the plugins with it, so
                // fall back to giving everything to the centre.
                thurbox::kernel::layout::Region {
                    slot: Some("center".to_string()),
                    ..Default::default()
                }
            }
        };
        let placed = resolve(&region, area);
        self.visible_slots = placed.iter().map(|s| s.slot.clone()).collect();
        // Closing the column you were standing in must not strand focus on it.
        // Corrected here rather than at key time because the toggle only shows
        // up in the arrangement on the frame AFTER it is flipped.
        //
        // Asked as `can_focus`, not `is_drawn`, and the difference is load-
        // bearing: this runs BEFORE the switch slot below records its selection,
        // so a pane focused by its own opening chord is still an alternate at
        // this point. Judging it drawn would move focus off it every time —
        // which is exactly what made the plugins pane unreachable by `F11`.
        let focusable_now = self.host.focusable();
        if focusable_now
            .get(self.focus)
            .is_some_and(|index| !self.can_focus_plugin(*index))
        {
            self.cycle_focus(1);
        }

        // 2. Each slot divides its rect among its plugins, by their DECLARED
        //    sizes — which is why this can happen before rendering.
        let focusable = self.host.focusable();
        let focused_plugin = focusable.get(self.focus).copied();

        for slot in &placed {
            // A band is a slot the KERNEL occupies: the arrangement names and
            // places it exactly as it does a pane's region, and the contents come
            // from here rather than from any plugin. Drawing one runs no Lua, so
            // no plugin can break it.
            if let Some(band) = Band::from_slot(&slot.slot) {
                self.render_band(frame, slot.rect, band);
                continue;
            }
            let members = self.host.in_slot(&slot.slot);
            if members.is_empty() {
                continue;
            }
            match self.host.slot_mode(&slot.slot) {
                // One visible occupant; the rest are alternatives waiting to be
                // selected. The focused plugin wins, so tabbing into a pane
                // brings it forward.
                SlotMode::Switch => {
                    let visible = members
                        .iter()
                        .position(|index| focused_plugin == Some(*index))
                        .unwrap_or_else(|| {
                            self.slot_selection
                                .get(&slot.slot)
                                .copied()
                                .unwrap_or(0)
                                .min(members.len().saturating_sub(1))
                        });
                    self.slot_selection.insert(slot.slot.clone(), visible);
                    if let Some(&index) = members.get(visible) {
                        self.draw_plugin(frame, index, slot.rect, focused_plugin == Some(index));
                    }
                }
                SlotMode::Stack => {
                    let sizes: Vec<_> =
                        members.iter().map(|i| self.host.plugins[*i].size).collect();
                    let rects =
                        thurbox::kernel::layout::divide_slot(slot.rect, Axis::Vertical, &sizes, 0);
                    for (nth, &index) in members.iter().enumerate() {
                        let Some(&rect) = rects.get(nth) else {
                            continue;
                        };
                        if rect.width == 0 || rect.height == 0 {
                            continue;
                        }
                        self.draw_plugin(frame, index, rect, focused_plugin == Some(index));
                    }
                }
            }
        }

        // 2b. Floats, above the arrangement. A plugin only floats on the
        //     frames it returns a float node, so a modal opens and closes with
        //     no separate channel for the kernel to keep in sync.
        self.grabbed = None;
        self.drawn_floats.clear();
        for index in self.host.floating() {
            let probe = RenderContext {
                width: area.width,
                height: area.height,
                focused: true,
                elapsed: self.started.elapsed().as_secs_f64(),
                frame: self.frames,
            };
            let Ok(rendered) = self.host.render(index, probe) else {
                continue;
            };
            let Some(float) = rendered.float else {
                // Not floating this frame: a closed modal draws nothing at all.
                continue;
            };
            let rect = Self::float_rect(area, float);
            // Dim what is beneath, so the float reads as above rather than
            // merely drawn later.
            frame.render_widget(ratatui::widgets::Clear, rect);
            let mut hits = Vec::new();
            paint::render_recording(frame, rect, &rendered.node, &self.terminals, &mut hits);
            // Recorded after every pane, so a float's targets win the overlap
            // for the same reason its cells do. Its whole rect goes in first —
            // a click that misses every button still lands on the modal rather
            // than on the pane it covers.
            self.push_targets(index, Some(rect), hits);
            // The topmost float takes input; later plugins win, matching the
            // order they are painted in.
            self.grabbed = Some(index);
            // Settled like a pane, by comparing what it drew — not marked changed
            // for simply being open. Unconditional, an open float held `dirty` set
            // forever, so the whole interface rebuilt every Lua tree at the frame
            // cap for as long as the creation wizard was up. A float that really
            // does animate still repaints: the 250 ms floor paints it, its tree
            // differs, and that marks the change.
            let unchanged = self
                .last_floats
                .get(&index)
                .is_some_and(|(last, node)| *last == rect && node == &rendered.node);
            if !unchanged {
                self.changed_this_frame = true;
            }
            self.last_floats
                .insert(index, (rect, rendered.node.clone()));
            self.drawn_floats.insert(index);
        }

        // System modals, above the arrangement and above every plugin float —
        // they are not in the layout, so nothing below could shrink them and
        // they shrink nothing. Deliberately NOT marked as a change: a modal's
        // content only moves when a key arrives, and that already marks the
        // screen dirty, so an open modal settles like everything else instead
        // of repainting sixty times a second.
        if self.modals.is_open() {
            // Given the area WITHOUT the chrome bands, for two reasons that are
            // really one: a modal dims everything it is handed
            // (`modals::chrome::modal_frame`), and the bands are the surfaces
            // that must stay readable while it is up. The message band exists so
            // an error is never hidden — dimming it works against its own
            // purpose — and the footer's chips read as buttons only by their
            // fill, so dimming turned every one of them into plain text until
            // the modal closed. Centring inside this area also keeps a tall
            // modal off the bands rather than merely darkening them.
            let content = self.content_area(area, &placed);
            let ui_dir = self.ui_dir.display().to_string();
            let inventory = std::mem::take(&mut self.inventory);
            self.modals.render(
                frame,
                content,
                &self.registry,
                &self.themes,
                self.config.in_force(),
                thurbox::kernel::modals::interface::Files {
                    rows: &inventory,
                    dir: &ui_dir,
                },
            );
            self.inventory = inventory;
        }

        // The counters, above the floats: a diagnostic a modal could cover
        // would be useless exactly when a modal is what you are diagnosing.
        if self.hud {
            render_hud(frame, hud_area(area), &self.perf.read());
            // The counters move on every iteration, so the HUD is never
            // settled — while it is up, the loop keeps painting.
            self.changed_this_frame = true;
        }

        // The selection is painted last, over whatever is beneath — it is a
        // highlight on cells that already exist, not a widget of its own.
        // A press that nothing else consumed arms a selection so the *same*
        // press can start a drag — but a press that never moved is a CLICK, and
        // painting it would reverse the one cell under the pointer for no reason
        // the user can see. So an unextended selection is armed but not drawn,
        // and contributes no copyable text.
        if let Some(selection) = self
            .selection
            .clone()
            .filter(|selection| selection.anchor != selection.cursor)
        {
            // v1's `apply_selection_highlight`: the theme's own selection pair,
            // not reverse video. REVERSED inverts whatever each cell already
            // had, so a selection over styled text came out a different colour
            // per span and matched no theme; naming the roles means the
            // selection looks the same everywhere and follows every palette.
            let style = self.themes.selection_style();
            thurbox::session::selection::highlight_buffer(frame.buffer_mut(), &selection, style);
            // Deliberately NOT a change, for the reason the system modals below
            // are not: the highlight is already in this frame's buffer, and it is
            // re-applied on every later paint. Moving it takes a mouse event, which
            // marks the screen dirty by itself -- so marking here only kept the
            // loop at the frame cap for as long as anything stayed selected.

            // Read here, not at copy time: outside a terminal the only record of
            // what is selected is the frame we are holding.
            let from_grid = self
                .surface_at(selection.pane.rect().x, selection.pane.rect().y)
                .filter(|(_, rect)| *rect == selection.pane.rect())
                .and_then(|(session, rect)| {
                    self.terminals
                        .selected_text(&session, &selection, (rect.x, rect.y))
                });
            let text = from_grid.unwrap_or_else(|| {
                thurbox::session::selection::extract_text_from_buffer(
                    frame.buffer_mut(),
                    &selection,
                )
            });
            self.selected_text = (!text.trim().is_empty()).then_some(text);
        } else {
            self.selected_text = None;
        }

        // An expired message is a change, so it is swept before the settle check
        // below. Where a live one is DRAWN is the message band's business — it
        // used to be painted over the centre pane's bottom border, because a
        // Lua-owned arrangement left no other row spare.
        if self
            .status
            .as_ref()
            .is_some_and(|(_, _, at)| at.elapsed() >= STATUS_TTL)
        {
            self.status = None;
            self.changed_this_frame = true;
        }

        // Settled: nothing moved this frame, so stop repainting until
        // something does or the floor elapses.
        self.dirty = self.changed_this_frame;

        // 3. A reload failure outranks anything a stale-but-working plugin says.
        //    The floor comes last of the three: while it is up the host is the
        //    bundled copy and reports no error of its own, so without it the
        //    interface would silently be something other than what the user
        //    edited.
        if let Some(error) = self
            .host
            .error
            .clone()
            .or_else(|| self.layout_error.clone())
            .or_else(|| self.floor.clone())
        {
            paint::render_error(frame, error_area(area), "reload failed", &error);
        }

        // Last, so it covers every pane, float and toast painted above.
        self.repaint_theme_background(frame);
    }

    /// Repaint cells that fell back to terminal-default colours with the active
    /// theme's background and primary text.
    ///
    /// v1 `App::repaint_theme_background`. Without it a theme only tints what a
    /// pane explicitly styled, and every gap between panes keeps the user's own
    /// terminal background — so 30 of the 36 presets rendered as a patchwork.
    ///
    /// Themes whose `app_bg` is `Reset` (the ANSI-based Default preset) skip it
    /// deliberately, so they keep honouring the terminal palette.
    fn repaint_theme_background(&self, frame: &mut Frame) {
        let palette = &self.themes.active().palette;
        if palette.app_bg == ratatui::style::Color::Reset {
            return;
        }
        let area = frame.area();
        let buf = frame.buffer_mut();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                let Some(cell) = buf.cell_mut(ratatui::layout::Position::new(x, y)) else {
                    continue;
                };
                if cell.bg == ratatui::style::Color::Reset {
                    cell.bg = palette.app_bg;
                }
                if cell.fg == ratatui::style::Color::Reset {
                    cell.fg = palette.text_primary;
                }
            }
        }
    }

    /// Render one plugin, painting an error panel in ITS OWN rect on failure.
    ///
    /// This is the isolation rule made concrete: a plugin that throws costs its
    /// own pane and nothing else. Its neighbours keep drawing, and its state
    /// survives for when the file is fixed.
    /// Centre a rect taking `width_pct` x `height_pct` of `area`.
    fn float_rect(area: Rect, float: thurbox::kernel::host::Float) -> Rect {
        // Cells when the plugin knows them, else a share of the screen. A modal
        // whose height follows its content — v1's pickers, all of them — can only
        // say so in cells; clamping keeps an over-ambitious one on screen.
        let width = clamp_span(
            float
                .cols
                .unwrap_or_else(|| (f64::from(area.width) * float.width_pct / 100.0) as u16),
            4,
            area.width,
        );
        let height = clamp_span(
            float
                .rows
                .unwrap_or_else(|| (f64::from(area.height) * float.height_pct / 100.0) as u16),
            3,
            area.height,
        );
        Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (area.height - height) / 2,
            width,
            height,
        }
    }

    fn draw_plugin(&mut self, frame: &mut Frame, index: usize, rect: Rect, focused: bool) {
        let ctx = RenderContext {
            width: rect.width,
            height: rect.height,
            focused,
            elapsed: self.started.elapsed().as_secs_f64(),
            frame: self.frames,
        };
        Counters::bump(&self.perf.renders);
        match self.host.render(index, ctx) {
            Ok(rendered) => {
                let node = rendered.node;
                // A plugin that floats is drawn above the arrangement instead,
                // so it takes no room in its slot.
                if rendered.float.is_some() {
                    return;
                }
                if focused {
                    // The session, for everything that is about sessions — the
                    // focus label, the info the bands show, `Ctrl+O`.
                    self.focused_session = node.first_session_surface().map(str::to_string);
                    // And whatever this pane is actually showing, which is where
                    // raw input goes. `input = "session"` never meant "the
                    // selected session"; it meant "what this pane is showing", and
                    // a plugin's own program is now one of the things that can be.
                    self.focused_surface = node.first_live_surface().map(str::to_string);
                }

                // Decoration: another plugin may restyle this tree. A decorator
                // that fails costs its decoration, not the pane — so the
                // original is what gets drawn.
                let slot = self.host.plugins[index].slot.clone();
                let mut node = node;
                for decorator in self.host.decorators_of(&slot) {
                    match self.host.decorate(decorator, &node, ctx) {
                        Ok(decorated) => node = decorated,
                        Err(e) => self.errors.push(e),
                    }
                }
                // A surface's cells live OUTSIDE the tree, so tree equality
                // cannot tell whether it changed. Its own output stamp can: a
                // pane whose agent has said nothing since the last paint is as
                // settled as a pane of text, which is what lets a screen with a
                // live terminal on it idle at the redraw floor instead of
                // repainting at the frame cap forever. v1 gates the same way,
                // off the same atomic (`detect_output_redraw`).
                if self.last_trees.len() <= index {
                    self.last_trees.resize(index + 1, None);
                }
                let surface_moved = match node.first_session_surface() {
                    Some(surface) => {
                        let stamp = self.terminals.output_stamp(surface);
                        let previous = self.last_output_painted.get(surface).copied();
                        if let Some(stamp) = stamp {
                            self.last_output_painted.insert(surface.to_string(), stamp);
                        }
                        // An unattached surface has no stamp and nothing to
                        // show, so it is settled rather than perpetually new.
                        stamp.is_some() && previous != stamp
                    }
                    None => false,
                };
                let unchanged = self.last_trees[index].as_ref() == Some(&node) && !surface_moved;
                if !unchanged {
                    self.changed_this_frame = true;
                }
                self.last_trees[index] = Some(node.clone());
                let mut hits = Vec::new();
                paint::render_recording(frame, rect, &node, &self.terminals, &mut hits);
                // The pane's own rect is only a target when focus can rest on
                // it. A footer click must reach the pill it landed on and
                // nothing else — v1 likewise records no `FocusPane` for panes
                // that cannot hold focus.
                let fallback = self.host.plugins[index].focusable.then_some(rect);
                self.push_targets(index, fallback, hits);
            }
            Err(e) => {
                paint::render_error(frame, rect, &e.plugin, &e.message);
                Counters::bump(&self.perf.failures);
                self.errors.push(e);
            }
        }
    }

    fn on_key(&mut self, key: &KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // A system modal takes input before anything else — that is what makes
        // it modal rather than a pane drawn on top. Two things still get
        // through: the escape route (quit, reload, the perf HUD), and another
        // modal's own opening chord, since opening one closes another. While
        // help is capturing, neither does — binding `ctrl+q` has to be
        // possible, and the kernel can allow it because it knows the capture
        // lasts exactly one keystroke.
        if self.modals.is_open() {
            if self.modals.captures_everything() {
                self.dispatch_modal_key(key);
                return;
            }
            if let Some(kind) = self.modal_chord(key) {
                self.toggle_modal(kind);
                return;
            }
            if !thurbox::kernel::modals::escapes(key) {
                self.dispatch_modal_key(key);
                return;
            }
        }

        // The reserved minimum: focus, reload and quit always work, even if a
        // plugin consumes every key it is offered.
        //
        // Quit is Ctrl+Q, not Ctrl+C: with a live terminal attached, Ctrl+C has
        // to reach the agent so a turn can be interrupted. v1 reserves the same
        // chord for the same reason.
        match key.code {
            KeyCode::Char('q') if ctrl => {
                self.quit = true;
                return;
            }
            // F10, not F5: v1 spends F1-F9 and F12 on real UI (F5 is the tasks
            // panel), so the dev reload takes one of the two keys v1 leaves
            // free rather than shadowing a pane the user expects.
            KeyCode::F(10) => {
                self.reload_interface();
                self.collect_declarations();
                self.clamp_focus();
                return;
            }
            // Tab is NOT a focus key: it belongs to the agent. See RESERVED.
            // v1 binds focus movement to Ctrl+H/Ctrl+L as well, and refuses to
            // let a focused terminal keep either: they are how you get *out* of
            // one, so they cannot be among the chords handed to the agent.
            KeyCode::Char('h') if ctrl => return self.cycle_focus(-1),
            KeyCode::Char('l') if ctrl => return self.cycle_focus(1),
            // The HUD reports on this loop, so it is the kernel's own key
            // rather than a plugin's — nothing a plugin does can hide it. v1
            // spends F12 on the same thing for the same reason.
            // The one reserved key that is a *feature*: v1 gates the HUD behind
            // `[features] perf_hud` because opening it also turns on wall-clock
            // timing collection, which is not free.
            KeyCode::F(12) if self.config.features().perf_hud => {
                self.hud = !self.hud;
                self.dirty = true;
                return;
            }

            // Copy and paste are reserved, like v1's: they must work from any
            // pane, and `Ctrl+C` must not reach the agent when there is a
            // selection to copy — that is the one case where thurbox wins the
            // chord back from the terminal.
            KeyCode::Char('c') if ctrl && self.selection.is_some() => {
                // No focused session required: a selection over the session
                // list, a modal or the footer is still a selection, and v1
                // copies it. Only the fall-back-to-whole-screen path needs a
                // terminal, because only a terminal HAS a screen.
                let session = self.focused_session.clone();
                self.copy_selection_or_screen(session.as_deref());
                return;
            }
            KeyCode::Char('v') if ctrl => {
                self.paste_into_focused();
                return;
            }
            _ => {}
        }

        let press = to_press(key);

        // A float takes every key while it is up — that is what makes it a
        // modal rather than merely a pane drawn on top. The reserved chords
        // above still work, so a modal can never trap you.
        if let Some(plugin) = self
            .grabbed
            .and_then(|index| self.host.plugins.get(index))
            .map(|plugin| plugin.name.clone())
        {
            let index = self.grabbed.expect("just resolved through it");
            if let Some(action) = self
                .registry
                .resolve(&press, Some(&plugin))
                .map(|b| b.action.clone())
            {
                match self.host.on_action(index, &action) {
                    Ok(true) => return,
                    Ok(false) => {}
                    Err(e) => self.errors.push(e),
                }
            }
            match self.host.on_key(index, &press) {
                Ok(_) => {}
                Err(e) => self.errors.push(e),
            }
            return;
        }

        // A declared key first: the registry resolves the chord to an action
        // and the plugin that owns it. This is the path that can be rebound,
        // conflict-checked and listed in help.
        let focused_name = self
            .host
            .focusable()
            .get(self.focus)
            .and_then(|index| self.host.plugins.get(*index))
            .map(|plugin| plugin.name.clone());
        if let Some((plugin, action, passthrough)) = self
            .registry
            .resolve(&press, focused_name.as_deref())
            .map(|binding| {
                (
                    binding.plugin.clone(),
                    binding.action.clone(),
                    binding.passthrough,
                )
            })
        {
            // v1's terminal passthrough: a chord the agent's own line editing
            // needs is left to the pty while a terminal has focus, and the
            // command stays reachable from every other pane (and its F-key
            // alternate). Gated on the bound chord, so rebinding a passthrough
            // action onto a free key makes it work in the terminal again.
            let defer_to_agent = passthrough
                && self.focused_wants_session_input()
                && is_ctrl_letter_chord(&canonical_chord(&press));
            // A chord the kernel declared for itself opens a system modal;
            // there is no plugin to hand it to.
            if !defer_to_agent && plugin == thurbox::kernel::modals::OWNER {
                if let Some(kind) = ModalKind::from_action(&action) {
                    self.toggle_modal(kind);
                    return;
                }
            }
            if !defer_to_agent {
                if let Some(index) = self.host.index_of(&plugin) {
                    match self.host.on_action(index, &action) {
                        Ok(true) => return,
                        Ok(false) => {}
                        Err(e) => self.errors.push(e),
                    }
                }
            }
        }

        // Then raw keys: the focused plugin, then the non-focusable listeners.
        let focusable = self.host.focusable();
        let mut order: Vec<usize> = focusable.get(self.focus).copied().into_iter().collect();
        order.extend(
            (0..self.host.plugins.len()).filter(|index| !self.host.plugins[*index].focusable),
        );

        for index in order {
            match self.host.on_key(index, &press) {
                Ok(true) => return,
                Ok(false) => {}
                Err(e) => {
                    // A throwing handler must not swallow the key or the app.
                    self.errors.push(e);
                }
            }
        }

        // Nothing claimed it. If the focused plugin asked for raw session input
        // and its surface names a live session, the key belongs to the agent.
        //
        // The kernel does not know which plugin is "the terminal": it knows one
        // declared `input = "session"` and which session the tree it returned
        // pointed at. Replace that plugin and this still works.
        if self.focused_wants_session_input() {
            if let Some(surface) = self.focused_surface.clone() {
                if let Some(bytes) = key_to_bytes(key.code, key.modifiers) {
                    // Whether it lands is the terminal's business; either way
                    // the key belongs to the pane that asked for raw input and
                    // is not offered to anything else.
                    //
                    // Routed by what the pane is SHOWING. A program pane's keys go
                    // to that program and to nothing else — and a pane with nothing
                    // behind it swallows nothing, since neither send finds a
                    // target.
                    let delivered = match self.terminals.program_key(&surface).cloned() {
                        Some(program) => self.terminals.send_to_program(&program, bytes),
                        None => self.terminals.send(&surface, bytes),
                    };
                    // Delivered means consumed, which is what the rule above says
                    // and what this now enforces. Falling through sent `Esc` to a
                    // program AND dismissed the pane under it in one keypress — a
                    // game opening its menu on a pane nobody is looking at.
                    //
                    // Gated on delivery rather than on having tried, because both
                    // sends already report it: a surface naming a session that is
                    // no longer live must not swallow the key, or `Esc` traps the
                    // user in a pane showing a dead terminal.
                    if delivered {
                        return;
                    }
                }
            }
        }

        // An `Esc` no pane claimed means "leave this one" — the v2 spelling of
        // v1 closing a modal, since a centre-slot pane is dismissed by focusing
        // whatever you came from.
        if key.code == KeyCode::Esc && self.focus_return != self.focus {
            let back = self.focus_return;
            self.focus_return = self.focus;
            if self.host.focusable().get(back).is_some() {
                self.focus = back;
            }
        }

        // Anything else is DROPPED. An unclaimed key does nothing.
        //
        // This used to quit on a bare `q` or `Esc`, which meant Escaping out of
        // the theme picker -- or any pane that does not claim Esc -- killed the
        // application. Quit is Ctrl+Q, reserved at the top of this function;
        // v1 has no bare-key quit either.
    }

    /// The modal this keystroke opens, if it is one of the kernel's own chords.
    ///
    /// Resolved through the registry rather than matched literally, so a
    /// rebound chord keeps opening its modal — including from inside another
    /// one.
    fn modal_chord(&self, key: &KeyEvent) -> Option<ModalKind> {
        let binding = self.registry.resolve(&to_press(key), None)?;
        (binding.plugin == thurbox::kernel::modals::OWNER)
            .then(|| ModalKind::from_action(&binding.action))
            .flatten()
    }

    /// Hand a keystroke to the open modal, and report whatever it says.
    fn dispatch_modal_key(&mut self, key: &KeyEvent) {
        // The registry's spelling of this keystroke, so a captured chord is
        // stored in the vocabulary the registry will later match — the three
        // encodings of `ctrl+/` folded into one, a capital folded to `shift+`.
        let chord = canonical_chord(&to_press(key));
        let message = self.with_modal_world(|modals, world| modals.on_key(key, &chord, world));
        if let Some(message) = message {
            self.toast(message);
        }
        self.dirty = true;
    }

    /// Run `act` against the modal layer with everything a modal may write to.
    ///
    /// The database is opened only for the theme picker, which is the one modal
    /// that persists outside the registry — a connection per keystroke would
    /// otherwise be paid by every keypress in help.
    /// Open or close a system modal.
    ///
    /// Goes through `abandon` first because a modal may have applied something
    /// while it was open — the theme picker previews on every cursor move — and
    /// closing it by its own chord is no more a choice than closing it with
    /// `Esc`.
    fn toggle_modal(&mut self, kind: ModalKind) {
        self.with_modal_world(|modals, world| {
            if modals.kind() == Some(kind) {
                modals.abandon(world);
            }
        });
        self.modals.toggle(kind);
        self.dirty = true;
    }

    /// Restore or remove one interface file, and ask for the reload.
    ///
    /// One implementation for both callers — a plugin's `command("plugin", …)`
    /// and the settings modal's Interface tab — so the two cannot come to mean
    /// different things about what restoring is.
    fn apply_plugin_edit(&mut self, file: &str, edit: thurbox::kernel::command::PluginEdit) {
        let outcome = match edit {
            thurbox::kernel::command::PluginEdit::Restore => {
                thurbox::kernel::bundled::restore(&self.ui_dir, file)
                    .map(|()| format!("restored {file}"))
            }
            thurbox::kernel::command::PluginEdit::Remove => {
                thurbox::kernel::bundled::remove(&self.ui_dir, file)
                    .map(|()| format!("removed {file}"))
            }
        };
        match outcome {
            Ok(message) => {
                // Removal changes no file the watcher is waiting on, so the
                // reload is asked for here.
                self.refresh_sources();
                self.reload_at = Some(Instant::now() + DEBOUNCE);
                self.toast(message);
            }
            Err(e) => self.report(e, Level::Error),
        }
    }

    /// Take what plugins asked for, start what may start, and publish answers.
    ///
    /// The runner closure is what makes a run land on the right machine: it
    /// resolves the session to its directory and its host **before** the worker
    /// starts, so an unreachable host is a reported failure rather than a
    /// program run against a path that only exists somewhere else.
    fn serve_runs(&mut self) {
        let asked = self.host.drain_runs();
        // Nothing asked now, nothing ever asked: every interface with no
        // capability-using plugin, i.e. almost all of them. Checked *before*
        // `self.runner()`, which clones every session's id, cwd and backend into a
        // fresh map behind a new `Arc` — the `||` below evaluates it whenever
        // nothing was asked, so that was happening on every 10 ms iteration.
        if asked.is_empty() && self.runs.is_empty() {
            return;
        }
        if !asked.is_empty() || self.runs.poll(&self.runner()) {
            self.dirty = true;
        }
        if !asked.is_empty() {
            let runner = self.runner();
            for (plugin, ask) in asked {
                self.runs.request(&plugin, ask, &runner);
            }
        }
        // Answers, per plugin, for the next render to read.
        let mut answers = thurbox::kernel::host::RunAnswers::new();
        for plugin in &self.host.plugins {
            if plugin.capabilities.is_empty() {
                continue;
            }
            let mine = self.runs.for_plugin(&plugin.path);
            if !mine.is_empty() {
                answers.insert(
                    plugin.path.clone(),
                    mine.into_iter()
                        .map(|(key, run)| (key.to_string(), run.clone()))
                        .collect(),
                );
            }
        }
        self.host.set_runs(answers);
    }

    /// How one ask becomes a program on the right machine.
    fn runner(&self) -> std::sync::Arc<thurbox::kernel::runs::Runner> {
        let sessions: std::collections::HashMap<String, (std::path::PathBuf, String)> = self
            .snapshots
            .current()
            .sessions
            .iter()
            .filter_map(|row| {
                let cwd = row.cwd.clone()?;
                Some((row.id.clone(), (cwd, row.backend.clone())))
            })
            .collect();
        std::sync::Arc::new(move |ask: &thurbox::kernel::runs::Ask| {
            let Some((cwd, backend)) = sessions.get(&ask.session) else {
                return thurbox::kernel::runs::Run::Failed(format!(
                    "no session {} to run it in",
                    ask.session
                ));
            };
            let Some(host) = thurbox::session_ops::resolve_host(backend) else {
                return thurbox::kernel::runs::Run::Failed(format!(
                    "'{backend}' is not in hosts.toml — cannot reach the machine it runs on"
                ));
            };
            let command = thurbox::kernel::runs::command_for(&ask.program, cwd, host.as_ref());
            thurbox::kernel::runs::capture(command, ask.timeout)
        })
    }

    /// Dispatch a command, remembering what it was so its outcome can be
    /// reported and its session let go of when it finishes.
    ///
    /// Tracked **here** rather than by sampling the in-flight list, because a
    /// command that succeeds is removed from that list inside `CommandBus::poll`
    /// — before anything gets to look at it. Sampling only caught commands that
    /// happened to be running when some *other* command finished, which for the
    /// common case (one restart, nothing else in flight) is never: the restart
    /// was never tracked, so it was never seen to finish, so its pane was never
    /// let go of and the session stayed frozen.
    ///
    /// It is also the only moment the subject can be resolved: a delete's row is
    /// gone by the time it reports.
    fn dispatch_tracked(&mut self, command: thurbox::kernel::command::Command) {
        let kind = command.kind();
        let session = command.session().to_string();
        let label = self
            .snapshots
            .current()
            .session(&session)
            .map(|row| row.name.clone())
            .filter(|label| !label.is_empty());
        let id = self.commands.dispatch(command);
        self.tracked_commands.insert(
            id,
            TrackedCommand {
                kind,
                session,
                label,
                failed: false,
            },
        );
    }

    /// Persist the pane id of a session's companion shell.
    fn remember_shell(&mut self, session: &str) {
        let Some(pane) = self.terminals.shell_pane_id(session) else {
            return;
        };
        if let Err(e) = self.snapshots.remember_shell(session, &pane) {
            tracing::warn!("could not record the shell pane for {session}: {e}");
        }
    }

    /// Re-attach any session to the shell window it already had.
    ///
    /// v1 does this at restore (`readopt_shell_pane`); here attaching is a
    /// worker, so the shell is picked up on the frame after the agent's own pane
    /// lands. Cheap to ask every iteration: it is a map lookup for a session that
    /// has no shell recorded, which is nearly all of them.
    fn readopt_shells(&mut self) {
        let wanted: Vec<(String, String)> = self
            .snapshots
            .current()
            .sessions
            .iter()
            .filter(|row| row.shell_backend_id.is_some())
            .filter(|row| self.terminals.is_attached(&row.id))
            .filter(|row| !self.terminals.has_shell(&row.id))
            .filter_map(|row| {
                row.shell_backend_id
                    .clone()
                    .map(|pane| (row.id.clone(), pane))
            })
            .collect();
        // Asked before the size is: `terminal::size()` is a syscall, and this
        // runs on every iteration of a loop that polls every 10ms. Almost always
        // there is nothing to adopt.
        if wanted.is_empty() {
            return;
        }
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        for (session, pane) in wanted {
            if self.terminals.readopt_shell(&session, &pane, rows, cols) {
                self.dirty = true;
            } else {
                // The window is gone — closed outside thurbox, or its server
                // restarted. Forget it, or this is retried on every iteration
                // forever against a pane that will never answer.
                let _ = self.snapshots.forget_shell(&session);
            }
        }
    }

    /// Relaunch the agent of any session whose window is gone.
    ///
    /// v1's `respawn_stale_session`, reached differently. v1 asks the question
    /// once, synchronously, during restore; here discovery is a worker, so the
    /// answer arrives a moment later and the question is asked each frame until
    /// it can be answered. Once per session per run either way — a session that
    /// cannot be relaunched must not be relaunched forever.
    ///
    /// It goes through `restart`, which is exactly a respawn once killing a
    /// window that is not there stopped being an error: same session id, same
    /// worktrees, same host, and `resume_trigger_for` deciding whether the agent
    /// resumes its conversation or starts fresh.
    fn respawn_missing_agents(&mut self) {
        let missing = self.terminals.missing_agents(self.snapshots.current());
        for id in missing {
            if !self.respawned.insert(id.clone()) {
                continue;
            }
            let name = self
                .snapshots
                .current()
                .session(&id)
                .map(|row| row.name.clone())
                .unwrap_or_else(|| id.clone());
            self.dispatch_tracked(thurbox::kernel::command::Command::Restart {
                session: id.clone(),
            });
            self.toast(format!("{name}: agent was gone, relaunching"));
        }
    }

    /// Rebuild the interface from the directory the user actually edits, dropping
    /// to the bundled copy only for as long as that one will not load.
    ///
    /// Every reload goes through here rather than `host.reload()` because the host
    /// rebuilds from whichever directory it was last built from. Once the floor had
    /// been installed, that was `/tmp`: the watcher fired on the user's fix, the
    /// fallback was rebuilt from itself, nothing changed, and no error appeared —
    /// while `ui_dir`, the watcher, the inventory and every Interface-tab action
    /// still pointed at the user's copy, so `r`/`d`/`space`/`t` wrote the right
    /// file to no visible effect. Only a restart recovered. It also cost a plugin
    /// author their edit-reload loop, since a `./ui` checkout takes the same path.
    fn reload_interface(&mut self) {
        self.host.reload_from(&self.ui_dir);
        Counters::bump(&self.perf.reloads);
        // Plugin indices are positions in a vector the rebuild just replaced, and
        // `grabbed` is one recorded during the previous paint. A reload between a
        // paint and a keystroke — a watcher firing on a deleted file — would leave
        // it pointing past the end. The next paint sets it again if a float is
        // still up.
        self.grabbed = None;
        self.last_floats.clear();
        self.drawn_floats.clear();
        // A plugin that was edited away, renamed, removed or turned off must not
        // leave its answers behind to accumulate across reloads — and must not be
        // able to read them again if a file of that name comes back.
        let live: Vec<String> = self
            .host
            .plugins
            .iter()
            .map(|plugin| plugin.path.clone())
            .collect();
        self.runs.retain_plugins(&live);
        // The same rule for a program it was holding, and here it matters more: an
        // answer left behind is a few bytes, a program left behind is a process
        // nothing on screen can ever reach again. A plugin that is still loaded
        // keeps its pane — a reload is an edit to a file, and losing your game to
        // one would make reloading unusable.
        self.terminals.retain_program_plugins(&live);
        if self.host.error.is_none() {
            if self.floor.take().is_some() {
                self.toast(format!("{} loaded again", self.ui_dir.display()));
            }
            return;
        }

        // An explicit THURBOX_UI_DIR is the user saying which interface to run, so
        // it is never silently swapped — the error stands on its own.
        if std::env::var_os("THURBOX_UI_DIR").is_some() {
            return;
        }
        let Ok(dir) = thurbox::kernel::bundled::fallback_dir() else {
            return;
        };
        let embedded = LuaHost::new(&dir);
        if embedded.error.is_some() {
            return;
        }
        // Recorded before the swap: the reason is the *user's* error, and the
        // host that carries it is about to be replaced by one with none.
        self.floor = Some(format!(
            "using the bundled interface — {} failed to load: {}",
            self.ui_dir.display(),
            self.host.error.clone().unwrap_or_default()
        ));
        self.host = embedded;
    }

    /// Turn one interface file off, or back on, and reload.
    ///
    /// The reload is the mechanism, not a side effect: a disabled plugin is one
    /// `build` did not read, so the decision only becomes true when the VM is
    /// built again (design D2/D3). No confirmation — the action is reversible by
    /// the same key, and confirming a reversible act is what teaches people to
    /// confirm without reading.
    fn apply_switch(&mut self, file: &str, off: bool) {
        let absolute = self.ui_dir.join(file);
        let key = absolute.to_string_lossy().into_owned();
        match self.registry.set_disabled(&key, off) {
            Ok(now_off) => {
                self.publish_disabled();
                self.reload_interface();
                self.collect_declarations();
                self.clamp_focus();
                self.refresh_sources();
                self.toast(if now_off {
                    format!("{file} turned off")
                } else {
                    format!("{file} turned on")
                });
            }
            Err(e) => self.report(e, Level::Error),
        }
    }

    /// Record the user's decision about one interface file, and reload.
    ///
    /// The reload is not incidental: a plugin's capabilities are installed when
    /// its environment is built, so revoking has to rebuild it — that is what
    /// makes the capability *absent* on the next frame rather than present and
    /// refusing (design D7).
    fn apply_trust(&mut self, file: &str, trusted: bool) {
        let absolute = self.ui_dir.join(file);
        let key = absolute.to_string_lossy().into_owned();
        let outcome = if trusted {
            match std::fs::read_to_string(&absolute) {
                // Trusted as it is *now*: the digest recorded here is what makes
                // "trusted, and since modified" a state the list can report.
                //
                // For an INSTALLED file the `src@version` is recorded beside it, so
                // the grant is a statement the user could actually have meant — "I
                // trust atlas v0.3.1" — and lapses when the pin moves instead of
                // reading as tampering after every ordinary release.
                Ok(contents) => {
                    let lock =
                        thurbox::kernel::packages::read_lock(&self.ui_dir).unwrap_or_default();
                    match lock.covering(file) {
                        Some(entry) => self
                            .registry
                            .trust_installed(&key, &entry.pin_key(), &contents)
                            .map(|()| format!("trusting {file} at {}", entry.pin_key())),
                        None => self
                            .registry
                            .trust(&key, &contents)
                            .map(|()| format!("trusting {file}")),
                    }
                }
                Err(e) => Err(format!("{}: {e}", absolute.display())),
            }
        } else {
            self.registry
                .revoke(&key)
                .map(|()| format!("no longer trusting {file}"))
        };
        match outcome {
            Ok(message) => {
                self.reload_interface();
                self.collect_declarations();
                self.clamp_focus();
                self.refresh_sources();
                self.toast(message);
            }
            Err(e) => self.report(e, Level::Error),
        }
    }

    fn with_modal_world<T>(
        &mut self,
        act: impl FnOnce(&mut Modals, &mut thurbox::kernel::modals::World<'_>) -> T,
    ) -> T {
        let db = matches!(self.modals.kind(), Some(ModalKind::Theme))
            .then(snapshots_db)
            .flatten();
        // The settings the modal shows, and the slot a save comes back through.
        // Cloned because the modal needs the settings while `self.config` is
        // borrowed mutably to apply whatever comes back.
        let in_force = self.config.in_force().clone();
        let mut saved = None;
        let mut edit = None;
        // Same reason as the settings clone: the modal reads it while `self` is
        // borrowed mutably to apply whatever comes back.
        let inventory = std::mem::take(&mut self.inventory);
        let outcome = {
            let mut world = thurbox::kernel::modals::World {
                registry: &mut self.registry,
                themes: &mut self.themes,
                settings: &in_force,
                save_settings: &mut saved,
                inventory: &inventory,
                interface_edit: &mut edit,
                db: db.as_ref(),
            };
            act(&mut self.modals, &mut world)
        };
        self.inventory = inventory;
        if let Some(draft) = saved {
            self.apply_settings(draft);
        }
        match edit {
            Some(thurbox::kernel::modals::interface::Edit::File { file, kind }) => {
                self.apply_plugin_edit(&file, kind);
            }
            Some(thurbox::kernel::modals::interface::Edit::Trust { file, trusted }) => {
                self.apply_trust(&file, trusted);
            }
            Some(thurbox::kernel::modals::interface::Edit::Switch { file, off }) => {
                self.apply_switch(&file, off);
            }
            None => {}
        }
        outcome
    }

    /// Take a saved draft into force and write it to the file.
    ///
    /// Two halves, in this order: the live flags apply now (that is what makes
    /// them live), and the file is written on a worker like everything else that
    /// touches the world — so a read-only filesystem surfaces as a reported
    /// failure rather than a silent one.
    fn apply_settings(&mut self, draft: thurbox::session::settings::Settings) {
        let outcome = self.config.adopt(draft.clone());
        self.config.mark_saved();
        self.commands
            .dispatch(thurbox::kernel::command::Command::Configure {
                settings: Box::new(draft),
            });
        if outcome == thurbox::kernel::config::Reloaded::NeedsRestart {
            self.toast("saved — some changes apply on restart".to_string());
        }
        self.dirty = true;
    }

    /// Whether the focused plugin declared `input = "session"`.
    fn focused_wants_session_input(&self) -> bool {
        self.host
            .focusable()
            .get(self.focus)
            .and_then(|index| self.host.plugins.get(*index))
            .is_some_and(|plugin| plugin.session_input)
    }

    /// Hand the registry what every loaded plugin declared, plus the kernel's
    /// own chords.
    ///
    /// The system modals go through the same registry as everything else, which
    /// is what makes them listable in help, conflict-checked against a plugin's
    /// keys, and rebindable — they simply have no Lua plugin behind them.
    /// Tell the host which plugins the user trusted, by the path it knows them by.
    ///
    /// Relative, because that is `Plugin::path`; trust is stored absolute so two
    /// interface directories cannot share it, so this is where the two meet.
    fn publish_trust(&self) {
        let trusted: Vec<String> = self
            .host
            .plugins
            .iter()
            .map(|plugin| plugin.path.clone())
            .filter(|path| {
                let absolute = self.ui_dir.join(path);
                self.registry.is_trusted(&absolute.to_string_lossy())
            })
            .collect();
        self.host.set_trusted(trusted);
    }

    /// Tell the host which plugins the user turned off.
    ///
    /// Derived from the *stored* absolute paths rather than from the loaded
    /// plugins, because a disabled one is not loaded — it would not be in the
    /// list to filter. Relative, because that is what `build` compares against.
    /// Start or close a plugin's program pane.
    ///
    /// The gate is here rather than in the queue: a command is honoured after the
    /// call that made it, so the check is "may the plugin that asked, right now" —
    /// which is also what makes revoking trust take effect on the next frame
    /// rather than at the next reload.
    ///
    /// A refusal is **reported to the plugin's own error channel** rather than
    /// silently dropped, because a pane that cannot start needs to be able to say
    /// why: an empty box that never explains itself is the failure mode the
    /// capability model is otherwise prone to.
    fn apply_program(
        &mut self,
        owner: &str,
        name: &str,
        program: &str,
        argv: &[String],
        close: bool,
    ) {
        let key = thurbox::kernel::terminal::ProgramKey::new(owner, name);
        if close {
            if self.terminals.release_program(&key) {
                self.changed_this_frame = true;
            }
            return;
        }
        // Absent rather than refusing, as `run` is: a plugin that did not declare
        // the capability, or that the user has not trusted, simply cannot start
        // anything. Reported so an untrusted pane can say so instead of looking
        // broken.
        if !self
            .host
            .may_path(owner, thurbox::kernel::host::Capability::Program)
        {
            self.report(
                format!("{owner} may not run a program — trust it in settings → Interface"),
                Level::Error,
            );
            return;
        }

        // Born at the rect it will be painted into where the last frame recorded
        // one; `open_shell` documents why the render-time resize cannot correct a
        // bad birth size once the size memo looks settled.
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let rect = self.terminals.last_rect(&key.surface_id());
        let (rows, cols) = match rect {
            Some(rect) if rect.width > 0 && rect.height > 0 => (rect.height, rect.width),
            _ => (rows, cols),
        };
        // The interface directory: the one directory a plugin-owned pane can be
        // said to belong to, since it has no session and therefore no worktree.
        let cwd = self.ui_dir.clone();
        if let Err(e) = self
            .terminals
            .start_program(&key, program, argv, Some(&cwd), rows, cols)
        {
            self.report(e, Level::Error);
            return;
        }
        self.changed_this_frame = true;
    }

    fn publish_disabled(&self) {
        let disabled: Vec<String> = self
            .registry
            .disabled()
            .filter_map(|absolute| thurbox::kernel::bundled::relative_to(&self.ui_dir, absolute))
            .collect();
        self.host.set_disabled(disabled);
    }

    fn collect_declarations(&mut self) {
        let (mut bindings, settings, mut pills) = self.host.all_declarations();
        bindings.extend(thurbox::kernel::modals::bindings());
        pills.extend(thurbox::kernel::modals::pills());
        self.registry.declare_all(bindings, settings, pills);
        // Trust is read against the plugin set that just loaded, so a reload —
        // including the one a trust change triggers — lands both together.
        self.publish_trust();
    }

    /// Report what just happened, for [`STATUS_TTL`], at informational level.
    fn toast(&mut self, message: impl Into<String>) {
        self.report(message, Level::Info);
    }

    /// Report at a chosen severity. The message band badges the three
    /// differently, as v1 does — an error that reads like a confirmation is an
    /// error nobody notices.
    fn report(&mut self, message: impl Into<String>, level: Level) {
        self.status = Some((message.into(), level, Instant::now()));
        self.dirty = true;
    }

    /// Draw one chrome band into the rect the arrangement gave it.
    ///
    /// Every value is read from state the kernel already holds — the version, the
    /// theme, the counts, the focused surface, the message — which is what makes
    /// "a band cannot be broken by a plugin" true rather than aspirational.
    fn render_band(&mut self, frame: &mut Frame, rect: Rect, band: Band) {
        let snapshot = self.snapshots.current();
        let selected = self.host.shared_string("selected");
        let session = selected
            .as_deref()
            .and_then(|id| snapshot.session(id))
            .map(|row| row.name.clone());
        // The VIEW if the focused pane is showing one, else the pane itself —
        // `focused_session` carries `<id>#shell` while the shell tab is up, and
        // the centre pane paints before the footer band, so this is already
        // resolved by the time the band draws.
        let focused_plugin = self
            .host
            .focusable()
            .get(self.focus)
            .and_then(|index| self.host.plugins.get(*index))
            .map(|plugin| plugin.name.clone())
            .unwrap_or_default();
        let focus_label = bands::focus_label(self.focused_session.as_deref(), &focused_plugin);
        // Work in flight outranks a toast while it runs: creation phases
        // routinely outlast the retention window, which is why they are progress
        // rather than a message. `first_running` rather than the whole list,
        // because progress outranking a message makes a *failed* entry hide the
        // error explaining it — see [`CommandBus::first_running`].
        let progress = self
            .commands
            .first_running()
            .map(|item| match &item.subject {
                Some(subject) => format!("{} {subject}…", item.kind),
                None => format!("{}…", item.kind),
            });
        let message = self
            .status
            .as_ref()
            .filter(|(_, _, at)| at.elapsed() < STATUS_TTL)
            .map(|(text, level, _)| (text.as_str(), *level));

        let state = BandState {
            version: env!("THURBOX_VERSION"),
            theme_label: &self.themes.active().display_name,
            // Nothing detects a newer release yet — v2 has no startup update
            // check (`v2-parity-gaps`, "no startup update check and no silent
            // auto-update"). The band renders the notice when something does;
            // until then there is honestly nothing to announce.
            update_available: self.updates.available(),
            session: session.as_deref(),
            session_count: snapshot.sessions.len(),
            automation_count: snapshot.automations.len(),
            focus_label: &focus_label,
            message,
            progress: progress.as_deref(),
            hovered: self.hovered.as_ref(),
            registry: &self.registry,
            themes: &self.themes,
        };
        if !bands::occupies(band, &state) {
            return;
        }
        let hits = bands::render(frame, rect, band, &state);
        self.band_targets.extend(hits);
        // A band drawn is a band that changed the frame; the message band in
        // particular appears and disappears without any tree moving.
        self.changed_this_frame = true;
    }

    /// Record one plugin's hitboxes, its own rect first.
    ///
    /// Order is the whole contract: the pane fallback goes in before the tree
    /// so that scanning in reverse finds an identified node first and the
    /// fallback only when nothing inside matched. v1 spells the same rule the
    /// other way round — specific targets recorded first, first match wins.
    fn push_targets(&mut self, plugin: usize, pane: Option<Rect>, hits: Vec<paint::Hit>) {
        if let Some(rect) = pane {
            self.click_targets.push(ClickTarget {
                plugin,
                rect,
                identity: Identity::default(),
            });
        }
        self.click_targets
            .extend(hits.into_iter().map(|hit| ClickTarget {
                plugin,
                rect: hit.rect,
                identity: hit.identity,
            }));
    }

    /// The band button under a point, if any.
    ///
    /// Scanned in reverse for the same reason the pane targets are: the last
    /// recorded hit is the topmost, so overlapping entries resolve to the one
    /// actually visible.
    fn band_target_at(&self, x: u16, y: u16) -> Option<thurbox::kernel::bands::Hit> {
        let position = ratatui::layout::Position::new(x, y);
        self.band_targets
            .iter()
            .rev()
            .find(|hit| hit.rect.contains(position))
            .cloned()
    }

    /// The target under a point, innermost and topmost first.
    fn target_at(&self, x: u16, y: u16) -> Option<ClickTarget> {
        let position = ratatui::layout::Position::new(x, y);
        self.click_targets
            .iter()
            .rev()
            .find(|target| target.rect.contains(position))
            .cloned()
    }

    fn on_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.on_click(mouse.column, mouse.row, mouse.modifiers)
            }
            MouseEventKind::Drag(MouseButton::Left) => self.drag_selection(mouse.column, mouse.row),
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(selection) = &mut self.selection {
                    selection.dragging = false;
                }
            }
            MouseEventKind::ScrollUp => self.on_scroll(mouse.column, mouse.row, true),
            MouseEventKind::ScrollDown => self.on_scroll(mouse.column, mouse.row, false),
            // A bare move only matters when it changes what is under the
            // pointer. Anything else — and there is a LOT of it, one report per
            // cell crossed — is dropped without touching `dirty`.
            MouseEventKind::Moved => self.hover(mouse.column, mouse.row),
            _ => {}
        }
    }

    /// A wheel tick, routed the way v1's `handle_mouse_scroll` routes one.
    ///
    /// Three legs, in order: an open modal owns the wheel outright; a live
    /// terminal that asked for mouse tracking gets the tick forwarded to its
    /// pty; otherwise the pane **under the pointer** scrolls — not the focused
    /// one, so you can spin the wheel over a list without leaving the pane you
    /// are working in.
    ///
    /// A tick becomes an `up`/`down` keystroke rather than a scroll command of
    /// its own: every scrollable pane already declares those, so the wheel and
    /// the arrow keys cannot come to mean different things — the same reasoning
    /// behind the `key:<chord>` click role.
    fn on_scroll(&mut self, x: u16, y: u16, up: bool) {
        let code = if up { KeyCode::Up } else { KeyCode::Down };
        let key = KeyEvent::new(code, KeyModifiers::NONE);

        // A modal takes the wheel as one selection step, never the panes it
        // covers. While help is capturing a chord it is left alone: the capture
        // would record the synthesized keystroke as the new binding.
        if self.modals.is_open() {
            if !self.modals.captures_everything() {
                self.dispatch_modal_key(&key);
            }
            return;
        }

        // A float owns the wheel for the same reason it owns clicks.
        if let Some(index) = self.grabbed {
            self.dispatch_key_to(index, &key);
            self.dirty = true;
            return;
        }

        if self.terminals.forward_wheel(x, y, up) {
            self.dirty = true;
            return;
        }

        if let Some(target) = self.target_at(x, y) {
            self.dispatch_key_to(target.plugin, &key);
            self.dirty = true;
        }
    }

    /// Offer a keystroke to one specific plugin: its declared action first, then
    /// its raw handler.
    ///
    /// Unlike `on_key` this never walks the focus order — the caller has already
    /// decided who should get it (the pane under the pointer, or a float).
    fn dispatch_key_to(&mut self, index: usize, key: &KeyEvent) {
        let press = to_press(key);
        let Some(plugin) = self.host.plugins.get(index).map(|p| p.name.clone()) else {
            return;
        };
        if let Some(action) = self
            .registry
            .resolve(&press, Some(&plugin))
            .map(|binding| binding.action.clone())
        {
            match self.host.on_action(index, &action) {
                Ok(true) => return,
                Ok(false) => {}
                Err(e) => self.errors.push(e),
            }
        }
        if let Err(e) = self.host.on_key(index, &press) {
            self.errors.push(e);
        }
    }

    /// Track the affordance under the pointer, repainting only when it changes.
    fn hover(&mut self, x: u16, y: u16) {
        // A modal owns the pointer while it is up, exactly as it owns clicks.
        // It has to be asked directly: it paints cell by cell and records no
        // `click_targets`, so hit-testing them would find only the panes it
        // covers — which are unreachable anyway, which is why the pane hover is
        // dropped rather than left frozen under the dim. v1 draws the same line
        // in `apply_hover_highlight`.
        if self.modals.is_open() {
            let moved = self.modals.on_hover(x, y);
            let dropped = self.hovered.take().is_some();
            if moved || dropped {
                self.dirty = true;
            }
            return;
        }

        let under = self
            .band_target_at(x, y)
            .map(|hit| hit.identity.clone())
            .or_else(|| self.target_at(x, y).map(|target| target.identity))
            .filter(|identity| !identity.is_empty());
        if under != self.hovered {
            self.hovered = under;
            self.dirty = true;
        }
    }

    /// A left press: modal, then link, then target, then a text selection.
    ///
    /// The order is v1's `handle_mouse_click`, minus the scrollbar leg it has
    /// and this does not.
    fn on_click(&mut self, x: u16, y: u16, modifiers: KeyModifiers) {
        // A system modal takes every click while it is up, the mouse half of
        // capturing input: a press that misses its rows is swallowed rather
        // than reaching the pane it covers.
        if self.modals.is_open() {
            let message = self.with_modal_world(|modals, world| modals.on_click(x, y, world));
            if let Some(message) = message {
                self.toast(message);
            }
            self.dirty = true;
            return;
        }

        // A chrome band's button, which is not a pane: pressing one runs its
        // action and leaves focus where it was. v1's footer pills behave the same
        // — you press Help without leaving the terminal you were in.
        if let Some(hit) = self.band_target_at(x, y) {
            if let Some(ClickVerb::Action(action)) = hit.identity.click_verb() {
                // `clicked` is only the fallback owner, and a band has no plugin
                // to fall back to; the action's own declaration is what resolves
                // it, exactly as for a pill drawn by a pane.
                self.run_clicked_action(&action, self.focus);
                self.dirty = true;
                return;
            }
        }

        let target = self.target_at(x, y);

        // A float takes every click while it is up — the mouse half of what
        // makes it a modal rather than a pane drawn on top. A press that misses
        // its buttons is swallowed rather than reaching what it covers.
        if let Some(grabbed) = self.grabbed {
            if let Some(target) = target.filter(|target| target.plugin == grabbed) {
                self.dispatch_click(target, x, y);
            }
            return;
        }

        if modifiers.contains(KeyModifiers::CONTROL) {
            // A modified press is a link open, never the start of a selection.
            self.selection = None;
            self.open_clicked_link(x, y);
            return;
        }

        if let Some(target) = target {
            if self.dispatch_click(target, x, y) {
                return;
            }
        }
        self.begin_selection(x, y);
    }

    /// Act on a hit target. `true` means the press is spent.
    ///
    /// A press that only focused a pane returns `false`, so the *same* press
    /// can still arm a drag-selection over the terminal it just focused — v1's
    /// rule, and why `FocusPane(Terminal)` is one of its two non-consuming
    /// click actions.
    fn dispatch_click(&mut self, target: ClickTarget, x: u16, y: u16) -> bool {
        // Every click focuses the pane it landed in, before the target acts. In
        // a `switch` slot that is also how a view is selected, so a tab pill
        // and a Tab press bring the same pane forward.
        self.focus_plugin(target.plugin);

        match target.identity.click_verb() {
            Some(ClickVerb::Action(action)) => {
                self.run_clicked_action(&action, target.plugin);
                true
            }
            // Replayed as a real keystroke, through the very handler the
            // keyboard uses — so a click on a button cannot do something its
            // key does not.
            Some(ClickVerb::Key(chord)) => {
                if let Some(key) = key_event_from_chord(&chord) {
                    self.on_key(&key);
                }
                true
            }
            Some(ClickVerb::Focus(plugin)) => {
                if let Some(index) = self.host.index_of(&plugin) {
                    self.focus_plugin(index);
                }
                true
            }
            // A pane that paints itself cell by cell (the theme picker, the
            // settings modal) has no per-row nodes to carry identity, so an
            // identity-less hit must still reach it — with coordinates local to
            // its rect, which is all it needs to map y back to a row. Without
            // this the only such panes you could click were the ones built from
            // `widgets.list`, which is exactly the half that worked.
            None => {
                let click = Click {
                    id: target.identity.id.clone(),
                    classes: target.identity.classes.clone(),
                    role: target.identity.role.clone(),
                    x: x.saturating_sub(target.rect.x),
                    y: y.saturating_sub(target.rect.y),
                };
                match self.host.on_click(target.plugin, &click) {
                    Ok(handled) => handled,
                    Err(e) => {
                        self.errors.push(e);
                        false
                    }
                }
            }
        }
    }

    /// Run a declared action, on the plugin that declared it.
    ///
    /// The registry already maps action → owner, so a pill can name an action
    /// belonging to a pane it has never heard of — which is exactly what the
    /// footer does. Falling back to the clicked plugin covers an action a
    /// plugin handles without declaring a key for it.
    fn run_clicked_action(&mut self, action: &str, clicked: usize) {
        // The footer names these by action, as it names a pane's — so a pill
        // reaches a modal the same way its chord does.
        if let Some(kind) = ModalKind::from_action(action) {
            self.toggle_modal(kind);
            return;
        }
        let owner = self
            .registry
            .bindings()
            .iter()
            .find(|binding| binding.action == action)
            .and_then(|binding| self.host.index_of(&binding.plugin))
            .unwrap_or(clicked);
        if let Err(e) = self.host.on_action(owner, action) {
            self.errors.push(e);
        }
    }

    /// Move focus onto a plugin, if focus can rest there at all.
    fn focus_plugin(&mut self, index: usize) {
        if !self.can_focus_plugin(index) {
            return;
        }
        if let Some(position) = self.host.focusable().iter().position(|i| *i == index) {
            if position != self.focus {
                self.focus_return = self.focus;
            }
            self.focus = position;
        }
    }

    /// Open the link under a `Ctrl+Click`, if there is one.
    ///
    /// Silent when there is not: v1 emits no toast for a control-click on plain
    /// text, because the chord is also how you click *through* the terminal.
    fn open_clicked_link(&mut self, x: u16, y: u16) {
        let Some((session, rect)) = self.surface_at(x, y) else {
            return;
        };
        let row = usize::from(y.saturating_sub(rect.y));
        let col = usize::from(x.saturating_sub(rect.x));
        if let Some(url) = self.terminals.url_at(&session, row, col) {
            self.open_or_copy_link(&url);
        }
    }

    /// Open a link, or copy it where nothing can open one.
    ///
    /// v1's `open_ctrl_clicked_url`. A remote host or a bare tty has no
    /// browser, so the clipboard's OSC 52 leg carries the URL back to the
    /// user's own machine instead — the same leg `Ctrl+C` rides — and the toast
    /// says which of the two happened.
    fn open_or_copy_link(&mut self, url: &str) {
        let opened = open_url(url);
        let message = match opened {
            Ok(()) => format!("Opening {url}"),
            Err(reason) => {
                let outcome = thurbox::clipboard::copy(
                    url,
                    self.clipboard.as_mut(),
                    thurbox::session::settings::global().clipboard.provider,
                );
                match outcome {
                    Ok(route) => format!(
                        "{reason} — copied {url} to clipboard{}",
                        route.toast_suffix()
                    ),
                    Err(e) => format!("{reason}, and the clipboard failed: {e}"),
                }
            }
        };
        self.toast(message);
    }

    /// Which session's terminal surface a point falls in, and where that
    /// surface was painted.
    fn surface_at(&self, x: u16, y: u16) -> Option<(String, Rect)> {
        let position = ratatui::layout::Position::new(x, y);
        self.snapshots
            .current()
            .sessions
            .iter()
            .filter_map(|row| Some((row.id.clone(), self.terminals.last_rect(&row.id)?)))
            .find(|(_, rect)| rect.contains(position))
    }

    /// Hand every visible OSC 8 run back to the terminal thurbox runs in.
    ///
    /// The only route to a browser when the agent is on a remote host: the
    /// outer terminal opens the link, so it has to be told the runs are links.
    /// Every attached session is offered rather than only the focused one,
    /// because the paints are validated against the drawn buffer — a session
    /// not painted this frame contributes nothing on its own.
    fn paint_terminal_hyperlinks(&self, buf: &ratatui::buffer::Buffer) {
        let mut paints = Vec::new();
        for row in &self.snapshots.current().sessions {
            paints.extend(self.terminals.hyperlink_paints(&row.id, buf));
        }
        if !paints.is_empty() {
            let _ = thurbox::kernel::terminal::paint_hyperlinks(&paints);
        }
    }

    /// Arm a drag-to-select over the terminal surface under the point.
    ///
    /// Confined to that surface's own rect: a drag that leaves the pane clamps
    /// rather than selecting the session list beside it.
    fn begin_selection(&mut self, x: u16, y: u16) {
        // Anywhere, not only over a terminal. v1 selects across the whole
        // interface and decides how to READ it afterwards — the vt100 grid when
        // the selection sits in a terminal, the painted frame otherwise
        // (`apply_selection_highlight`). v2 refused to start one outside a
        // terminal "because it could not be copied from", which was only true
        // while nothing read the frame buffer.
        //
        // But it is confined to ONE PANE, as v1 confines it
        // (`pane_rects` → `border_block.inner`). Falling back to the whole
        // screen is what made it feel trigger-happy: a press in the session list
        // armed a screen-wide selection, so a single cell of pointer drift
        // painted a band clear across the interface instead of a few columns of
        // the list.
        //
        // Anchoring to the surface rect when there is one keeps grid extraction
        // exact: the pane rect is what converts screen coordinates into grid
        // coordinates.
        let rect = match self.surface_at(x, y) {
            Some((_, rect)) => Some(rect),
            None => self.pane_inner_at(x, y),
        };
        let Some(rect) = rect else {
            // Not inside any pane's content — a border, or a gap. v1 clears the
            // selection here rather than starting one.
            self.selection = None;
            return;
        };
        let pane = PaneBounds::from_rect(rect);
        let (x, y) = pane.clamp(x, y);
        self.selection = Some(Selection::new(
            TermPos {
                row: y as usize,
                col: x as usize,
            },
            pane,
        ));
    }

    /// The content area of the pane under a point.
    ///
    /// The pane's rect is the click target carrying an EMPTY identity, which the
    /// paint walk records before the tree — so this is the same geometry a click
    /// falls back to. The border test itself is
    /// `PaneBounds::content_at`, where it is unit-tested.
    fn pane_inner_at(&self, x: u16, y: u16) -> Option<Rect> {
        let position = ratatui::layout::Position::new(x, y);
        let pane = self
            .click_targets
            .iter()
            .rev()
            .find(|target| target.identity.is_empty() && target.rect.contains(position))
            .map(|target| target.rect)?;
        PaneBounds::content_at(pane, x, y).map(|bounds| bounds.rect())
    }

    fn drag_selection(&mut self, x: u16, y: u16) {
        let Some(selection) = &mut self.selection else {
            return;
        };
        let (x, y) = selection.pane.clamp(x, y);
        selection.cursor = TermPos {
            row: y as usize,
            col: x as usize,
        };
    }

    /// Copy the selection when there is one, else the whole visible screen.
    ///
    /// v1's rule, and worth keeping: `Ctrl+C` with nothing selected should do
    /// something useful rather than nothing.
    fn copy_selection_or_screen(&mut self, session: Option<&str>) {
        // The selection was read off the frame that painted it, whichever pane
        // that was. Falling back to the whole visible screen is v1's rule for
        // "Ctrl+C with nothing selected should do something useful", and needs
        // a terminal to have a screen at all.
        let text = match self.selected_text.clone() {
            Some(text) => Some(text),
            None => session.and_then(|session| self.terminals.visible_text(session)),
        };

        let message = match text {
            Some(text) if !text.trim().is_empty() => {
                let outcome = thurbox::clipboard::copy(
                    &text,
                    self.clipboard.as_mut(),
                    thurbox::session::settings::global().clipboard.provider,
                );
                match outcome {
                    Ok(route) => format!(
                        "copied {} line(s){}",
                        text.lines().count(),
                        route.toast_suffix()
                    ),
                    Err(e) => format!("copy failed: {e}"),
                }
            }
            _ => "nothing to copy".to_string(),
        };
        self.toast(message);
        self.selection = None;
    }

    /// Paste the clipboard into the focused session's terminal.
    ///
    /// Sent as a bracketed paste, so a multi-line paste arrives as text rather
    /// than as a series of submissions — an agent prompt with newlines in it
    /// would otherwise fire on the first one.
    fn paste_into_focused(&mut self) {
        let Some(text) = thurbox::clipboard::paste(self.clipboard.as_mut()) else {
            // No OSC 52 read fallback: terminals disable clipboard *reads* by
            // default and probing for one can stall for seconds. The route that
            // does work here is the terminal's own paste chord, which arrives as
            // `Event::Paste` — so point at it rather than reporting a dead end.
            self.toast(thurbox::clipboard::PASTE_UNAVAILABLE_HINT);
            return;
        };
        self.on_paste(text);
    }

    /// Deliver pasted text to whatever has focus.
    ///
    /// One path for both routes — `ctrl+v` and the terminal's own paste — so the
    /// two cannot come to behave differently. A surface that takes typing gets
    /// the characters replayed as keystrokes, which is how it already receives
    /// them; a terminal gets one bracketed paste, so a prompt with newlines in
    /// it does not fire on the first one.
    fn on_paste(&mut self, text: String) {
        if text.is_empty() {
            return;
        }

        // A modal or a float owns typed input while it is up, so it owns a paste
        // too. Control characters are dropped rather than replayed: `Enter` into
        // a filter or a name field would submit it mid-paste.
        if self.modals.is_open() {
            for ch in text.chars().filter(|ch| !ch.is_control()) {
                self.dispatch_modal_key(&KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
            }
            return;
        }
        if let Some(index) = self.grabbed {
            for ch in text.chars().filter(|ch| !ch.is_control()) {
                self.dispatch_key_to(index, &KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
            }
            self.dirty = true;
            return;
        }

        let Some(session) = self.focused_session.clone() else {
            self.report(
                "nothing to paste into — focus a session's terminal first",
                Level::Error,
            );
            return;
        };
        let mut bytes = Vec::with_capacity(text.len() + 12);
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");

        self.toast(if self.terminals.send(&session, bytes) {
            format!("pasted {} character(s)", text.chars().count())
        } else {
            "no live terminal to paste into".to_string()
        });
    }

    fn cycle_focus(&mut self, step: isize) {
        let focusable = self.host.focusable();
        let count = focusable.len().max(1) as isize;
        // Walk at most one full lap: if nothing is visible (a pathological
        // arrangement), leave focus where it was rather than spinning.
        for hop in 1..=count {
            let next = (self.focus as isize + step * hop).rem_euclid(count) as usize;
            if focusable
                .get(next)
                .is_some_and(|index| self.can_focus_plugin(*index))
            {
                self.focus = next;
                return;
            }
        }
    }

    /// Where a plugin sits, as the two focus rules need to see it.
    fn placement(&self, index: usize) -> thurbox::kernel::focus::Placement {
        let Some(plugin) = self.host.plugins.get(index) else {
            return thurbox::kernel::focus::Placement {
                floats: false,
                float_open: false,
                slot_placed: false,
                chosen_in_switch: None,
            };
        };
        // A `switch` slot draws ONE of its occupants; the rest are alternatives.
        let chosen_in_switch =
            matches!(self.host.slot_mode(&plugin.slot), SlotMode::Switch).then(|| {
                // `slot_selection` stores a position WITHIN the slot's members,
                // not a plugin index — comparing it to one made the terminal
                // look unselected and cycled focus off it at startup. Resolve it.
                let members = self.host.in_slot(&plugin.slot);
                let chosen = self.slot_selection.get(&plugin.slot).copied().unwrap_or(0);
                members.get(chosen) == Some(&index)
            });
        thurbox::kernel::focus::Placement {
            floats: plugin.floats,
            float_open: self.drawn_floats.contains(&index),
            slot_placed: self.visible_slots.contains(&plugin.slot),
            chosen_in_switch,
        }
    }

    /// Whether a plugin would be drawn right now: its slot was placed on the
    /// last frame, or it floats above the arrangement and so needs no slot.
    fn is_visible_plugin(&self, index: usize) -> bool {
        thurbox::kernel::focus::is_drawn(self.placement(index))
    }

    /// Whether focus may rest on a plugin — which for a `switch` alternate is
    /// **not** the same question, since focusing it is what brings it forward.
    /// See `kernel::focus`.
    fn can_focus_plugin(&self, index: usize) -> bool {
        thurbox::kernel::focus::can_focus(self.placement(index))
    }

    /// Plugins may have come and gone under us; focus must land on one that
    /// still exists.
    fn clamp_focus(&mut self) {
        let count = self.host.focusable().len();
        self.focus = self.focus.min(count.saturating_sub(1));
    }
}

/// Clamp a span into `floor..=cap`, tolerating a `cap` below the `floor`.
///
/// `u16::clamp` asserts `min <= max`, and every rect below takes its cap from the
/// space available — which on a short terminal is smaller than the floor the
/// content wants. The cap wins, because a rect must never exceed its parent.
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
    let width = 30.min(area.width);
    let height = 8.min(area.height);
    Rect {
        x: area.x + area.width - width,
        y: area.y,
        width,
        height,
    }
}

/// Paint the counters.
///
/// Counts rather than timings, which is the whole point of `kernel::perf`: a
/// number that says "an idle loop painted no frames" is exact, where one that
/// says "idle was fast" is a coin toss on shared hardware.
fn render_hud(frame: &mut Frame, area: Rect, counters: &thurbox::kernel::perf::Snapshot) {
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
    let text = format!(
        "iterations {}\nframes     {}\nskipped    {}\nrenders    {}\nfailures   {}\nreloads    {}",
        counters.iterations,
        counters.frames,
        counters.skipped,
        counters.renders,
        counters.failures,
        counters.reloads,
    );
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
