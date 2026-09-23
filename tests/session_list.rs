//! The session list's contract with the rest of the interface.
//!
//! Three things every other pane depends on and none of which is visible in what
//! the list draws: `Enter` goes to the session you picked; the selection is
//! **steerable** — a pane that writes `store.selected` moves the cursor rather
//! than being overwritten on the next frame; and the selection is a **session**
//! rather than a row, so a snapshot that opens or closes a session above the
//! cursor leaves the highlight where it was. v1 has one `App::select_session` for
//! the first two; here it is a value two plugins share, so the rule has to be
//! asserted. The third is issue #1211, and the assertion that catches it has to
//! be the painted frame: the list published the right value to `store.selected`
//! after a steer and after a create all along, and still moved the bar on an
//! ordinary rebuild.

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;

use thurbox::kernel::command::{Command, InFlight, Phase};
use thurbox::kernel::events::Event;
use thurbox::kernel::host::{KeyPress, LuaHost, Published, RenderContext};
use thurbox::kernel::paint::{render as paint_node, PlaceholderSurfaces};
use thurbox::kernel::registry::{Registry, Value};
use thurbox::kernel::snapshot::{GitState, SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;
use thurbox::session::SessionState;

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
        status: SessionState::Idle,
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
        stopped: false,
        hook_state: None,
        reports_as: None,
        detected_agent: None,
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
    publish_with(host, snapshot, &registry_for(host));
}

/// A `create` the kernel has accepted and no snapshot has answered yet: it
/// names no session, only the repo its row will land in.
fn creating(repo: &str) -> InFlight {
    InFlight {
        id: 1,
        kind: "create",
        session: String::new(),
        subject: Some(repo.into()),
        host: None,
        phase: Phase::Running,
        error: None,
    }
}

/// The registry the kernel would build from what the bundled plugins declare —
/// separate so a test can override a setting before publishing it.
fn registry_for(host: &LuaHost) -> Registry {
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    registry
}

fn publish_with(host: &LuaHost, snapshot: &Snapshot, registry: &Registry) {
    publish_inflight(host, snapshot, registry, &[]);
}

fn publish_inflight(
    host: &LuaHost,
    snapshot: &Snapshot,
    registry: &Registry,
    inflight: &[InFlight],
) {
    let themes = Themes::load(None);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
        snapshot,
        attach_errors: &Default::default(),
        inflight,
        themes: &themes,
        registry,
        diffs: &diffs,
        links: &Default::default(),
        search: None,
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
        selection: None,
        hovered: None,
        printing: &Default::default(),
    })
    .expect("publish");
}

/// Render the list, which is also what publishes the selection.
fn render(host: &LuaHost) {
    render_in(host, &snapshot());
}

fn render_in(host: &LuaHost, snapshot: &Snapshot) {
    render_with(host, snapshot, &registry_for(host));
}

fn render_with(host: &LuaHost, snapshot: &Snapshot, registry: &Registry) {
    publish_with(host, snapshot, registry);
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
    press_inflight(host, snapshot, &[], chord);
}

