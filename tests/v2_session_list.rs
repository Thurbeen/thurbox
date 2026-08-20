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
use thurbox::kernel::snapshot::{GitState, SessionRow, Snapshot};
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

fn publish_in(host: &LuaHost, snapshot: &Snapshot) {
    let themes = Themes::load(None);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        snapshot,
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
    render_in(host, &snapshot());
}

fn render_in(host: &LuaHost, snapshot: &Snapshot) {
    publish_in(host, snapshot);
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
    press_in(host, &snapshot(), chord);
}

fn press_in(host: &LuaHost, snapshot: &Snapshot, chord: &str) {
    publish_in(host, snapshot);
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

/// A worktree with nothing in it that a delete could not put back.
fn clean() -> GitState {
    GitState {
        files_changed: 0,
        insertions: 0,
        deletions: 0,
        untracked: 0,
        dirty: false,
        ahead: 0,
        behind: 0,
    }
}

/// Render the confirmation float and return what it drew, so a test can ask
/// whether a question was put at all — and what it itemised.
fn confirm_tree(host: &LuaHost, snapshot: &Snapshot) -> String {
    publish_in(host, snapshot);
    let index = host.index_of("confirm").expect("no confirm plugin");
    let rendered = host
        .render(
            index,
            RenderContext {
                width: 60,
                height: 12,
                focused: false,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render the confirmation");
    format!("{:?}", rendered.node)
}

#[test]
fn force_deleting_a_clean_session_does_not_ask() {
    // v1's rule, in `App::delete_active_session`: `assess_delete_risk` returning
    // `Some(risk)` opened `ConfirmDelete`, `None` deleted on the keystroke. v2
    // asked every time, which is how the answer to a question stops being a
    // decision.
    let host = host();
    let mut snapshot = snapshot();
    snapshot.sessions[0].git = Some(clean());
    render_in(&host, &snapshot);
    press_in(&host, &snapshot, "D");

    assert_eq!(
        host.drain_commands(),
        vec![Command::Delete {
            session: "aaa".into(),
            force: true,
        }],
        "a known-clean session is torn down on the keystroke"
    );
    assert!(
        !confirm_tree(&host, &snapshot).contains("Confirm"),
        "and no question was put: a worktree directory alone is not work at risk"
    );
}

#[test]
fn force_deleting_a_session_with_work_asks_first_and_says_what_is_lost() {
    let host = host();
    let mut snapshot = snapshot();
    snapshot.sessions[0].git = Some(GitState {
        files_changed: 2,
        untracked: 1,
        dirty: true,
        ahead: 3,
        ..clean()
    });
    snapshot.sessions[0].worktree_count = 1;
    render_in(&host, &snapshot);
    press_in(&host, &snapshot, "D");

    assert!(
        host.drain_commands().is_empty(),
        "nothing is torn down until the question is answered"
    );
    let tree = confirm_tree(&host, &snapshot);
    assert!(tree.contains("and its worktree?"), "the question: {tree}");
    assert!(
        tree.contains("3 uncommitted or untracked file(s)"),
        "{tree}"
    );
    assert!(
        tree.contains("3 commit(s) not pushed anywhere else"),
        "{tree}"
    );
    assert!(
        tree.contains("its worktree directory"),
        "listed as what else goes, once a question is owed: {tree}"
    );
}

#[test]
fn a_clean_primary_does_not_speak_for_the_other_worktrees() {
    // The snapshot stats one directory per session, so on a multi-worktree
    // session a clean answer covers the primary and nothing else. v1 assessed
    // every worktree it was about to remove; not being able to is unknown.
    let host = host();
    let mut snapshot = snapshot();
    snapshot.sessions[0].git = Some(clean());
    snapshot.sessions[0].worktree_count = 2;
    render_in(&host, &snapshot);
    press_in(&host, &snapshot, "D");

    assert!(
        host.drain_commands().is_empty(),
        "a checkout nobody read must not be torn down unasked"
    );
    let tree = confirm_tree(&host, &snapshot);
    assert!(
        tree.contains("its other worktrees could not be read"),
        "{tree}"
    );
    assert!(
        tree.contains("its 2 worktree directories"),
        "and what goes is counted, not assumed singular: {tree}"
    );
}

#[test]
fn a_state_that_could_not_be_read_asks_rather_than_assume_clean() {
    // `git` is nil for a stat that has not run, a directory that is not a
    // worktree, and a host that could not be reached. v1 folded all three into
    // `DeleteRisk::unknown()` and confirmed.
    let host = host();
    let snapshot = snapshot();
    assert!(snapshot.sessions[0].git.is_none());
    render_in(&host, &snapshot);
    press_in(&host, &snapshot, "D");

    assert!(host.drain_commands().is_empty(), "it must not delete blind");
    assert!(
        confirm_tree(&host, &snapshot).contains("its state could not be read"),
        "and it says why it is asking"
    );
}
