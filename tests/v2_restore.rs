//! The restore list — v1's `Ctrl+U` modal, against the real bundled plugins.
//!
//! What is under test is the division of labour v1 had and v2 has to keep: the
//! session list's `Ctrl+Z` undoes the delete *you* just did, while this lists
//! every row the database still holds. And the half that is easy to lose in a
//! port: a force-deleted row recovers committed work only, so it asks first —
//! here through the shared confirmation rather than v1's bespoke
//! `ConfirmRestore` modal.

use ratatui::backend::TestBackend;
use ratatui::Terminal;
use thurbox::kernel::command::Command;
use thurbox::kernel::host::{KeyPress, LuaHost, Published, RenderContext};
use thurbox::kernel::paint::{render, PlaceholderSurfaces};
use thurbox::kernel::registry::{Registry, Scope};
use thurbox::kernel::snapshot::{DeletedRow, Snapshot};
use thurbox::kernel::theme::Themes;

const PLUGIN: &str = "restore";

fn host() -> LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

fn deleted(id: &str, name: &str, partial: bool) -> DeletedRow {
    DeletedRow {
        id: id.into(),
        name: name.into(),
        agent: "claude".into(),
        // An hour before the snapshot's instant, so the age is a stable "1h ago"
        // rather than something the wall clock decides.
        deleted_at: 3_600_000,
        worktrees: 1,
        partial,
    }
}

fn snapshot() -> Snapshot {
    Snapshot {
        deleted: vec![
            deleted("aaa", "fix-osc52", false),
            deleted("bbb", "add-wsl", false),
        ],
        taken_at_ms: 7_200_000,
        ..Snapshot::default()
    }
}

fn registry(host: &LuaHost) -> Registry {
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    registry
}

