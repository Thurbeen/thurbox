use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ratatui::layout::Rect;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::Layer as _;

use thurbox::kernel::command::CommandBus;
use thurbox::kernel::diff::DiffStore;
use thurbox::kernel::host::LuaHost;
use thurbox::kernel::metrics::Metrics;
use thurbox::kernel::modals::Modals;
use thurbox::kernel::notify::Notifier;
use thurbox::kernel::perf::Counters;
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::SnapshotStore;
use thurbox::kernel::terminal::Terminals;
use thurbox::kernel::theme::Themes;
use thurbox::kernel::watch::Watcher;

use super::{enable_mouse_clicks, push_keyboard_enhancement, restore_terminal};
use crate::App;

const V2_STATE_NOTICE: &str = "thurbox 2.x state found · thurbox-cli migrate --dry-run";

const LOG_FILES_KEPT: usize = 30;

fn log_appender(dir: PathBuf) -> tracing_appender::rolling::RollingFileAppender {
    tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("thurbox.log")
        .max_log_files(LOG_FILES_KEPT)
        .build(dir)
        .expect("a daily log appender in the data dir")
}

pub(crate) async fn run() -> Result<(), Box<dyn Error>> {
    // A panic in raw mode leaves mouse reporting on, and the terminal streams reports at the next shell.
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

    // The default action of SIGHUP, SIGTERM and SIGINT runs no panic hook.
    install_signal_restore();

    // tracing-appender stops writing when its guard drops, and the panic hook logs until the process ends.
    let log_dir = thurbox::paths::log_directory().unwrap_or_else(|| std::path::PathBuf::from("."));
    let create = log_dir.clone();
    tokio::task::spawn_blocking(move || std::fs::create_dir_all(create))
        .await
        .ok();
    let (writer, guard) = tracing_appender::non_blocking(log_appender(log_dir));
    Box::leak(Box::new(guard));
    let file_layer = || {
        tracing_subscriber::fmt::layer()
            .with_writer(writer.clone())
            .with_ansi(false)
            .with_filter(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive("thurbox=debug".parse().unwrap_or_default())
                    .add_directive("runtime_mux=debug".parse().unwrap_or_default()),
            )
    };

    let process_start = Instant::now();
    let mut startup = thurbox::kernel::perf::Startup::default();

    // A tracing global subscriber cannot be replaced once installed.
    let phase = Instant::now();
    let (config, config_warnings) = tracing::subscriber::with_default(
        tracing_subscriber::registry().with(file_layer()),
        thurbox::kernel::config::Config::load,
    );
    startup.config_init_ms = phase.elapsed().as_millis() as u64;
    let startup_settings = config.in_force().clone();

    let (bridge, bridge_error) = match thurbox::telemetry::init(
        &startup_settings.telemetry,
        env!("THURBOX_VERSION"),
        &thurbox::backend::instance::local_socket_name(),
    ) {
        Ok((bridge, logs_off)) => (bridge, logs_off),
        Err(e) => (None, Some(format!("telemetry is off: {e}"))),
    };
    tracing_subscriber::registry()
        .with(file_layer())
        .with(bridge)
        .init();
    if let Some(e) = bridge_error {
        tracing::warn!("{e}");
    }
    for warning in &config_warnings {
        tracing::warn!("{warning}");
    }

    let backends = std::sync::Arc::new(thurbox::backend::wiring::local_only(&startup_settings));

    let mut startup_notices: Vec<String> = config_warnings;
    if thurbox::paths::v2_state_unmigrated() {
        startup_notices.push(V2_STATE_NOTICE.to_string());
    }

    let (ui_dir, ui_notices, recovering) = resolve_ui_dir()?;
    startup_notices.extend(ui_notices);

    let ui_phase = Instant::now();
    let host = LuaHost::new(&ui_dir);

    let initial_focus = focus_index_of(&host, "agent");

    let phase = Instant::now();
    let fleets = thurbox::fleet_link::Fleets::of(&startup_settings);
    let fleet = fleets.in_force().clone();
    let links = fleets.all().to_vec();
    let snapshots = SnapshotStore::with_source(
        Box::new(thurbox::kernel::snapshot::fleet::FleetWatches::new(
            fleet.name(),
            links
                .iter()
                .cloned()
                .map(thurbox::kernel::snapshot::fleet::FleetWatch::spawn)
                .collect(),
        )),
        &startup_settings,
    );
    startup.snapshot_open_ms = phase.elapsed().as_millis() as u64;

    let phase = Instant::now();
    let themes = Themes::load(thurbox::config::interface_state::load().theme.as_deref());
    startup.theme_activate_ms = phase.elapsed().as_millis() as u64;

    let control = match thurbox::ui_control::Server::start() {
        Ok(control) => Some(control),
        Err(error) => {
            tracing::warn!("local UI control unavailable: {error}");
            startup_notices.push(format!("local UI control unavailable: {error}"));
            None
        }
    };
    let mut app = App {
        control,
        control_revision: 0,
        control_observed: None,
        control_events: std::collections::VecDeque::new(),
        control_tickets: std::collections::HashMap::new(),
        control_state_version: 0,
        control_registry_version: 0,
        control_placed: Vec::new(),
        control_floats: std::collections::HashSet::new(),
        control_focus: 0,
        control_modal: (None, None, None),
        host,
        sources: thurbox::kernel::bundled::sources(&ui_dir),
        watcher: Watcher::new(&ui_dir)?,
        ui_dir,
        snapshots,
        terminals: {
            let mut terminals = terminals(std::sync::Arc::clone(&backends), &startup_settings);
            for link in &links {
                terminals.attach_fleet_through(link.clone());
            }
            terminals
        },
        commands: CommandBus::fleet(fleets),
        diffs: DiffStore::new(),
        repos: thurbox::kernel::repos::RepoStore::with_fleets(&links),
        metrics: Metrics::new(),
        clipboard: arboard::Clipboard::new().ok(),
        image_probe: thurbox::clipboard::ImageProbe::default(),
        paste_targets: Vec::new(),
        held_input: Default::default(),
        probed_presses: 0,
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
        echo: std::collections::VecDeque::new(),
        echo_due: None,
        last_frame: None,
        last_keystroke: None,
        animation_tick: 0,
        animation_step: 0,
        selection: None,
        wheel_notch: crate::coordinator::mouse::WheelNotch::default(),
        click_train: crate::coordinator::mouse::ClickTrain::default(),
        notifier: Notifier::new(
            startup_settings.features.notifications,
            startup_settings.notifications,
        ),
        themes,
        updates: thurbox::kernel::updates::Updates::start(config.features(), links.clone()),
        slot_selection: std::collections::HashMap::new(),
        visible_slots: std::collections::HashSet::new(),
        pending_focus: None,
        editor_request: None,
        click_targets: Vec::new(),
        last_area: Rect::new(0, 0, 0, 0),
        screen_size: crossterm::terminal::size().unwrap_or((80, 24)),
        selected_text: None,
        copy_after_paint: false,
        hovered: None,
        mouse: config.features().mouse,
        paste_burst: crate::coordinator::paste::PasteBurst::for_platform(),
        holds: Default::default(),
        config,
        registry: match recovering {
            true => Registry::default(),
            false => Registry::load(),
        },
        modals: Modals::default(),
        focus: initial_focus,
        focus_return: initial_focus,
        reload_at: None,
        errors: Vec::new(),
        failing: std::collections::HashMap::new(),
        links: std::collections::HashMap::new(),
        link_stamps: std::collections::HashMap::new(),
        link_scans: std::collections::HashMap::new(),
        last_link_paints: Vec::new(),
        search: thurbox::kernel::search::SearchStore::new(),
        trust: std::collections::HashMap::new(),
        trust_stale: true,
        layout_error: None,
        floor: None,
        floor_cause: None,
        memory_rebuilt: None,
        status: None,
        startup_notices: startup_notices.into(),
        startup_notice_due: None,
        reported_failures: std::collections::HashSet::new(),
        tracked_commands: std::collections::HashMap::new(),
        band_targets: Vec::new(),
        focused_session: None,
        diffs_shown: false,
        focused_surface: None,
        focused_typing: false,
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
        pointer_grab: None,
        pty_pointer: None,
        runs: thurbox::kernel::runs::RunStore::new(),
        inventory: Vec::new(),
        bookmark_in_flight: false,
        report_after: None,
        hud: false,
        events: crate::coordinator::events::Events::new(),
        quit: false,
    };

    app.publish_disabled();
    app.reload_interface(crate::coordinator::events::ReloadReason::Boot);
    app.collect_declarations();
    app.startup.ui_build_ms = ui_phase.elapsed().as_millis() as u64;
    if let Some(notice) = app.startup_notices.pop_front() {
        app.toast(notice);
        if !app.startup_notices.is_empty() {
            app.startup_notice_due = Some(Instant::now() + crate::STATUS_TTL);
        }
    }

    let terminal = ratatui::init();

    // The kitty keyboard protocol tells `cmd+…` and `ctrl+/` apart; `ratatui::restore()` does not pop it.
    app.holds.releases = push_keyboard_enhancement();
    if app.mouse {
        enable_mouse_clicks();
    }
    // Without bracketed paste, the first newline of a multi-line paste submits the prompt.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste);
    let result = app.run(terminal);
    restore_terminal();
    drop(app);
    backends.shutdown_all();
    thurbox::telemetry::shutdown();
    result
}

