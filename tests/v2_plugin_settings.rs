//! Settings a plugin declares are real knobs, not decoration.
//!
//! The contribution API (`Registry::settings`) existed from the start and no
//! bundled plugin used it, so the settings modal rendered an empty pane. That
//! is the failure mode these guard against: a declaration that nothing reads is
//! worse than none, because it puts a row in front of the user that does not do
//! anything when they change it.

use ratatui::backend::TestBackend;
use ratatui::Terminal;

use thurbox::kernel::host::{LuaHost, Published, RenderContext};
use thurbox::kernel::registry::{Registry, Value};
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;

fn host() -> LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

fn row(name: &str, repo: &str) -> SessionRow {
    SessionRow {
        id: format!("{name}-0000"),
        name: name.into(),
        agent: "claude".into(),
        status: "idle".into(),
        cwd: None,
        repo: Some(repo.into()),
        repos: vec![repo.into()],
        branch: Some("feat/x".into()),
        backend: "local-tmux".into(),
        backend_id: Some("%1".into()),
        remote_host: None,
        agent_session_id: None,
        parent_id: None,
        display_order: None,
        worktree_count: 1,
        git: None,
        hook_state: None,
        shell_backend_id: None,
        member_dirs: Vec::new(),
    }
}

/// Render the session list with `registry` in force.
fn session_list(host: &LuaHost, registry: &Registry) -> String {
    let snapshot = Snapshot {
        sessions: vec![row("one", "thurbox"), row("two", "website")],
        ..Snapshot::default()
    };
    let themes = Themes::load(None);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        snapshot: &snapshot,
        attach_errors: &Default::default(),
        inflight: &[],
        themes: &themes,
        registry,
        diffs: &diffs,
        links: &Default::default(),
        content: &Default::default(),
        meta: &Default::default(),
        metrics: &Default::default(),
        status_rows: 0,
        can_open: true,
        inventory: &[],
        ui_dir: "ui",
        settings: &Default::default(),
        repos: &repos,
        wants: &Default::default(),
        focus: Some("sessions"),
        hovered: None,
    })
    .expect("publish");

    let index = host
        .plugins
        .iter()
        .position(|p| p.name == "sessions")
        .expect("sessions plugin");
    let node = host
        .render(
            index,
            RenderContext {
                width: 40,
                height: 12,
                focused: true,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render")
        .node;

    let mut terminal = Terminal::new(TestBackend::new(40, 12)).expect("terminal");
    terminal
        .draw(|frame| {
            thurbox::kernel::paint::render(
                frame,
                frame.area(),
                &node,
                &thurbox::kernel::paint::PlaceholderSurfaces,
            )
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    (0..12)
        .map(|y| {
            (0..40)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn registry_for(host: &LuaHost) -> Registry {
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    registry
}

#[test]
fn the_bundled_plugins_declare_settings_at_all() {
    // The regression this whole file exists for: with none declared, the
    // settings modal has nothing to render and the extension point is unproven.
    let host = host();
    let registry = registry_for(&host);
    assert!(
        !registry.settings().is_empty(),
        "no bundled plugin declares a setting, so the settings modal is empty"
    );
}

#[test]
fn a_declared_setting_actually_changes_what_is_drawn() {
    // Declaring is half of it. Flipping `sessions.group_by_repo` must remove the
    // repo headers, or the row in the modal is a lie.
    let host = host();
    let mut registry = registry_for(&host);

    let grouped = session_list(&host, &registry);
    assert!(
        grouped.contains("thurbox") && grouped.contains("website"),
        "expected repo headers by default:\n{grouped}"
    );

    registry
        .set_setting("sessions", "group_by_repo", Some(Value::Bool(false)))
        .expect("set");
    let flat = session_list(&host, &registry);
    assert!(
        !flat.contains("thurbox") && !flat.contains("website"),
        "headers should be gone once grouping is off:\n{flat}"
    );
    // The sessions themselves are untouched — only the header line goes.
    assert!(
        flat.contains("one") && flat.contains("two"),
        "sessions must survive:\n{flat}"
    );
}

#[test]
fn a_setting_reverts_to_its_default_when_cleared() {
    let host = host();
    let mut registry = registry_for(&host);
    registry
        .set_setting("sessions", "group_by_repo", Some(Value::Bool(false)))
        .expect("set");
    registry
        .set_setting("sessions", "group_by_repo", None)
        .expect("clear");
    let restored = session_list(&host, &registry);
    assert!(
        restored.contains("thurbox"),
        "clearing the override should restore the declared default:\n{restored}"
    );
}
