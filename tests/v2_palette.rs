//! The command palette against the real bundled plugins: every action the
//! registry knows is a row, typing filters them, and `Enter` runs the chosen one
//! through the same path a key press takes
//! (`openspec/changes/plugin-events-and-command-palette`).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use thurbox::kernel::host::{KeyPress, LuaHost};
use thurbox::kernel::modals::interface::Files;
use thurbox::kernel::modals::palette::{self, Dispatch, QUIT_ACTION, RELOAD_ACTION};
use thurbox::kernel::modals::{self, ModalKind, Modals, World};
use thurbox::kernel::registry::{binding_from, CommandDecl, Registry, Scope, Setting};
use thurbox::kernel::theme::Themes;

fn host() -> LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

/// The registry the binary builds: the plugins' declarations, their chord-less
/// commands, and the kernel's own chords.
fn registry(host: &LuaHost) -> Registry {
    let mut registry = Registry::default();
    let (mut bindings, settings) = host.declarations();
    bindings.extend(modals::bindings());
    registry.declare(bindings, settings);
    registry.declare_commands(host.commands());
    registry
}

fn key_press(chord: &str) -> KeyPress {
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

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Publish an empty world, so a plugin's handler has `thurbox.*` to read.
fn publish(host: &LuaHost, registry: &Registry) {
    let themes = Themes::load(None);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&thurbox::kernel::host::Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
        snapshot: &Default::default(),
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
        focus: None,
        hovered: None,
    })
    .expect("publish");
}

/// Drive the modal layer the way the loop does, and hand back what the palette
/// asked the loop to run, if anything.
fn modal_key(
    modals: &mut Modals,
    key: KeyEvent,
    chord: &str,
    registry: &mut Registry,
    themes: &mut Themes,
) -> Option<Dispatch> {
    let mut run = None;
    let mut world = World {
        settings_on_disk: &Default::default(),
        save_settings: &mut None,
        inventory: &[],
        interface_edit: &mut None,
        run_action: &mut run,
        registry,
        themes,
        db: None,
    };
    modals.on_key(&key, chord, &mut world);
    run
}

fn type_query(modals: &mut Modals, text: &str, registry: &mut Registry, themes: &mut Themes) {
    for c in text.chars() {
        modal_key(
            modals,
            press(KeyCode::Char(c)),
            &c.to_string(),
            registry,
            themes,
        );
    }
}

#[test]
fn every_kind_of_action_is_a_row() {
    let host = host();
    let mut registry = registry(&host);
    registry.declare_commands(vec![CommandDecl {
        plugin: "mine".into(),
        action: "mine.export".into(),
        description: "export the list".into(),
    }]);
    let rows = palette::rows(&registry);
    let find = |action: &str| rows.iter().find(|row| row.action == action);

    // A key-bound action, with its chord beside it.
    let delete = find("sessions.delete").expect("a plugin's key is a row");
    // Its alternates joined as help joins them: the pane-scoped `d` and the
    // global `ctrl+d` are one action.
    assert_eq!(delete.chords.as_deref(), Some("d / ctrl+d"));
    assert_eq!(delete.plugin, "sessions");
    // A chord-less command, with none.
    let export = find("mine.export").expect("a chord-less command is a row");
    assert_eq!(export.chords, None);
    assert_eq!(export.description, "export the list");
    // The kernel's own: the modals with their chords, plus reload and quit,
    // which no binding backs.
    let help = find("help.open").expect("a kernel modal is a row");
    assert_eq!(help.plugin, modals::OWNER);
    assert_eq!(help.chords.as_deref(), Some("f1 / ctrl+g"));
    assert_eq!(find(RELOAD_ACTION).unwrap().chords.as_deref(), Some("f10"));
    assert_eq!(find(QUIT_ACTION).unwrap().chords.as_deref(), Some("ctrl+q"));
    // Not itself: opening the palette from inside it is not an action.
    assert!(find("palette.open").is_none());
}

#[test]
fn a_disabled_plugins_actions_are_absent() {
    let mut host = host();
    host.set_disabled(vec!["plugins/65_search.lua".to_string()]);
    host.reload();
    assert!(host.error.is_none(), "{:?}", host.error);
    let rows = palette::rows(&registry(&host));
    assert!(
        !rows.iter().any(|row| row.action == "search.open"),
        "a plugin turned off declares nothing, so it has no rows"
    );
}