// After SIGHUP the loop may be reading a pty that is gone; exit here with the shell's 128 + signal.
fn install_signal_restore() {
    fn restore_and_exit(name: &str, number: i32) -> ! {
        restore_terminal();
        tracing::info!("exiting on {name}");
        std::process::exit(128 + number)
    }

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        // `SignalKind` does not expose the POSIX signal numbers portably.
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
        // Ctrl+C, Ctrl+Break and a closed console window all arrive here.
        tokio::spawn(async {
            if tokio::signal::ctrl_c().await.is_ok() {
                restore_and_exit("SIGINT", 2);
            }
        });
    }
}

fn focus_index_of(host: &LuaHost, name: &str) -> usize {
    host.focusable()
        .iter()
        .position(|index| {
            host.mods
                .get(*index)
                .is_some_and(|loaded| loaded.name == name)
        })
        .unwrap_or(0)
}

fn resolve_ui_dir() -> Result<(PathBuf, Vec<String>, bool), Box<dyn Error>> {
    let migration = thurbox::kernel::mod_migration::migrate_in_force();
    if !migration.errors.is_empty() {
        let dir = thurbox::kernel::bundled::fallback_dir()?;
        let mut notices = Vec::new();
        notices.extend(migration.notice());
        notices.extend(directory_notice(
            &dir,
            thurbox::kernel::bundled::Chosen::Fallback,
        ));
        notices.push(RECOVERY_NOTICE.to_string());
        return Ok((dir, notices, true));
    }
    let (dir, chosen, report) = thurbox::kernel::bundled::resolve(true)?;
    let mut notices = Vec::new();
    notices.extend(migration.notice());
    notices.extend(directory_notice(&dir, chosen));
    notices.extend(delivery_notice(&report));
    Ok((dir, notices, false))
}