fn press_inflight(host: &LuaHost, snapshot: &Snapshot, inflight: &[InFlight], chord: &str) {
    publish_inflight(host, snapshot, &registry_for(host), inflight);
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

// --- the selection is a session, not the row it happens to sit on -----------

/// Three rows in one repo, so the rendered order is the snapshot's order and a
/// row prepended to that lands ABOVE the cursor.
fn three() -> Snapshot {
    Snapshot {
        sessions: vec![
            row("aaa", "first"),
            row("bbb", "second"),
            row("ccc", "third"),
        ],
        ..Snapshot::default()
    }
}

/// The same list with one more session above it — a create this interface did
/// not perform, so no `session.post_create`, no follow, nothing but a snapshot
/// carrying one more row.
fn and_one_above(world: &Snapshot) -> Snapshot {
    let mut sessions = vec![row("000", "newcomer")];
    sessions.extend(world.sessions.iter().cloned());
    Snapshot {
        sessions,
        ..world.clone()
    }
}

/// The same list with one session gone — a delete this interface did not
/// perform either.
fn without(world: &Snapshot, id: &str) -> Snapshot {
    Snapshot {
        sessions: world
            .sessions
            .iter()
            .filter(|session| session.id != id)
            .cloned()
            .collect(),
        ..world.clone()
    }
}

/// The painted frame is the only evidence that catches this file's selection
/// bug: the list published the right `store.selected` after a steer and after a
/// create all along, so a test reading that value alone stays green whether or
/// not a plain rebuild keeps the cursor where it was.
fn paint(host: &LuaHost, plugin: &str, width: u16, height: u16) -> Buffer {
    let node = host
        .render(
            host.index_of(plugin)
                .unwrap_or_else(|| panic!("no plugin named {plugin}")),
            RenderContext {
                width,
                height,
                focused: true,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .unwrap_or_else(|e| panic!("{plugin} should render: {e}"))
        .node;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| paint_node(frame, frame.area(), &node, &PlaceholderSurfaces))
        .expect("draw");
    terminal.backend().buffer().clone()
}

/// The text of the row wearing the selection bar. The selection is a full-width
/// background rather than a marker glyph, so the row carrying one is the body row
/// whose first *inner* cell stops sharing the background of the border cell
/// beside it. Read as a difference rather than against a literal colour, so the
/// assertion holds under any preset; the border rows are skipped because a
/// focused pane's title wears a highlight of its own.
fn selection_bar(buffer: &Buffer) -> Option<String> {
    (1..buffer.area.height.saturating_sub(1))
        .find(|&y| buffer[(1, y)].bg != buffer[(0, y)].bg)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
}

/// A pane's top border, which is where a title is drawn.
fn title(buffer: &Buffer) -> String {
    (0..buffer.area.width)
        .map(|x| buffer[(x, 0)].symbol())
        .collect()
}

/// The three readings of the selection must name the same session: the value the
/// list published, the bar it painted, and the title of the agent pane that
/// draws whatever was published. One write moved all three, so one assertion
/// reads all three — and the list is painted first, because painting it is what
/// publishes.
#[track_caller]
fn assert_every_reading_names(host: &LuaHost, id: &str, name: &str) {
    let highlighted = selection_bar(&paint(host, PLUGIN, 40, 12));
    let title = title(&paint(host, "agent", 60, 6));
    assert_eq!(
        host.shared_string("selected").as_deref(),
        Some(id),
        "the list published a different session"
    );
    assert!(
        highlighted.as_deref().is_some_and(|row| row.contains(name)),
        "the painted bar is on a different session: {highlighted:?}"
    );
    assert!(
        title.contains(&format!("{name} (claude)")),
        "the agent pane is on a different session: {title:?}"
    );
}

/// Put the cursor on the middle row and leave it there, with nothing chasing
/// it: no follow, no foreign `store.selected`, no `session.post_create`. This is
/// the state the bug needs — a plain cursor over a plain list.
fn on_the_middle_row(host: &LuaHost, world: &Snapshot) {
    render_in(host, world);
    press_in(host, world, "j");
    render_in(host, world);
    assert_eq!(
        host.shared_string("selected").as_deref(),
        Some("bbb"),
        "the middle row is where this starts"
    );
    host.drain_commands();
}

#[test]
fn a_session_opening_above_the_cursor_moves_neither_the_bar_nor_the_agent_pane() {
    // The whole of issue #1211: the cursor was an index restored from `state`,
    // so a row appearing above it slid a different session under the highlight —
    // and under the agent pane, which draws whatever the list published, while
    // the keyboard stayed where it was.
    let host = host();
    let world = three();
    on_the_middle_row(&host, &world);

    publish_in(&host, &and_one_above(&world));
    assert_every_reading_names(&host, "bbb", "second");
    assert!(
        host.drain_commands().is_empty(),
        "and the keyboard was not moved to make the three of them agree"
    );
}

#[test]
fn a_session_closing_above_the_cursor_moves_neither_the_bar_nor_the_agent_pane() {
    // The other half of the same write: a row *leaving* above the cursor shifts
    // every later row up by one, so an index-based cursor lands one row further
    // down the list.
    let host = host();
    let world = three();
    on_the_middle_row(&host, &world);

    publish_in(&host, &without(&world, "aaa"));
    assert_every_reading_names(&host, "bbb", "second");
}

#[test]
fn the_selected_session_going_away_hands_the_selection_to_the_row_below_it() {
    // The branch that decides what "follow the session" means when there is no
    // session left to follow: the row that has taken its place. Never the top,
    // which is a second theft, and never nothing, which would blank the agent
    // pane over a session the operator never closed.
    let host = host();
    let world = three();
    on_the_middle_row(&host, &world);

    publish_in(&host, &without(&world, "bbb"));
    assert_every_reading_names(&host, "ccc", "third");
}

#[test]
fn the_last_session_going_away_clamps_to_the_new_last_row() {
    // The same rule at the end of the list, where there is no row below: the
    // list shortened under the cursor, so the cursor lands on its last row.
    let host = host();
    let world = three();
    render_in(&host, &world);
    press_in(&host, &world, "j");
    press_in(&host, &world, "j");
    render_in(&host, &world);
    assert_eq!(host.shared_string("selected").as_deref(), Some("ccc"));

    let shrunk = without(&world, "ccc");
    render_in(&host, &shrunk);
    assert_eq!(
        host.shared_string("selected").as_deref(),
        Some("bbb"),
        "the list's last row, not its first"
    );
}

#[test]
fn the_last_session_of_all_going_away_publishes_nothing_rather_than_a_dead_id() {
    // The end of the removal branch. Re-deriving the row from a session id says
    // nothing about a list with no sessions left in it, so what is pinned here is
    // that the list publishes *nothing* and the agent pane falls back to its own
    // empty frame — rather than the last id it saw being held on out of caution,
    // which would leave that pane titled after a session that is gone.
    let host = host();
    let world = three();
    on_the_middle_row(&host, &world);

    let empty = Snapshot::default();
    render_in(&host, &empty);
    assert_eq!(
        host.shared_string("selected").as_deref(),
        None,
        "nothing is selected when there is nothing to select"
    );
    assert!(
        title(&paint(&host, "agent", 60, 6)).contains("No Session"),
        "the agent pane kept a session the list no longer has"
    );
}

/// What the kernel hands the list when a create or a fork this interface asked
/// for has landed, with the row already resolved in the snapshot.
fn post_create(id: &str, name: &str) -> Event {
    Event::new("session.post_create")
        .with("session", Some(id))
        .with("name", Some(name))
        .with("agent", Some("claude"))
}

/// The setting on, in the registry the plugin reads its value back from.
fn following_new_sessions(host: &LuaHost) -> Registry {
    let mut registry = registry_for(host);
    registry
        .set_setting(PLUGIN, "focus_new_session", Some(Value::Bool(true)))
        .expect("set focus_new_session");
    registry
}

#[test]
fn a_session_this_interface_created_moves_nothing_by_default() {
    // The default this interface has always had: the row appears and waits to
    // be picked. Asserted rather than assumed, because the machinery that can
    // move the cursor is now loaded either way — only the setting is off.
    let host = host();
    render(&host);

    let failures = host.dispatch_event(&post_create("bbb", "second"));
    assert!(failures.is_empty(), "{failures:?}");
    render(&host);
    assert_eq!(
        host.shared_string("selected").as_deref(),
        Some("aaa"),
        "creating a session must not move the selection unless asked"
    );
    assert!(
        host.drain_commands().is_empty(),
        "and must not take the keyboard either"
    );
}

#[test]
fn with_the_setting_on_a_created_session_is_selected_and_opened() {
    // The whole of what the setting buys: the two halves `Enter` performs, for
    // a row the user did not have to find.
    let host = host();
    let registry = following_new_sessions(&host);
    render_with(&host, &snapshot(), &registry);

    // Nothing is drained first: a render that started issuing commands should
    // fail this assertion rather than hide behind a reset buffer.
    host.dispatch_event(&post_create("bbb", "second"));
    assert_eq!(
        host.drain_commands(),
        vec![Command::Focus {
            plugin: "agent".into(),
            toggle: false,
        }],
        "the agent pane is what shows a session, so opening one focuses it"
    );
    render_with(&host, &snapshot(), &registry);
    assert_eq!(
        host.shared_string("selected").as_deref(),
        Some("bbb"),
        "the cursor followed the session that was just created"
    );
}

#[test]
fn a_pending_jump_loses_to_the_users_own_cursor_move() {
    // The follow is sticky, not a lock: moving the cursor yourself after the
    // jump is a choice made later, so it stands.
    let host = host();
    let registry = following_new_sessions(&host);
    render_with(&host, &snapshot(), &registry);

    host.dispatch_event(&post_create("bbb", "second"));
    render_with(&host, &snapshot(), &registry);
    press(&host, "k");
    render_with(&host, &snapshot(), &registry);
    assert_eq!(
        host.shared_string("selected").as_deref(),
        Some("aaa"),
        "the jump must not pull the cursor back"
    );
}

#[test]
fn only_a_create_this_interface_made_is_subscribed_to() {
    // `session.created` fires for every row that appears, whoever made it —
    // subscribing to it would let a `thurbox-cli session create`, an automation
    // or a second instance take the keyboard out from under the user.
    let host = host();
    let index = host.index_of(PLUGIN).expect("no sessions plugin");
    let events = &host.plugins[index].events;
    assert!(
        events.iter().any(|name| name == "session.post_create"),
        "the list must hear about the creates this interface performed: {events:?}"
    );
    assert!(
        !events.iter().any(|name| name == "session.created"),
        "a session created elsewhere must not move this cursor: {events:?}"
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
        merged: None,
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

#[test]
fn force_deleting_a_merged_branch_does_not_ask() {
    // The regression this fixes. A squash-merged branch keeps its commits
    // forever — they are not ancestors of the default branch, so the ahead
    // count never falls back to zero once the remote branch is gone. Counting
    // them as "not pushed anywhere else" asks about work that is already on
    // `origin/main`, which is every finished session, which is how the answer
    // to the question stops being a decision.
    let host = host();
    let mut snapshot = snapshot();
    snapshot.sessions[0].git = Some(GitState {
        ahead: 3,
        merged: Some(true),
        ..clean()
    });
    snapshot.sessions[0].worktree_count = 1;
    render_in(&host, &snapshot);
    press_in(&host, &snapshot, "D");

    assert_eq!(
        host.drain_commands(),
        vec![Command::Delete {
            session: "aaa".into(),
            force: true,
        }],
        "the work is on the default branch: there is nothing to lose"
    );
}

#[test]
fn force_deleting_an_unmerged_branch_still_asks() {
    // The other half: `merged` only ever silences the question, never asks one
    // of its own, and an unproven branch (`false`, or a check that could not
    // run) keeps the commits it is ahead by.
    for merged in [Some(false), None] {
        let host = host();
        let mut snapshot = snapshot();
        snapshot.sessions[0].git = Some(GitState {
            ahead: 3,
            merged,
            ..clean()
        });
        snapshot.sessions[0].worktree_count = 1;
        render_in(&host, &snapshot);
        press_in(&host, &snapshot, "D");

        assert!(
            host.drain_commands().is_empty(),
            "merged={merged:?} must not delete unasked"
        );
        let tree = confirm_tree(&host, &snapshot);
        assert!(
            tree.contains("3 commit(s) not pushed anywhere else"),
            "merged={merged:?}: {tree}"
        );
    }
}

#[test]
fn shift_s_sorts_each_group_by_name() {
    // The baseline the fix below must not move: with nothing in flight, the
    // group's sessions are persisted in name order, case-insensitively.
    let host = host();
    let snapshot = Snapshot {
        sessions: vec![row("aaa", "Zulu"), row("bbb", "alpha")],
        ..Snapshot::default()
    };

    render_in(&host, &snapshot);
    press_in(&host, &snapshot, "S");

    assert_eq!(
        host.drain_commands(),
        vec![Command::Order {
            list: vec!["bbb".into(), "aaa".into()],
        }]
    );
}

/// Issue #1200: `Shift+S` took the pane down whenever a creation was in flight
/// in a group that already held a session. A placeholder is its own root block
/// with no `session` behind it, and a group of two blocks is the first one that
/// invokes the comparator at all — which is why one session was never enough.
#[test]
fn sorting_with_a_creation_in_flight_does_not_take_the_pane_down() {
    let host = host();
    let snapshot = snapshot();
    let inflight = [creating("thurbox")];

    render_in(&host, &snapshot);
    press_inflight(&host, &snapshot, &inflight, "S");

    // The sort still happens, over the sessions that have one: a placeholder
    // carries no session id, so there is nothing of it to persist an order for.
    assert_eq!(
        host.drain_commands(),
        vec![Command::Order {
            list: vec!["aaa".into(), "bbb".into()],
        }]
    );
}

/// `lib.order`'s own answer, read off a pane that calls it.
///
/// The sort's output reaches the screen through nothing: the list is rebuilt
/// from `session_model.build` every frame, and `persist_order` keeps only the
/// rows carrying a session id. So where it puts a block is asserted here, on
/// the function, rather than inferred from a pane that would draw the same
/// list either way.
fn sorted_by_order_lua(items: &str) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");

    // `require` reads the interface directory, so the module under test is the
    // repository's own file, copied in beside the probe rather than restated.
    let lib = dir.path().join("lib");
    std::fs::create_dir_all(&lib).expect("mkdir");
    let checkout = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    std::fs::copy(checkout.join("lib/order.lua"), lib.join("order.lua")).expect("copy lib.order");

    std::fs::write(
        plugins.join("10_probe.lua"),
        format!(
            r#"
local order = require("lib.order")

return {{
  name = "probe",
  slot = "center",
  render = function()
    local names = {{}}
    for _, item in ipairs(order.sorted_within_groups({items})) do
      names[#names + 1] = item.session and item.session.name or "<nameless>"
    end
    return {{ type = "text", text = table.concat(names, ",") }}
  end,
}}
"#
        ),
    )
    .expect("write the probe");

    let host = LuaHost::new(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);
    let index = host.index_of("probe").expect("no probe plugin");
    let rendered = host
        .render(
            index,
            RenderContext {
                width: 40,
                height: 4,
                focused: false,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render the probe");
    format!("{:?}", rendered.node)
}

#[test]
fn the_sort_puts_a_nameless_block_at_its_groups_end() {
    // The placeholder is deliberately FIRST in the input. `session_model.build`
    // never emits one there — which is the point: the contract has to hold for
    // the function, not for the one arrangement its caller happens to pass, or
    // the next caller inherits a comparator that indexes a session that is not
    // there.
    let items = r#"{
      { session = { name = "zulu" }, depth = 0, header = "thurbox", target = "z" },
      { command = {}, depth = 0, target = false },
      { session = { name = "alpha" }, depth = 0, target = "a" },
    }"#;

    let drawn = sorted_by_order_lua(items);
    assert!(
        drawn.contains("alpha,zulu,<nameless>"),
        "the named blocks sort and the nameless one lands last:\n{drawn}"
    );
}
