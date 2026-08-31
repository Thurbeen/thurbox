//! What `settings.toml` controls in v2.
//!
//! The bug this file exists to prevent is not a wrong value — it is a setting
//! that is *read nowhere*. `thurbox2` used to never call `settings::init`, so
//! `settings::global()` handed out defaults and the entire file was ignored
//! however carefully it had been written. Nothing about that failure was visible:
//! every switch looked honoured because every default matched.
//!
//! So these tests assert the reach of a setting rather than its parsing: that the
//! file is what is in force, that a change to it lands where it has to, and that a
//! plugin can see it.

use thurbox::kernel::config::{Config, Reloaded};
use thurbox::session::settings::Settings;

/// Isolate config and data into a tempdir, process-wide so a worker sees it too.
fn isolate() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    let config = home.path().join("config");
    let data = home.path().join("data");
    std::fs::create_dir_all(&config).expect("mkdir");
    std::fs::create_dir_all(&data).expect("mkdir");
    std::env::set_var("THURBOX_CONFIG_DIR", &config);
    std::env::set_var("THURBOX_DATA_DIR", &data);
    thurbox::paths::set_test_dir(&data);
    home
}

fn write_settings(body: &str) {
    let path = thurbox::agent::settings_config::settings_config_path().expect("settings path");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&path, body).expect("write settings.toml");
}

fn read_settings() -> String {
    let path = thurbox::agent::settings_config::settings_config_path().expect("settings path");
    std::fs::read_to_string(path).expect("read settings.toml")
}

// ── The file reaches the interface at all ──────────────────────────────────

#[test]
fn the_file_is_what_is_in_force() {
    let _home = isolate();
    write_settings(
        "[features]\n\
         soft_delete = false\n\
         perf_hud = false\n\
         mouse = false\n\
         \n\
         [notifications]\n\
         sound = false\n\
         min_interval_secs = 30\n",
    );

    let (config, warnings) = Config::load();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(!config.features().soft_delete);
    assert!(!config.features().perf_hud);
    assert!(!config.features().mouse);
    assert!(!config.in_force().notifications.sound);
    assert_eq!(config.in_force().notifications.min_interval_secs, 30);
}

#[test]
fn loading_publishes_the_settings_process_wide() {
    // The half of the failure that was invisible: everything reading
    // `settings::global()` — including `Database::open`, which prunes the audit
    // log to `audit_retention_days` — sees defaults until this is done.
    let _home = isolate();
    write_settings("audit_retention_days = 7\n");

    let (config, _) = Config::load();
    assert_eq!(config.in_force().audit_retention_days, 7);
    assert_eq!(
        thurbox::session::settings::global().audit_retention_days,
        7,
        "a restart-only value has to be readable through the global, or the \
         callers that read it there are still on defaults"
    );
}

#[test]
fn audit_history_is_pruned_to_the_configured_retention() {
    // `Database::open` prunes, reading the retention from the published settings —
    // which is why publishing them before the first open is load-bearing.
    let _home = isolate();
    write_settings("audit_retention_days = 1\n");
    let (_config, _) = Config::load();

    let path = thurbox::paths::database_file().expect("db path");
    let db = thurbox::storage::Database::open(&path).expect("open");
    let old = thurbox::sync::current_time_millis() - 5 * 24 * 60 * 60 * 1000;
    db.conn_ref()
        .execute(
            "INSERT INTO audit_log (timestamp, entity_type, entity_id, action) \
             VALUES (?1, 'session', 'x', 'created')",
            rusqlite::params![old as i64],
        )
        .expect("insert");

    assert_eq!(db.prune_audit_log().expect("prune"), 1);
}

// ── Reload ─────────────────────────────────────────────────────────────────

#[test]
fn a_live_change_on_disk_applies_without_a_restart() {
    let _home = isolate();
    write_settings("[features]\nsoft_delete = true\n");
    let (mut config, _) = Config::load();
    assert!(config.features().soft_delete);

    write_settings("[features]\nsoft_delete = false\n");
    // The poll is throttled and mtime-based; a fresh read is what the loop would
    // eventually do, so drive `adopt` with what the file now says.
    let (fresh, _) = thurbox::agent::settings_config::load_or_seed_with_warnings();
    assert_eq!(config.adopt(fresh), Reloaded::Live);
    assert!(!config.features().soft_delete);
}

