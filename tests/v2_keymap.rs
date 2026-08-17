//! v2's keymap against v1's, chord for chord.
//!
//! v1's defaults are data — `Action::default_chords_for` — so this does not
//! restate them in a list of its own where it can be avoided: it walks v1's own
//! table and asks the v2 registry what each chord does. A key v1 hands to the
//! agent in a focused terminal (`Action::terminal_passthrough`) has to be
//! handed over here too, or the agent's line editing quietly stops working the
//! day the pane it belongs to lands.
//!
//! The bundled plugins are loaded exactly as the binary loads them, so this
//! fails if a plugin drops a chord as readily as if the kernel does.

use std::path::PathBuf;

use ratatui::layout::Rect;

use thurbox::kernel::host::{KeyPress, LuaHost, Published, RenderContext};
use thurbox::kernel::layout::resolve;
use thurbox::kernel::registry::{is_ctrl_letter_chord, normalise_chord, Registry, Scope, RESERVED};
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;
use thurbox::session::keybindings::Action;

fn host() -> LuaHost {
    let host = LuaHost::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui"));
    assert!(
        host.error.is_none(),
        "the bundled plugins must load: {:?}",
        host.error
    );
    host
}

fn registry(host: &LuaHost) -> Registry {
    let mut registry = Registry::default();
    let (mut bindings, settings) = host.declarations();
    // As the binary does: the kernel's own chords — the ones that open a system
    // modal — are declared alongside the plugins', so they are listed, routed
    // and conflict-checked with everything else.
    bindings.extend(thurbox::kernel::modals::bindings());
    registry.declare(bindings, settings);
    registry
}