fn publish_in(host: &LuaHost, snapshot: &Snapshot) {
    let themes = Themes::load(None);
    let registry = registry(host);
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

fn key(chord: &str) -> KeyPress {
    let mut key = KeyPress::default();
    for part in chord.split('+') {
        match part {
            "ctrl" => key.ctrl = true,
            name => {
                key.name = name.to_string();
                let mut chars = name.chars();
                key.ch = match (chars.next(), chars.next()) {
                    (Some(c), None) => Some(c),
                    _ => None,
                };
            }
        }
    }
    key
}

/// Route a chord the way the binary does while the float has the keyboard:
/// resolve it against the plugin, then fall through to `on_key`.
fn press_in(host: &LuaHost, snapshot: &Snapshot, plugin: &str, chord: &str) {
    publish_in(host, snapshot);
    let index = host
        .index_of(plugin)
        .unwrap_or_else(|| panic!("no {plugin} plugin"));
    let press = key(chord);
    if let Some(binding) = registry(host).resolve(&press, Some(plugin)) {
        let action = binding.action.clone();
        if host.on_action(index, &action).expect("on_action") {
            return;
        }
    }
    host.on_key(index, &press).expect("on_key");
}

fn press(host: &LuaHost, chord: &str) {
    press_in(host, &snapshot(), PLUGIN, chord);
}

/// Render one plugin and return what it drew, plus whether it floated at all —
/// which is what "the modal is open" means here: a closed float draws nothing.
fn rendered(host: &LuaHost, snapshot: &Snapshot, plugin: &str) -> (bool, String) {
    publish_in(host, snapshot);
    let index = host
        .index_of(plugin)
        .unwrap_or_else(|| panic!("no {plugin} plugin"));
    let out = host
        .render(
            index,
            RenderContext {
                width: 60,
                height: 16,
                focused: false,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render");
    (out.float.is_some(), format!("{:?}", out.node))
}

fn tree(host: &LuaHost) -> String {
    rendered(host, &snapshot(), PLUGIN).1
}

#[test]
fn ctrl_u_is_bound_globally_and_deferred_to_the_agent_like_v1() {
    // v1 binds `Ctrl+U` to `OpenRestoreSessions` and puts it in
    // `terminal_passthrough`, because it is readline's kill-line: the list has to
    // be reachable from every other pane without stealing the chord from a
    // focused agent.
    let host = host();
    let registry = registry(&host);
    let binding = registry
        .resolve(&key("ctrl+u"), None)
        .expect("ctrl+u is bound to nothing");
    assert_eq!(binding.action, "restore.open");
    assert_eq!(binding.scope, Scope::Global);
    assert!(binding.passthrough, "ctrl+u is readline's kill-line");
}

#[test]
fn the_list_is_closed_until_the_chord_opens_it() {
    let host = host();
    assert!(!rendered(&host, &snapshot(), PLUGIN).0, "closed by default");
    press(&host, "ctrl+u");
    let (floated, tree) = rendered(&host, &snapshot(), PLUGIN);
    assert!(floated, "the list floats once opened");
    assert!(tree.contains("Restore Deleted Sessions"), "{tree}");
    // And the chord toggles, as the kernel's own modals do.
    press(&host, "ctrl+u");
    assert!(!rendered(&host, &snapshot(), PLUGIN).0, "closed again");
}

#[test]
fn a_row_carries_what_tells_two_deleted_sessions_apart() {
    // v1's row is `name (agent) 3m ago [wt]`. Each piece is the answer to a
    // different question — which agent ran it, whether this is the one deleted a
    // moment ago, and whether there are checkouts to reattach.
    let host = host();
    press(&host, "ctrl+u");
    let tree = tree(&host);
    assert!(tree.contains("fix-osc52 (claude)"), "{tree}");
    assert!(
        tree.contains("1h ago"),
        "measured against the snapshot: {tree}"
    );
    assert!(tree.contains("[wt]"), "{tree}");
}

#[test]
fn enter_restores_the_selected_row_and_closes() {
    let host = host();
    press(&host, "ctrl+u");
    press(&host, "j");
    press(&host, "enter");
    assert_eq!(
        host.drain_commands(),
        vec![Command::Restore {
            session: "bbb".into(),
            best_effort: false,
        }],
        "the row under the cursor, not the first one"
    );
    assert!(
        !rendered(&host, &snapshot(), PLUGIN).0,
        "a list left up would invite a second Enter on a row already coming back"
    );
}

#[test]
fn esc_closes_without_restoring_anything() {
    let host = host();
    press(&host, "ctrl+u");
    press(&host, "esc");
    assert!(host.drain_commands().is_empty());
    assert!(!rendered(&host, &snapshot(), PLUGIN).0);
}

#[test]
fn a_force_deleted_row_is_tagged_and_asks_before_a_best_effort_restore() {
    // v1 gates this behind `ConfirmRestore`: force-delete removed the worktree
    // directory, so only committed work on the branch comes back. The tag is on
    // the row as well as in the question — the difference has to be readable
    // before the choice, not only in the confirmation after it.
    let host = host();
    let snapshot = Snapshot {
        deleted: vec![deleted("ccc", "gone", true)],
        taken_at_ms: 7_200_000,
        ..Snapshot::default()
    };
    press_in(&host, &snapshot, PLUGIN, "ctrl+u");
    let (_, tree) = rendered(&host, &snapshot, PLUGIN);
    assert!(tree.contains("force-deleted"), "{tree}");

    press_in(&host, &snapshot, PLUGIN, "enter");
    assert!(
        host.drain_commands().is_empty(),
        "nothing is restored until the question is answered"
    );
    let (floated, question) = rendered(&host, &snapshot, "confirm");
    assert!(floated, "the shared confirmation is what asks");
    assert!(
        question.contains("Restore force-deleted 'gone'?"),
        "{question}"
    );
    assert!(
        question.contains("uncommitted and untracked changes were lost on delete"),
        "{question}"
    );

    // Answering it is what issues the command, best-effort — the kernel refuses
    // a force-deleted row without that flag.
    press_in(&host, &snapshot, "confirm", "y");
    assert_eq!(
        host.drain_commands(),
        vec![Command::Restore {
            session: "ccc".into(),
            best_effort: true,
        }]
    );
}

#[test]
fn the_float_paints_its_rows_and_its_footer_inside_the_size_it_asked_for() {
    // The tree tests above cannot see a row clipped by the frame, and this modal
    // asks for its own height: one row per deleted session, plus the border and
    // the footer. Painting it is what proves the arithmetic.
    let host = host();
    let snapshot = snapshot();
    press_in(&host, &snapshot, PLUGIN, "ctrl+u");
    publish_in(&host, &snapshot);
    let index = host.index_of(PLUGIN).expect("no restore plugin");
    let out = host
        .render(
            index,
            RenderContext {
                width: 60,
                height: 16,
                focused: false,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render");
    let float = out.float.expect("the list floats");
    let width = float.cols.expect("a fixed width, as v1's modals have");
    let height = float.rows.expect("a height sized to its rows");
    assert_eq!(height, 5, "two rows, the frame and the footer");

    let mut terminal =
        Terminal::new(TestBackend::new(width, height)).expect("terminal for the float");
    terminal
        .draw(|frame| render(frame, frame.area(), &out.node, &PlaceholderSurfaces))
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let painted: Vec<String> = (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect()
        })
        .collect();
    let screen = painted.join("\n");

    assert!(screen.contains("Restore Deleted Sessions"), "{screen}");
    assert!(screen.contains("fix-osc52"), "{screen}");
    assert!(screen.contains("add-wsl"), "{screen}");
    // The pills are the clickable half of the footer; a clipped one is a button
    // whose label lies about what it does.
    assert!(screen.contains("[ Restore ]"), "{screen}");
    assert!(screen.contains("[ Close ]"), "{screen}");
}

#[test]
fn an_empty_list_says_so_and_enter_does_nothing() {
    let host = host();
    let empty = Snapshot::default();
    press_in(&host, &empty, PLUGIN, "ctrl+u");
    let (floated, tree) = rendered(&host, &empty, PLUGIN);
    assert!(
        floated,
        "the answer 'nothing was deleted' is still an answer"
    );
    assert!(tree.contains("No deleted sessions"), "{tree}");
    press_in(&host, &empty, PLUGIN, "enter");
    assert!(host.drain_commands().is_empty());
}
