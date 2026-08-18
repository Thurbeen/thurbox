//! The session list's contract with the rest of the interface.
//!
//! Two things every other pane depends on and neither of which is visible in what
//! the list draws: `Enter` goes to the session you picked, and the selection is
//! **steerable** — a pane that writes `store.selected` moves the cursor rather
//! than being overwritten on the next frame. v1 has one `App::select_session` for
//! that; here it is a value two plugins share, so the rule has to be asserted.

use thurbox::kernel::command::Command;
use thurbox::kernel::host::{KeyPress, LuaHost, Published, RenderContext};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;

const PLUGIN: &str = "sessions";

fn host() -> LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

fn row(id: &str, name: &str) -> SessionRow {
    SessionRow {
        id: id.into(),
        name: name.into(),
        agent: "claude".into(),
        status: "idle".into(),
        cwd: Some(std::path::PathBuf::from("/src/thurbox")),
        repo: Some("thurbox".into()),
        repos: vec!["thurbox".into()],
        branch: Some("main".into()),
        base_branch: None,
        backend: "local-tmux".into(),
        backend_id: Some("%1".into()),
        remote_host: None,
        agent_session_id: None,
        parent_id: None,
        display_order: None,
        worktree_count: 0,
        git: None,
        hook_state: None,
        shell_backend_id: None,
        member_dirs: Vec::new(),
    }
}

fn snapshot() -> Snapshot {
    Snapshot {
        sessions: vec![row("aaa", "first"), row("bbb", "second")],
        ..Snapshot::default()
    }
}

fn publish(host: &LuaHost) {
    let themes = Themes::load(None);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        snapshot: &snapshot(),
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
        settings: &Default::default(),
        repos: &repos,
        wants: &Default::default(),
        focus: None,
        hovered: None,
    })
    .expect("publish");
}

/// Render the list, which is also what publishes the selection.
fn render(host: &LuaHost) {
    publish(host);
    let index = host.index_of(PLUGIN).expect("no sessions plugin");
    host.render(
        index,
        RenderContext {
            width: 30,
            height: 12,
            focused: true,
            elapsed: 0.0,
            frame: 0,
        },
    )
    .expect("render");
}

fn press(host: &LuaHost, chord: &str) {
    publish(host);
    let index = host.index_of(PLUGIN).expect("no sessions plugin");
    let mut key = KeyPress {
        name: chord.to_string(),
        ..KeyPress::default()
    };
    if chord.chars().count() == 1 {
        key.ch = chord.chars().next();
    }
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    if let Some(binding) = registry.resolve(&key, Some(PLUGIN)) {
        let action = binding.action.clone();
        if host.on_action(index, &action).expect("on_action") {
            return;
        }
    }
    host.on_key(index, &key).expect("on_key");
}

#[test]
fn enter_opens_the_selected_session() {
    // v1's Enter on a row moves focus to the terminal; the agent pane is what
    // shows a session here, so opening is a focus change.
    let host = host();
    render(&host);
    press(&host, "enter");
    assert_eq!(
        host.drain_commands(),
        vec![Command::Focus {
            plugin: "agent".into(),
            toggle: false,
        }]
    );
}

#[test]
fn enter_is_declared_so_help_lists_it_and_it_can_be_rebound() {
    let host = host();
    let index = host.index_of(PLUGIN).expect("no sessions plugin");
    assert!(
        host.plugins[index]
            .bindings
            .iter()
            .any(|binding| binding.chord == "enter"),
        "a key that only exists inside on_key is invisible to help and unrebindable"
    );
}

#[test]
fn the_list_publishes_the_session_under_its_cursor() {
    let host = host();
    render(&host);
    assert_eq!(host.shared_string("selected").as_deref(), Some("aaa"));
    press(&host, "j");
    render(&host);
    assert_eq!(host.shared_string("selected").as_deref(), Some("bbb"));
}

#[test]
fn another_pane_can_steer_the_selection() {
    // The gap this closes: the list republished its own cursor every frame, so a
    // pane that wrote `store.selected` — a search result, a task opening its
    // session — was undone a frame later and the jump silently did nothing.
    let host = host();
    render(&host);
    assert_eq!(host.shared_string("selected").as_deref(), Some("aaa"));

    host.set_shared_string("selected", "bbb");
    render(&host);
    assert_eq!(
        host.shared_string("selected").as_deref(),
        Some("bbb"),
        "the write survived the next render"
    );

    // And the cursor really moved with it, rather than the value merely sticking:
    // stepping on lands past the steered row, not past the old one.
    press(&host, "k");
    render(&host);
    assert_eq!(host.shared_string("selected").as_deref(), Some("aaa"));
}

#[test]
fn a_session_that_went_away_does_not_freeze_the_selection() {
    // Steering at an id the list cannot show must not strand the cursor: the list
    // keeps its own, and the request is simply not honoured.
    let host = host();
    render(&host);
    host.set_shared_string("selected", "gone");
    render(&host);
    assert_eq!(
        host.shared_string("selected").as_deref(),
        Some("aaa"),
        "the cursor stayed on a row that exists"
    );
}