#[test]
fn a_restart_only_change_is_reported_rather_than_implied() {
    let _home = isolate();
    write_settings("scrollback_lines = 10000\n");
    let (mut config, _) = Config::load();

    write_settings("scrollback_lines = 50000\n");
    let (fresh, _) = thurbox::agent::settings_config::load_or_seed_with_warnings();
    assert_eq!(config.adopt(fresh), Reloaded::NeedsRestart);
    assert_eq!(
        config.in_force().scrollback_lines,
        10000,
        "the parsers were built with the old value, so that is what is in force"
    );
}

#[test]
fn an_unreadable_file_keeps_what_was_in_force() {
    let _home = isolate();
    write_settings("[features]\nsoft_delete = false\n");
    let (mut config, _) = Config::load();
    assert!(!config.features().soft_delete);

    // Not valid TOML at all.
    write_settings("[features\nsoft_delete = ???\n");
    let outcome = config.poll();
    assert!(
        matches!(outcome, None | Some(Reloaded::Failed(_))),
        "{outcome:?}"
    );
    assert!(
        !config.features().soft_delete,
        "a broken file must not silently reset a switch to its default"
    );
}

#[test]
fn our_own_write_is_not_reported_as_an_outside_edit() {
    let _home = isolate();
    write_settings("[features]\nsoft_delete = true\n");
    let (mut config, _) = Config::load();

    let mut edited = Settings::default();
    edited.features.soft_delete = false;
    thurbox::agent::settings_config::save_settings(&edited).expect("save");
    config.mark_saved();

    assert_eq!(
        config.poll(),
        None,
        "the interface's own save must not come back as somebody else's change"
    );
}

/// The settings panel edits the *file*, so it drafts from the file.
///
/// The bug: it drafted from what was in force, and a restart-only change is
/// deliberately never taken into force. So one visit saved `mouse = false`
/// correctly, and the next visit — any visit, changing anything — carried the
/// stale `mouse = true` back into the document and reverted it. Two thirds of
/// the panel's core rows are restart-only, which made "my settings do not
/// survive a restart" the everyday symptom.
#[test]
fn a_saved_restart_only_change_is_not_reverted_by_the_next_save() {
    let _home = isolate();
    let (mut config, _) = Config::load();

    // One visit to the panel: seed a draft, change a restart-only row, save.
    // The loop adopts the draft and writes it, which is `App::apply_settings`.
    let mut draft = config.on_disk().clone();
    draft.features.mouse = false;
    assert_eq!(config.adopt(draft.clone()), Reloaded::NeedsRestart);
    thurbox::agent::settings_config::save_settings(&draft).expect("save");

    // The next visit, with the modal (and its draft) long since dropped: change
    // something else entirely.
    let mut draft = config.on_disk().clone();
    draft.features.soft_delete = false;
    // Still `NeedsRestart`: the earlier change is pending until the next launch,
    // and saying otherwise would report it as applied.
    assert_eq!(config.adopt(draft.clone()), Reloaded::NeedsRestart);
    thurbox::agent::settings_config::save_settings(&draft).expect("save");

    let (next_launch, _) = thurbox::agent::settings_config::load_or_seed_with_warnings();
    assert!(
        !next_launch.features.mouse,
        "the restart-only change was reverted by an unrelated later save"
    );
    assert!(!next_launch.features.soft_delete, "and the live one landed");
}

/// The other half of the same rule: what is *in force* still refuses the
/// restart-only change, or the split this all rests on would be gone.
#[test]
fn the_file_moves_ahead_of_what_is_in_force() {
    let _home = isolate();
    let (mut config, _) = Config::load();

    let mut draft = config.on_disk().clone();
    draft.features.mouse = false;
    config.adopt(draft);

    assert!(!config.on_disk().features.mouse, "the file has the change");
    assert!(
        config.features().mouse,
        "and it is still not in force, because the escape was already sent"
    );
}

#[test]
fn saving_preserves_the_files_comments() {
    // The seed is documented inline, and a save that regenerated the file would
    // throw that away — which is why `save_settings` edits the document.
    let _home = isolate();
    write_settings(
        "# my notes about this profile\n\
         audit_retention_days = 30\n",
    );
    let mut edited = Settings::default();
    edited.features.soft_delete = false;
    thurbox::agent::settings_config::save_settings(&edited).expect("save");

    let written = read_settings();
    assert!(
        written.contains("# my notes about this profile"),
        "{written}"
    );
}

// ── What the switches do to the interface ──────────────────────────────────

/// The bundled interface, loaded from the repo's own `ui/`.
fn host() -> thurbox::kernel::host::LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    let host = thurbox::kernel::host::LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