#[test]
fn ctrl_p_opens_the_palette_from_a_focused_terminal_and_over_another_modal() {
    let host = host();
    let registry = registry(&host);
    // Global and never deferred to the agent: the palette has to open from the
    // terminal, which is where the user mostly is.
    let binding = registry
        .resolve(&key_press("ctrl+p"), Some("agent"))
        .expect("ctrl+p is bound");
    assert_eq!(binding.action, "palette.open");
    assert_eq!(binding.scope, Scope::Global);
    assert!(!binding.passthrough);
    assert_eq!(
        ModalKind::from_action(&binding.action),
        Some(ModalKind::Palette)
    );

    let mut modals = Modals::default();
    modals.toggle(ModalKind::Help);
    modals.toggle(ModalKind::Palette);
    assert_eq!(
        modals.kind(),
        Some(ModalKind::Palette),
        "opening one closes another"
    );
    assert!(
        !modals.captures_everything(),
        "the reserved chords still escape it"
    );
    assert!(modals::escapes(&KeyEvent::new(
        KeyCode::Char('q'),
        KeyModifiers::CONTROL
    )));
}

#[test]
fn typing_filters_and_enter_runs_the_selection_through_on_action() {
    let host = host();
    let mut registry = registry(&host);
    let mut themes = Themes::load(None);
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Palette);

    type_query(&mut modals, "search.open", &mut registry, &mut themes);
    assert_eq!(modals.palette_query(), Some("search.open"));
    let dispatch = modal_key(
        &mut modals,
        press(KeyCode::Enter),
        "enter",
        &mut registry,
        &mut themes,
    )
    .expect("Enter hands the loop an action");
    assert_eq!(dispatch.plugin, "search");
    assert_eq!(dispatch.action, "search.open");
    assert!(!modals.is_open(), "closed before the loop runs it");

    // What the loop then does: the owning plugin's `on_action`, whether or not
    // it is focused — and the strip opens exactly as `ctrl+/` opens it.
    publish(&host, &registry);
    let index = host.index_of(&dispatch.plugin).expect("the search pane");
    assert!(host.on_action(index, &dispatch.action).expect("on_action"));
    assert_eq!(host.shared_bool("panels.search"), Some(true));
}

#[test]
fn nothing_matching_means_enter_runs_nothing_and_esc_runs_nothing() {
    let host = host();
    let mut registry = registry(&host);
    let mut themes = Themes::load(None);
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Palette);
    type_query(&mut modals, "zzzzzz", &mut registry, &mut themes);
    assert!(modal_key(
        &mut modals,
        press(KeyCode::Enter),
        "enter",
        &mut registry,
        &mut themes
    )
    .is_none());
    assert!(modals.is_open());
    assert!(modal_key(
        &mut modals,
        press(KeyCode::Esc),
        "esc",
        &mut registry,
        &mut themes
    )
    .is_none());
    assert!(!modals.is_open(), "Esc closes without choosing");
}

#[test]
fn a_chord_less_command_can_be_given_a_chord_and_is_then_a_key() {
    let mut registry = Registry::default();
    registry.declare(
        vec![binding_from(
            "mine",
            "j",
            "mine.next",
            "next item",
            None,
            false,
            None,
        )],
        Vec::<Setting>::new(),
    );
    registry.declare_commands(vec![CommandDecl {
        plugin: "mine".into(),
        action: "mine.export".into(),
        description: "export the list".into(),
    }]);
    assert!(registry.resolve(&key_press("f7"), Some("mine")).is_none());

    registry
        .rebind("mine.export", Some("f7"))
        .expect("a command is a legal rebind target");
    let binding = registry
        .resolve(&key_press("f7"), Some("mine"))
        .expect("the chord now fires the command");
    assert_eq!(binding.action, "mine.export");
    assert!(binding.overridden);
    // Help lists it, since it is a binding now; the palette shows the chord.
    assert!(registry
        .bindings()
        .iter()
        .any(|b| b.action == "mine.export"));
    let rows = palette::rows(&registry);
    let export = rows.iter().find(|row| row.action == "mine.export").unwrap();
    assert_eq!(export.chords.as_deref(), Some("f7"));
    // One row, not two, even though it is both a command and a key.
    assert_eq!(
        rows.iter()
            .filter(|row| row.action == "mine.export")
            .count(),
        1
    );

    // Reset, and it is chord-less again.
    registry.rebind("mine.export", None).expect("reset");
    assert!(registry.resolve(&key_press("f7"), Some("mine")).is_none());
    assert!(!registry
        .bindings()
        .iter()
        .any(|b| b.action == "mine.export"));
}

