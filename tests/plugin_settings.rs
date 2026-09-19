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
        status: thurbox::session::SessionState::Idle,
        cwd: None,
        repo: Some(repo.into()),
        repos: vec![repo.into()],
        branch: Some("feat/x".into()),
        base_branch: None,
        backend: "local-tmux".into(),
        backend_id: Some("%1".into()),
        remote_host: None,
        agent_session_id: None,
        parent_id: None,
        display_order: None,
        worktree_count: 1,
        git: None,
        stopped: false,
        hook_state: None,
        reports_as: None,
        detected_agent: None,
        shell_backend_id: None,
        member_dirs: Vec::new(),
    }
}

/// `row`, moved by hand to `position` in the manual order.
fn ordered(mut row: SessionRow, position: i64) -> SessionRow {
    row.display_order = Some(position);
    row
}

/// `row`, running on another machine. Both fields, as a real remote row
/// carries them: the backend is how the session was started and `remote_host`
/// is the bare name the session list is published and grouped by.
fn on_host(mut row: SessionRow, machine: &str) -> SessionRow {
    row.backend = format!("ssh:{machine}");
    row.remote_host = Some(machine.into());
    row
}

/// Where each of `names` lands in the rendered list, top to bottom.
fn rendered_order<'a>(drawn: &str, names: &[&'a str]) -> Vec<&'a str> {
    let mut found: Vec<(usize, &str)> = names
        .iter()
        .map(|name| {
            let at = drawn
                .find(name)
                .unwrap_or_else(|| panic!("{name} is not on screen:\n{drawn}"));
            (at, *name)
        })
        .collect();
    found.sort_by_key(|(at, _)| *at);
    found.into_iter().map(|(_, name)| name).collect()
}

/// Render the session list with `registry` in force.
fn session_list(host: &LuaHost, registry: &Registry) -> String {
    session_list_of(
        host,
        registry,
        vec![row("one", "thurbox"), row("two", "website")],
    )
}

/// The same, over a session set the caller chose.
fn session_list_of(host: &LuaHost, registry: &Registry, sessions: Vec<SessionRow>) -> String {
    let snapshot = Snapshot {
        sessions,
        ..Snapshot::default()
    };
    let themes = Themes::load(None);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
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
        selection: None,
        hovered: None,
        printing: &Default::default(),
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

#[test]
fn grouping_off_means_ungrouped_and_not_merely_unlabelled() {
    // The knob suppressed the header LINE and kept the repo clustering, so a
    // manual order that interleaved two repos was silently re-clustered on the
    // next build: `Shift+J` past a repo boundary was accepted, persisted, and
    // undone a frame later, with the headers that would have explained it
    // turned off. Off has to mean one flat list ordered by the manual order.
    let host = host();
    let mut registry = registry_for(&host);
    let sessions = || {
        vec![
            ordered(row("alpha", "thurbox"), 0),
            ordered(row("bravo", "website"), 1),
            ordered(row("charlie", "thurbox"), 2),
        ]
    };

    let grouped = session_list_of(&host, &registry, sessions());
    assert_eq!(
        rendered_order(&grouped, &["alpha", "bravo", "charlie"]),
        vec!["alpha", "charlie", "bravo"],
        "grouped, a repo's sessions still cluster:\n{grouped}"
    );

    registry
        .set_setting("sessions", "group_by_repo", Some(Value::Bool(false)))
        .expect("set");
    let flat = session_list_of(&host, &registry, sessions());
    assert_eq!(
        rendered_order(&flat, &["alpha", "bravo", "charlie"]),
        vec!["alpha", "bravo", "charlie"],
        "ungrouped, the manual order is the whole order:\n{flat}"
    );
}

#[test]
fn host_grouping_is_the_operators_choice() {
    // The host axis shipped derived: a list spanning machines grouped by them
    // and there was no way to say otherwise. The repo axis has been a knob
    // since it shipped, and this is the row beside it.
    let host = host();
    let mut registry = registry_for(&host);
    // One repo on two machines, so the repo axis alone cannot produce the
    // second header and only the host axis can.
    let across_machines = || {
        vec![
            row("one", "thurbox"),
            on_host(row("two", "thurbox"), "buildbox"),
        ]
    };

    // Two headers, because the two machines split one repo group in two. The
    // repo name is only ever on a header here -- the sessions are `one` and
    // `two` -- so counting it counts the groups.
    let by_machine = session_list_of(&host, &registry, across_machines());
    assert!(
        by_machine.contains("buildbox"),
        "the default is what the operator has today:\n{by_machine}"
    );
    assert_eq!(
        by_machine.matches("thurbox").count(),
        2,
        "one per machine:\n{by_machine}"
    );

    registry
        .set_setting("sessions", "group_by_host", Some(Value::Bool(false)))
        .expect("set");
    let merged = session_list_of(&host, &registry, across_machines());
    assert!(
        !merged.contains("buildbox") && !merged.contains("local"),
        "off, no header names a machine:\n{merged}"
    );
    assert_eq!(
        merged.matches("thurbox").count(),
        1,
        "the repo axis is its own knob, and its one group is now whole:\n{merged}"
    );
    assert_eq!(
        rendered_order(&merged, &["one", "two"]),
        vec!["one", "two"],
        "and the two machines' sessions are one group:\n{merged}"
    );
}

#[test]
fn one_machine_draws_no_host_header_whatever_the_setting_says() {
    // Grouping into a single group is noise, which is the axis's own gate and
    // not the knob's: the knob decides whether the gate is asked at all. On,
    // off, and never touched must therefore all render the same list here.
    let host = host();
    let mut registry = registry_for(&host);
    let one_machine = || vec![row("one", "thurbox"), row("two", "website")];

    let untouched = session_list_of(&host, &registry, one_machine());
    for value in [Value::Bool(true), Value::Bool(false)] {
        registry
            .set_setting("sessions", "group_by_host", Some(value.clone()))
            .expect("set");
        let drawn = session_list_of(&host, &registry, one_machine());
        assert!(
            !drawn.contains("local"),
            "one machine needs no header naming it, with the knob {value:?}:\n{drawn}"
        );
        assert_eq!(
            drawn, untouched,
            "and the whole list is what it was before the knob existed"
        );
    }
}