/// One session with work at risk, so a confirmation has something to itemise.
fn session_row() -> thurbox::kernel::snapshot::SessionRow {
    thurbox::kernel::snapshot::SessionRow {
        id: "11111111-1111-1111-1111-111111111111".into(),
        name: "fix-osc52".into(),
        agent: "claude".into(),
        status: "idle".into(),
        cwd: Some(std::path::PathBuf::from("/src/thurbox")),
        repo: Some("thurbox".into()),
        repos: vec!["thurbox".into()],
        branch: Some("fix/osc52".into()),
        base_branch: None,
        backend: "local-tmux".into(),
        backend_id: Some("%1".into()),
        remote_host: None,
        agent_session_id: None,
        parent_id: None,
        display_order: None,
        worktree_count: 1,
        git: Some(thurbox::kernel::snapshot::GitState {
            files_changed: 3,
            insertions: 10,
            deletions: 2,
            untracked: 1,
            dirty: true,
            ahead: 2,
            behind: 0,
        }),
        stopped: false,
        hook_state: None,
        shell_backend_id: None,
        member_dirs: Vec::new(),
    }
}

fn publish_with(host: &thurbox::kernel::host::LuaHost, settings: &Settings) {
    let themes = thurbox::kernel::theme::Themes::load(None);
    let mut registry = thurbox::kernel::registry::Registry::default();
    let (bindings, declared) = host.declarations();
    registry.declare(bindings, declared);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    let snapshot = thurbox::kernel::snapshot::Snapshot {
        sessions: vec![session_row()],
        ..Default::default()
    };
    host.publish(&thurbox::kernel::host::Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
        snapshot: &snapshot,
        attach_errors: &Default::default(),
        inflight: &[],
        themes: &themes,
        registry: &registry,
        diffs: &diffs,
        links: &Default::default(),
        content: &Default::default(),
        meta: &Default::default(),
        metrics: &Default::default(),
        status_rows: 0,
        can_open: true,
        inventory: &[],
        ui_dir: "ui",
        settings,
        repos: &repos,
        wants: &Default::default(),
        focus: None,
        hovered: None,
    })
    .expect("publish");
}

/// Send a chord to a plugin the way the loop does.
fn press(host: &thurbox::kernel::host::LuaHost, plugin: &str, chord: &str) {
    let index = host
        .index_of(plugin)
        .unwrap_or_else(|| panic!("no {plugin}"));
    let mut key = thurbox::kernel::host::KeyPress {
        name: chord.to_string(),
        ..Default::default()
    };
    if chord.chars().count() == 1 {
        key.ch = chord.chars().next();
    }
    let mut registry = thurbox::kernel::registry::Registry::default();
    let (bindings, declared) = host.declarations();
    registry.declare(bindings, declared);
    if let Some(binding) = registry.resolve(&key, Some(plugin)) {
        let action = binding.action.clone();
        if host.on_action(index, &action).expect("on_action") {
            return;
        }
    }
    host.on_key(index, &key).expect("on_key");
}