const RECOVERY_NOTICE: &str =
    "until the move to mods succeeds, trust, turned-off panes, mod settings and rebound keys changed here are not saved";

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
        Chosen::Fallback => Some(format!(
            "interface from {shown} — your copy could not be written"
        )),
    }
}

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

fn terminals(
    backends: std::sync::Arc<thurbox::backend::BackendRegistry>,
    settings: &thurbox::session::settings::Settings,
) -> Terminals {
    use thurbox::kernel::terminal::stream_pane;
    let mut terminals = Terminals::with_registry(backends, settings);
    if settings.stream_panes {
        match stream_pane::view_program() {
            Some(program) => terminals.view_through(stream_pane::View {
                program,
                socket: thurbox::backend::instance::local_socket_name(),
                scrollback: settings.scrollback_lines,
            }),
            None => tracing::warn!(
                "stream_panes is on, but no runtime-mux program is beside thurbox or on PATH: panes use control mode"
            ),
        }
    }
    terminals
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_appender_keeps_a_months_worth_and_no_more() {
        use std::io::Write as _;

        let dir = tempfile::tempdir().expect("tempdir");

        let seeded = LOG_FILES_KEPT + 5;
        for nth in 0..seeded {
            // overlayfs records no birth time, so a file named with an impossible date is never pruned.
            let day = format!("2000-{:02}-{:02}", nth / 28 + 1, nth % 28 + 1);
            std::fs::write(dir.path().join(format!("thurbox.log.{day}")), b"old\n")
                .expect("seed a day of logs");
        }
        std::fs::write(dir.path().join("version-check.json"), b"{}\n").expect("seed");

        let mut appender = log_appender(dir.path().to_path_buf());
        appender.write_all(b"today\n").expect("write a line");
        appender.flush().expect("flush");

        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read the log dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();

        let logs: Vec<&String> = names
            .iter()
            .filter(|n| n.starts_with("thurbox.log."))
            .collect();
        assert_eq!(
            logs.len(),
            LOG_FILES_KEPT,
            "seeded {seeded} days, kept {}: {logs:?}",
            logs.len()
        );
        assert!(
            names.iter().any(|n| n == "version-check.json"),
            "the cap deleted a file that was not a log: {names:?}"
        );

        let today = logs
            .iter()
            .find(|n| !n.starts_with("thurbox.log.2000-"))
            .unwrap_or_else(|| panic!("the line just written went somewhere else: {logs:?}"));
        let date = today
            .strip_prefix("thurbox.log.")
            .expect("checked by the filter above");
        assert_eq!(
            date.len(),
            "2000-01-01".len(),
            "the live log is not `thurbox.log.<date>`: {today}"
        );
    }

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

        for chosen in [Chosen::Override, Chosen::Checkout, Chosen::Fallback] {
            let said = directory_notice(dir, chosen)
                .unwrap_or_else(|| panic!("{chosen:?} must be announced"));
            assert!(
                said.contains("interface from"),
                "{chosen:?} names the directory: {said}"
            );
        }
        let fallback = directory_notice(dir, Chosen::Fallback).expect("announced");
        assert!(
            fallback.contains("could not be written"),
            "it says why, or it reads as a choice: {fallback}"
        );
    }
}