fn row(name: &str) -> SessionRow {
    SessionRow {
        id: format!("{name}-0000-0000-0000-000000000000"),
        name: name.to_string(),
        agent: "claude".to_string(),
        status: "idle".to_string(),
        cwd: Some(PathBuf::from("/src/thurbox")),
        repo: Some("thurbox".to_string()),
        repos: Vec::new(),
        branch: Some(format!("feat/{name}")),
        backend: "local-tmux".to_string(),
        backend_id: Some("%1".to_string()),
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

fn publish(host: &LuaHost, snapshot: &Snapshot) {
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

fn index_of(host: &LuaHost, plugin: &str) -> usize {
    host.index_of(plugin)
        .unwrap_or_else(|| panic!("no plugin named {plugin}"))
}

/// Build the keypress a terminal would deliver for a canonical chord.
fn press(chord: &str) -> KeyPress {
    let mut key = KeyPress::default();
    for part in chord.split('+') {
        match part {
            "ctrl" => key.ctrl = true,
            "alt" => key.alt = true,
            "shift" => key.shift = true,
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

/// Route a chord the way the binary does for a *global* key: resolve it with
/// nothing focused, then call the owning plugin's action.
fn fire(host: &LuaHost, chord: &str) {
    let registry = registry(host);
    let binding = registry
        .resolve(&press(chord), None)
        .unwrap_or_else(|| panic!("{chord} resolves to nothing"));
    let index = index_of(host, &binding.plugin);
    host.on_action(index, &binding.action)
        .unwrap_or_else(|e| panic!("{chord}: {e}"));
}

/// Every chord v1's key table calls global **whose owner still exists**, and
/// what v2 does with it.
///
/// Spelled out because the mapping from a v1 `Action` to a v2 plugin action is
/// the thing under test: inferring it would only prove the inference.
///
/// The chords v1 also binds globally but v2 currently answers with nothing are
/// below, in `CHORDS_AWAITING_THEIR_PANE`. They are listed rather than dropped so
/// re-adding a pane has an obvious place to reconnect, and so the shortfall is
/// counted rather than forgotten.
const GLOBAL_CHORDS: [(&str, &str); 21] = [
    ("ctrl+n", "new_session.open"),
    // One declaration, three spellings: the kernel folds the encodings a
    // terminal may deliver `Ctrl+/` as, where v1 bound all three by hand.
    ("ctrl+/", "search.open"),
    ("ctrl+7", "search.open"),
    ("ctrl+_", "search.open"),
    ("ctrl+d", "sessions.delete"),
    ("ctrl+r", "sessions.restart"),
    ("ctrl+f", "sessions.fork"),
    ("ctrl+s", "sessions.sync"),
    ("ctrl+o", "sessions.editor"),
    ("ctrl+z", "sessions.undo"),
    ("ctrl+j", "sessions.next"),
    ("ctrl+k", "sessions.previous"),
    ("ctrl+t", "shell.open"),
    ("ctrl+y", "themes.open"),
    ("ctrl+g", "help.open"),
    ("ctrl+,", "settings.open"),
    ("f4", "themes.open"),
    // F-key alternates, which reach a pane from a focused terminal.
    ("f1", "help.open"),
    ("f6", "settings.open"),
    ("f8", "shell.open"),
    ("f9", "sessions.toggle_panel"),
];

/// v1 chords with no v2 owner, each named with the pane that would bring it
/// back. Asserted to be *unbound* — a chord that silently resolved to something
/// else would be worse than one that does nothing.
const CHORDS_AWAITING_THEIR_PANE: [(&str, &str); 10] = [
    ("ctrl+u", "restore"),
    ("ctrl+p", "automations"),
    ("ctrl+w", "tasks"),
    ("ctrl+x", "review"),
    ("ctrl+b", "info"),
    ("ctrl+e", "files"),
    ("f2", "info"),
    ("f3", "files"),
    ("f5", "tasks"),
    ("f7", "review"),
];

#[test]
fn every_global_chord_v1_binds_resolves_to_a_v2_action() {
    let host = host();
    let registry = registry(&host);
    for (chord, action) in GLOBAL_CHORDS {
        let binding = registry
            .resolve(&press(chord), None)
            .unwrap_or_else(|| panic!("{chord} is bound to nothing"));
        assert_eq!(binding.action, action, "{chord}");
        assert_eq!(
            binding.scope,
            Scope::Global,
            "{chord} must fire from any pane, as it does in v1"
        );
    }
}

#[test]
fn a_chord_whose_pane_was_removed_is_unbound_rather_than_reused() {
    // Re-pointing a freed chord at something else would silently change what a
    // v1 user's muscle memory does. Better that it does nothing until its pane
    // returns — see `openspec/changes/v2-parity-gaps/`.
    let host = host();
    let registry = registry(&host);
    for (chord, pane) in CHORDS_AWAITING_THEIR_PANE {
        assert!(
            registry.resolve(&press(chord), None).is_none(),
            "{chord} belongs to the {pane} pane; it must stay unbound until that \
             pane is back, not be reused"
        );
    }
}

#[test]
fn the_session_list_column_is_hidden_and_restored_by_its_own_key() {
    // v1's F9 (`Action::ToggleSessionList`) gives the terminal the full width.
    // The arrangement decides this before any plugin renders, so the toggle has
    // to move state the arrangement can read — `ui/lib/panels.lua`.
    let host = host();
    let area = Rect {
        x: 0,
        y: 0,
        width: 140,
        height: 40,
    };
    let placed = |host: &LuaHost| -> Vec<String> {
        resolve(
            &host.arrangement(area.width, area.height).expect("layout"),
            area,
        )
        .iter()
        .map(|slot| slot.slot.clone())
        .collect()
    };

    assert!(placed(&host).contains(&"sessions".to_string()));
    fire(&host, "f9");
    assert!(
        !placed(&host).contains(&"sessions".to_string()),
        "F9 should take the session column out of the arrangement"
    );
    fire(&host, "f9");
    assert!(
        placed(&host).contains(&"sessions".to_string()),
        "and put it back"
    );
}

#[test]
fn the_session_chords_issue_the_commands_v1_runs() {
    let host = host();
    let snapshot = Snapshot {
        sessions: vec![row("fix-osc52")],
        ..Snapshot::default()
    };
    publish(&host, &snapshot);
    // Rendering is what settles the cursor onto a row, exactly as a frame does.
    host.render(
        index_of(&host, "sessions"),
        RenderContext {
            width: 40,
            height: 12,
            focused: true,
            elapsed: 1.0,
            frame: 1,
        },
    )
    .expect("render the list");
    host.drain_commands();

    for (chord, kind) in [
        ("ctrl+d", "delete"),
        ("ctrl+r", "restart"),
        ("ctrl+f", "fork"),
        ("ctrl+s", "sync"),
        ("ctrl+o", "editor"),
    ] {
        fire(&host, chord);
        let issued = host.drain_commands();
        assert_eq!(issued.len(), 1, "{chord} issued {issued:?}");
        assert_eq!(issued[0].kind(), kind, "{chord}");
        assert_eq!(issued[0].session(), snapshot.sessions[0].id, "{chord}");
    }
}

#[test]
fn undo_restores_the_delete_that_was_just_made_and_nothing_else() {
    // v1's Ctrl+Z is an undo of *your* delete (`App::undo_delete` restores its
    // own `pending_delete`), not "restore whatever was deleted last" — which
    // could be another instance's session.
    let host = host();
    let snapshot = Snapshot {
        sessions: vec![row("fix-osc52")],
        ..Snapshot::default()
    };
    publish(&host, &snapshot);
    host.render(
        index_of(&host, "sessions"),
        RenderContext {
            width: 40,
            height: 12,
            focused: true,
            elapsed: 1.0,
            frame: 1,
        },
    )
    .expect("render the list");
    host.drain_commands();

    // Nothing deleted yet: the key is inert rather than restoring a stranger.
    fire(&host, "ctrl+z");
    assert!(host.drain_commands().is_empty());

    fire(&host, "ctrl+d");
    host.drain_commands();
    fire(&host, "ctrl+z");
    let issued = host.drain_commands();
    assert_eq!(issued.len(), 1, "{issued:?}");
    assert_eq!(issued[0].kind(), "restore");
    assert_eq!(issued[0].session(), snapshot.sessions[0].id);

    // One undo per delete: the second press has nothing left to undo.
    fire(&host, "ctrl+z");
    assert!(host.drain_commands().is_empty());
}

/// Chords v1 hands to the agent that v2 does not yet.
///
#[test]
fn the_chords_v1_leaves_to_the_agent_are_marked_and_no_others_are() {
    // Read out of v1's own tables rather than restated: a chord that stops
    // being passthrough there must fail here rather than drift.
    let host = host();
    let registry = registry(&host);
    let mut checked = 0;
    for action in Action::all() {
        for chord in action.default_chords_for(false) {
            let chord = normalise_chord(&chord.display());
            // v1 gates the deferral on the bound chord being a bare
            // Ctrl+<letter>, so an F-key alternate is never handed over.
            if !is_ctrl_letter_chord(&chord) {
                continue;
            }
            let Some(binding) = registry
                .bindings()
                .iter()
                .find(|b| b.chord == chord && b.scope == Scope::Global)
            else {
                continue;
            };
            assert_eq!(
                binding.passthrough,
                action.terminal_passthrough(),
                "{chord} ({}) does not match v1's {action:?}",
                binding.action
            );
            checked += 1;
        }
    }
    // A mapping that matched nothing would pass vacuously.
    assert!(checked >= 10, "only {checked} chords were compared");
}

#[test]
fn a_bare_ctrl_letter_is_the_only_chord_that_defers() {
    // The gate v1 applies before handing a key to the pty. Anything else — an
    // F-key alternate, Ctrl+, , Ctrl+/ — reaches thurbox from the terminal too.
    for chord in ["ctrl+d", "ctrl+x", "ctrl+w"] {
        assert!(is_ctrl_letter_chord(chord), "{chord}");
    }
    for chord in ["f7", "ctrl+,", "ctrl+/", "ctrl+shift+d", "ctrl+7", "d"] {
        assert!(!is_ctrl_letter_chord(chord), "{chord}");
    }
}

#[test]
fn no_plugin_claims_a_chord_the_kernel_reserves() {
    // The escape route: quit, reload, focus movement and the perf HUD must work
    // whatever a plugin declares, so nothing may shadow them.
    let host = host();
    for binding in registry(&host).bindings() {
        assert!(
            !RESERVED.contains(&binding.chord.as_str()),
            "{} claims the reserved chord {}",
            binding.action,
            binding.chord
        );
    }
}

#[test]
fn the_bundled_plugins_agree_on_who_owns_which_chord() {
    // Two panes declaring `j` is fine — focus decides. A global chord claimed
    // twice is not, and the shadowed one would silently never fire.
    let host = host();
    let registry = registry(&host);
    assert!(
        registry.conflicts().is_empty(),
        "{:?}",
        registry.conflicts()
    );
}