/// Whether the confirmation is up, and what it says.
fn confirmation(host: &thurbox::kernel::host::LuaHost, settings: &Settings) -> Option<String> {
    publish_with(host, settings);
    let index = host.index_of("confirm").expect("no confirm plugin");
    let rendered = host
        .render(
            index,
            thurbox::kernel::host::RenderContext {
                width: 120,
                height: 40,
                focused: true,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render");
    let float = rendered.float?;
    let rows = float.rows.expect("the confirmation sizes its own height");
    let width = (120.0 * float.width_pct / 100.0) as u16;
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, rows)).expect("terminal");
    terminal
        .draw(|frame| {
            thurbox::kernel::paint::render_recording(
                frame,
                ratatui::layout::Rect::new(0, 0, width, rows),
                &rendered.node,
                &thurbox::kernel::terminal::Terminals::new(),
                &mut Vec::new(),
            );
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    Some(
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn with_soft_delete(on: bool) -> Settings {
    let mut settings = Settings::default();
    settings.features.soft_delete = on;
    settings
}

#[test]
fn deleting_with_soft_delete_on_is_reversible_and_asks_nothing() {
    let host = host();
    let settings = with_soft_delete(true);
    publish_with(&host, &settings);
    press(&host, "sessions", "d");

    assert_eq!(
        host.drain_commands(),
        vec![thurbox::kernel::command::Command::Delete {
            session: "11111111-1111-1111-1111-111111111111".into(),
            force: false,
        }],
        "a soft delete goes straight through — Ctrl+Z is the safety net"
    );
    assert!(confirmation(&host, &settings).is_none());
}

#[test]
fn deleting_with_soft_delete_off_confirms_and_itemises_the_loss() {
    let host = host();
    let settings = with_soft_delete(false);
    publish_with(&host, &settings);
    press(&host, "sessions", "d");

    assert!(
        host.drain_commands().is_empty(),
        "nothing is torn down before the question is answered"
    );
    let screen = confirmation(&host, &settings).expect("the confirmation must be up");
    assert!(screen.contains("fix-osc52"), "{screen}");
    assert!(screen.contains("for good"), "{screen}");
    // What v1 itemises: uncommitted work, unpushed commits, the worktree.
    assert!(
        screen.contains("4 uncommitted or untracked file(s)"),
        "{screen}"
    );
    assert!(screen.contains("2 commit(s) not pushed"), "{screen}");
    assert!(screen.contains("worktree directory"), "{screen}");

    // Answering it is what deletes, and it deletes for real.
    press(&host, "confirm", "y");
    assert_eq!(
        host.drain_commands(),
        vec![thurbox::kernel::command::Command::Delete {
            session: "11111111-1111-1111-1111-111111111111".into(),
            force: true,
        }]
    );
    assert!(
        confirmation(&host, &settings).is_none(),
        "and the question goes away with it"
    );
}

#[test]
fn declining_the_confirmation_deletes_nothing() {
    let host = host();
    let settings = with_soft_delete(false);
    publish_with(&host, &settings);
    press(&host, "sessions", "d");
    assert!(confirmation(&host, &settings).is_some());

    press(&host, "confirm", "esc");
    assert!(host.drain_commands().is_empty());
    assert!(confirmation(&host, &settings).is_none());
}

// ── Updates ────────────────────────────────────────────────────────────────

#[test]
fn with_both_update_switches_off_nothing_is_fetched_or_replaced() {
    // The switch has to mean "do not go to the network", not "go, then discard":
    // a machine with no route out must cost nothing, and a user who said no must
    // not have binaries replaced.
    let _home = isolate();
    write_settings("[features]\nversion_check = false\nauto_update = false\n");
    let (config, _) = Config::load();
    assert!(!config.features().version_check);
    assert!(!config.features().auto_update);

    let started = std::time::Instant::now();
    let mut updates = thurbox::kernel::updates::Updates::start(config.features());
    assert!(
        started.elapsed() < std::time::Duration::from_millis(200),
        "starting must not wait on anything"
    );
    assert!(updates.available().is_none());
    assert_eq!(updates.poll(), None);
}

#[test]
fn the_update_switches_are_editable_and_marked_as_needing_a_restart() {
    // They are listed only because they now gate something; both take effect at
    // the next launch, since the work runs once at startup.
    let _home = isolate();
    write_settings("");
    let (config, _) = Config::load();
    let mut edited = config.in_force().clone();
    edited.features.version_check = false;
    assert!(
        config.in_force().restart_only_differs(&edited),
        "changing the check is a restart-only change"
    );
    let mut edited = config.in_force().clone();
    edited.features.auto_update = false;
    assert!(config.in_force().restart_only_differs(&edited));
}

#[test]
fn a_raised_column_threshold_leaves_only_the_central_pane() {
    // The arrangement runs before any plugin, so it reads the threshold from what
    // the kernel published rather than from a constant compiled into it.
    let host = host();
    let settings = Settings {
        two_panel_min_cols: 200,
        ..Settings::default()
    };
    publish_with(&host, &settings);
    let placed: Vec<String> = thurbox::kernel::layout::resolve(
        &host.arrangement(150, 40).expect("arrangement"),
        ratatui::layout::Rect::new(0, 0, 150, 40),
    )
    .into_iter()
    .map(|slot| slot.slot)
    .collect();
    assert!(
        !placed.iter().any(|slot| slot == "sessions"),
        "150 columns is below the configured threshold: {placed:?}"
    );
    assert!(placed.iter().any(|slot| slot == "center"), "{placed:?}");

    // And the same width shows both columns at the default threshold.
    publish_with(&host, &Settings::default());
    let placed: Vec<String> = thurbox::kernel::layout::resolve(
        &host.arrangement(150, 40).expect("arrangement"),
        ratatui::layout::Rect::new(0, 0, 150, 40),
    )
    .into_iter()
    .map(|slot| slot.slot)
    .collect();
    assert!(placed.iter().any(|slot| slot == "sessions"), "{placed:?}");
}
