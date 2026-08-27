//! The boot sequence: everything between process start and the loop.
//!
//! `main` itself stays a one-liner in `main.rs` beside the state it constructs;
//! the order here is load-bearing and each step's comment says why it sits
//! where it does — the panic hook before anything can panic, extensions before
//! the interface takes the terminal, the consent gate before the interface is
//! even built.

use super::*;

pub(crate) async fn run() -> Result<(), Box<dyn Error>> {
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

    // The same restore for a signal, which the panic hook cannot see. A
    // `SIGHUP` (the terminal or ssh session went away), a `SIGTERM` (a session
    // manager, or the machine waking from a long sleep and reaping what it
    // suspended) or a `kill -INT` ends the process with the default action,
    // and the default action runs no hook — so mouse reporting stayed on and
    // the next shell printed `\x1b[<64;…M` on every scroll. Spawned before the
    // terminal is taken: every step of `restore_terminal` is a no-op for a mode
    // that was never enabled, and a signal that lands during boot is otherwise
    // the one this misses.
    install_signal_restore();

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

    // The user's settings, published process-wide BEFORE anything reads one.
    // `Database::open` below reads a restart-only value — it prunes the audit log
    // to `audit_retention_days` — and v1 loads them at the same point for the same
    // reason. Without this call `settings::global()` hands out `Settings::default`
    // and the whole file is ignored, however carefully it was written.
    let phase = Instant::now();
    let (config, config_warnings) = thurbox::kernel::config::Config::load();
    startup.config_init_ms = phase.elapsed().as_millis() as u64;

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
    // Where a peer sharing sessions with this machine looks for its CLI: a
    // pointer to this build's own, so a dev checkout is found before a release
    // install on PATH (ADR-24).
    thurbox::session_ops::host_cli::advertise_running_cli();
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
        paste_burst: crate::coordinator::paste::PasteBurst::for_platform(),
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
        events: crate::coordinator::events::Events::new(),
        quit: false,
    };

    // The user's decisions have to reach the host before the interface it will
    // run is built: `LuaHost::new` already built one, from every file on disk,
    // so a plugin turned off would be loaded for exactly one frame. Told, then
    // rebuilt.
    app.publish_disabled();
    app.reload_interface(crate::coordinator::events::ReloadReason::Boot);
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

/// Put the terminal back and exit when a signal ends the process.
///
/// Exits directly rather than asking the loop to quit: the loop is on the
/// thread blocked in `app.run`, and after a `SIGHUP` its `event::poll` may be
/// reading a pty that is already gone, so a request would be honoured late or
/// never. Exiting from the runtime's thread skips the loop's drops, exactly as
/// the panic path does — tmux keeps the sessions alive, so nothing is lost.
/// The status is the shell's convention, `128 + signal`, so a wrapper script
/// can tell a signal from a clean quit.
///
/// `restore_terminal` is idempotent and takes no lock, so racing the normal
/// exit path — a `SIGTERM` that lands while `Ctrl+Q` is being honoured —
/// costs a redundant escape and nothing else.
fn install_signal_restore() {
    fn restore_and_exit(name: &str, number: i32) -> ! {
        restore_terminal();
        tracing::info!("exiting on {name}");
        std::process::exit(128 + number)
    }

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        // The numbers are POSIX's (1, 2, 15 on Linux and macOS alike), spelled
        // here because `SignalKind` does not expose them portably and `libc`
        // is a dev-dependency only.
        let hangup = ("SIGHUP", signal(SignalKind::hangup()), 1);
        let terminate = ("SIGTERM", signal(SignalKind::terminate()), 15);
        let interrupt = ("SIGINT", signal(SignalKind::interrupt()), 2);
        for (name, stream, number) in [hangup, terminate, interrupt] {
            match stream {
                Ok(mut stream) => {
                    tokio::spawn(async move {
                        if stream.recv().await.is_some() {
                            restore_and_exit(name, number);
                        }
                    });
                }
                Err(e) => tracing::warn!("could not listen for {name}: {e}"),
            }
        }
    }
    #[cfg(not(unix))]
    {
        // Ctrl+C / Ctrl+Break / a closed console window all arrive here.
        tokio::spawn(async {
            if tokio::signal::ctrl_c().await.is_ok() {
                restore_and_exit("SIGINT", 2);
            }
        });
    }
}

/// Index into the host's focusable plugins for `name`, or 0.
///
/// `App::focus` indexes the FOCUSABLE list, not the plugin list, so this cannot
/// just be `plugins.position(...)`.
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

/// Find the plugin directory.
///
/// Two rules, in order: `THURBOX_UI_DIR`, then the user's own copy —
/// materialized from the embedded interface on first run, preserving anything
/// they edited. A missing or unwritable config directory is not fatal: the
/// embedded copies are written somewhere throwaway and used from there, because
/// no interface at all is the one outcome worth avoiding (design.md D11).
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