#[test]
fn a_key_and_a_command_for_one_action_are_one_row_with_the_chord() {
    let mut registry = Registry::default();
    registry.declare(
        vec![binding_from(
            "mine",
            "x",
            "mine.export",
            "",
            None,
            false,
            None,
        )],
        Vec::<Setting>::new(),
    );
    registry.declare_commands(vec![CommandDecl {
        plugin: "mine".into(),
        action: "mine.export".into(),
        description: "export the list".into(),
    }]);
    let rows = palette::rows(&registry);
    let export: Vec<_> = rows
        .iter()
        .filter(|row| row.action == "mine.export")
        .collect();
    assert_eq!(export.len(), 1);
    assert_eq!(export[0].chords.as_deref(), Some("x"));
    assert_eq!(
        export[0].description, "export the list",
        "the command's description fills the key's blank"
    );
}

/// The palette's frame, pinned cell for cell over a fixed registry.
#[test]
fn the_palette_frame_is_pinned() {
    let mut registry = Registry::default();
    registry.declare(
        vec![
            binding_from(
                "sessions",
                "ctrl+d",
                "sessions.delete",
                "delete the selected session",
                Some("global"),
                false,
                None,
            ),
            binding_from(
                "agent",
                "f8",
                "shell.open",
                "open a shell",
                None,
                false,
                None,
            ),
        ],
        Vec::<Setting>::new(),
    );
    registry.declare_commands(vec![CommandDecl {
        plugin: "mine".into(),
        action: "mine.export".into(),
        description: "export the list".into(),
    }]);
    let mut themes = Themes::load(None);
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Palette);
    type_query(&mut modals, "e", &mut registry, &mut themes);

    let mut terminal = Terminal::new(TestBackend::new(60, 12)).expect("terminal");
    terminal
        .draw(|frame| {
            modals.render(
                frame,
                Rect::new(0, 0, 60, 12),
                &registry,
                &themes,
                &Default::default(),
                Files {
                    rows: &[],
                    dir: "ui",
                },
            );
        })
        .expect("draw");
    let buffer = terminal.backend().buffer();
    let actual: Vec<String> = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    let expected = [
        "",
        "",
        "            ╭ Commands ────────────────────────╮",
        "            │ > e█                         5/5 │",
        "            │ ▸ sessions delete the s…  ctrl+d │",
        "            │   agent    open a shell       f8 │",
        "            │   mine     export the list       │",
        "            │   kernel   reload the inte…  f10 │",
        "            │   kernel   quit (sessio…  ctrl+q │",
        "            │type filter  ↑/↓ mov  Run   Close │",
        "            ╰──────────────────────────────────╯",
        "",
    ];
    if actual
        .iter()
        .map(String::as_str)
        .ne(expected.iter().copied())
    {
        let literal = actual
            .iter()
            .map(|line| format!("        {line:?},"))
            .collect::<Vec<_>>()
            .join("\n");
        panic!("frame differs from the expected literal.\nactual, as a literal:\n[\n{literal}\n]");
    }
}

#[test]
fn the_agent_pane_is_reachable_by_name_from_the_palette() {
    // The pane's tabs were click-only roles and focusing it was only ever the
    // session list's `Enter`, so "open the agent" had no row. Now it has three,
    // none spending a chord, and the focus one issues the same command the
    // list's Enter does.
    let host = host();
    let registry = registry(&host);
    let rows = palette::rows(&registry);
    for action in ["terminal.focus", "terminal.agent", "terminal.shell"] {
        let row = rows
            .iter()
            .find(|row| row.action == action)
            .unwrap_or_else(|| panic!("{action} is not a palette row"));
        assert_eq!(row.plugin, "agent");
        assert_eq!(row.chords, None, "{action} is chord-less");
    }
    publish(&host, &registry);
    let index = host.index_of("agent").expect("the agent pane");
    assert!(host.on_action(index, "terminal.focus").expect("on_action"));
    let issued = host.drain_commands();
    assert_eq!(
        issued,
        vec![thurbox::kernel::command::Command::Focus {
            plugin: "agent".into(),
            toggle: false
        }]
    );
}
