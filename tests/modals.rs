//! The system modals — help, settings and the theme picker — against the real
//! bundled plugins.
//!
//! What is under test is the shape rather than the pixels: that these three are
//! *not* plugins, that they overlay the interface instead of replacing a pane,
//! that they capture input while open, and that a plugin reaches them by
//! declaring data and nothing else.
//!
//! Renders the modals through the same entry point the binary uses
//! (`Modals::render`), so a modal that stopped drawing would fail here.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use thurbox::kernel::host::{KeyPress, LuaHost, Published, RenderContext};
use thurbox::kernel::layout::{resolve, SlotMode};
use thurbox::kernel::modals::interface::Files;
use thurbox::kernel::modals::{self, ModalKind, Modals, World};
use thurbox::kernel::registry::{binding_from, Registry, Setting, Value};
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;
use thurbox::storage::Database;

const WIDTH: u16 = 150;
const HEIGHT: u16 = 40;

fn host() -> LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

/// The registry the binary builds: every plugin's declarations plus the
/// kernel's own modal chords.
fn registry(host: &LuaHost) -> Registry {
    let mut registry = Registry::default();
    let (mut bindings, settings) = host.declarations();
    bindings.extend(modals::bindings());
    registry.declare(bindings, settings);
    registry
}

fn row(name: &str) -> SessionRow {
    SessionRow {
        id: format!("{name}-0000-0000-0000-000000000000"),
        name: name.into(),
        agent: "claude".into(),
        status: "idle".into(),
        cwd: Some(std::path::PathBuf::from("/src/thurbox")),
        repo: Some("thurbox".into()),
        repos: vec!["thurbox".into()],
        branch: Some(format!("feat/{name}")),
        base_branch: None,
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

fn sample() -> Snapshot {
    Snapshot {
        sessions: vec![row("fix-osc52"), row("add-wsl")],
        ..Snapshot::default()
    }
}

fn publish(host: &LuaHost, registry: &Registry, themes: &Themes) {
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
        snapshot: &sample(),
        attach_errors: &Default::default(),
        inflight: &[],
        themes,
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

/// Paint the whole interface — every plugin in its resolved rect — and then,
/// optionally, the modal layer over it. This is the binary's own order.
fn screen(
    host: &LuaHost,
    modals: &mut Modals,
    registry: &Registry,
    themes: &Themes,
) -> Vec<String> {
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let region = host.arrangement(WIDTH, HEIGHT).expect("arrangement");
    let placed = resolve(&region, area);

    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("terminal");
    terminal
        .draw(|frame| {
            for slot in &placed {
                let members = host.in_slot(&slot.slot);
                let Some(&index) = members.first() else {
                    continue;
                };
                let members = match host.slot_mode(&slot.slot) {
                    SlotMode::Switch => vec![index],
                    SlotMode::Stack => members.to_vec(),
                };
                for index in members {
                    let ctx = RenderContext {
                        width: slot.rect.width,
                        height: slot.rect.height,
                        focused: false,
                        elapsed: 0.0,
                        frame: 0,
                    };
                    let Ok(rendered) = host.render(index, ctx) else {
                        continue;
                    };
                    if rendered.float.is_some() {
                        continue;
                    }
                    thurbox::kernel::paint::render(
                        frame,
                        slot.rect,
                        &rendered.node,
                        &thurbox::kernel::paint::PlaceholderSurfaces,
                    );
                }
            }
            modals.render(
                frame,
                area,
                registry,
                themes,
                &Default::default(),
                Files {
                    rows: &[],
                    dir: "ui",
                },
            );
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    (0..HEIGHT)
        .map(|y| {
            (0..WIDTH)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

/// Paint only the modal layer, on a screen of the given size.
fn modal_screen(
    modals: &mut Modals,
    registry: &Registry,
    themes: &Themes,
    width: u16,
    height: u16,
) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| {
            modals.render(
                frame,
                frame.area(),
                registry,
                themes,
                &Default::default(),
                Files {
                    rows: &[],
                    dir: "ui",
                },
            )
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a modal does to a cell it does not cover.
///
/// Regression: opening a modal stripped the footer band's chip fills, so every
/// button in the bottom band lost its background and read as plain text until
/// the modal closed. The dim pass is meant to *darken* what it covers, not
/// discard the colours underneath.
#[test]
fn a_modal_dims_the_chrome_beneath_it_rather_than_erasing_its_colours() {
    use ratatui::style::{Color, Style};
    use ratatui::widgets::Paragraph;

    let themes = Themes::load(None);
    let registry = Registry::default();
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Help);

    // A filled chip on the bottom row, exactly as the action band draws one.
    let filled = Style::default().fg(Color::White).bg(Color::Indexed(24));
    let mut terminal = Terminal::new(TestBackend::new(90, 20)).expect("terminal");
    terminal
        .draw(|frame| {
            let row = ratatui::layout::Rect {
                x: 0,
                y: 19,
                width: 90,
                height: 1,
            };
            frame.render_widget(
                Paragraph::new(ratatui::text::Line::from(ratatui::text::Span::styled(
                    " Help · F1 ",
                    filled,
                ))),
                row,
            );
            modals.render(
                frame,
                frame.area(),
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

    let buffer = terminal.backend().buffer().clone();
    let chip = &buffer[(1, 19)];
    assert_ne!(
        chip.bg,
        Color::Reset,
        "the chip under a modal must keep a background, not fall back to the \
         terminal default — that is what made the bottom band's buttons vanish"
    );
}

/// The chord column must never touch the description beside it.
#[test]
fn a_long_chord_list_widens_the_column_instead_of_running_into_its_description() {
    // Regression: the column was a fixed 16 and `pad` only ever ADDS spaces, so
    // an action carrying three alternates (`j / down / ctrl+j`, 17 columns) had
    // its description butted straight onto it with no gap.
    let themes = Themes::load(None);
    let host = host();
    let mut registry = Registry::default();
    let (mut bindings, settings) = host.declarations();
    bindings.extend(thurbox::kernel::modals::bindings());
    registry.declare(bindings, settings);

    let mut modals = Modals::default();
    modals.toggle(ModalKind::Help);
    let screen = modal_screen(&mut modals, &registry, &themes, 100, 40);

    // Every row that lists chords keeps at least one space before its
    // description. Checked by finding the widest chord run and asserting no line
    // has a non-space immediately after it.
    let long_rows: Vec<&str> = screen.lines().filter(|line| line.contains(" / ")).collect();
    assert!(
        !long_rows.is_empty(),
        "expected at least one row with alternates:\n{screen}"
    );
    for line in long_rows {
        let joined = line.trim_start_matches([' ', '›']);
        // The chords end at the first double space; whatever follows must not be
        // glued to them.
        let gap = joined
            .find("  ")
            .unwrap_or_else(|| panic!("no gap after the chords in {line:?}"));
        assert!(gap > 0, "{line:?}");
    }

    // And the descriptions still line up: every row's description starts at the
    // same column.
    let starts: std::collections::BTreeSet<usize> = screen
        .lines()
        .filter(|line| line.contains(" / ") || line.trim_start().starts_with("ctrl+q"))
        .filter_map(|line| {
            let after = line.find("  ")?;
            line[after..]
                .find(|c: char| !c.is_whitespace())
                .map(|offset| after + offset)
        })
        .collect();
    assert_eq!(
        starts.len(),
        1,
        "descriptions must share one column, found {starts:?}\n{screen}"
    );
}

/// Moving through the theme picker applies as it goes, and leaving puts it back.
#[test]
fn the_theme_picker_previews_as_you_move_and_reverts_when_abandoned() {
    let dir = tempfile::tempdir().expect("temp dir");
    let _guard = thurbox::paths::TestPathGuard::new(dir.path());
    let mut registry = Registry::default();
    let mut themes = Themes::load(None);
    let opened_with = themes.active_name().to_string();

    let mut modals = Modals::default();
    modals.toggle(ModalKind::Theme);
    // Settles the cursor onto the active theme and records what to put back.
    modal_screen(&mut modals, &registry, &themes, 90, 30);

    // Moving previews: the palette changes without being asked to.
    send(
        &mut modals,
        press(KeyCode::Down),
        "down",
        &mut registry,
        &mut themes,
        None,
    );
    let previewed = themes.active_name().to_string();
    assert_ne!(
        previewed, opened_with,
        "moving the cursor should apply the theme it lands on"
    );

    // Nothing was persisted — a preview is not a choice.
    assert_eq!(
        thurbox::storage::Database::open(&thurbox::paths::database_file().expect("path"))
            .expect("db")
            .get_active_theme()
            .expect("read"),
        None,
        "a preview must not be written to the database"
    );

    // Esc puts back what was active when it opened.
    send(
        &mut modals,
        press(KeyCode::Esc),
        "esc",
        &mut registry,
        &mut themes,
        None,
    );
    assert!(!modals.is_open());
    assert_eq!(
        themes.active_name(),
        opened_with,
        "abandoning the picker must undo the preview"
    );
}

#[test]
fn choosing_a_theme_keeps_it_and_persists_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let _guard = thurbox::paths::TestPathGuard::new(dir.path());
    let mut registry = Registry::default();
    let mut themes = Themes::load(None);
    let opened_with = themes.active_name().to_string();

    let mut modals = Modals::default();
    modals.toggle(ModalKind::Theme);
    modal_screen(&mut modals, &registry, &themes, 90, 30);

    send(
        &mut modals,
        press(KeyCode::Down),
        "down",
        &mut registry,
        &mut themes,
        None,
    );
    let chosen = themes.active_name().to_string();
    assert_ne!(chosen, opened_with);

    send(
        &mut modals,
        press(KeyCode::Enter),
        "enter",
        &mut registry,
        &mut themes,
        None,
    );
    assert!(!modals.is_open(), "choosing closes the picker");
    assert_eq!(
        themes.active_name(),
        chosen,
        "the chosen theme stays; only an abandoned preview is undone"
    );
}

#[test]
fn closing_the_picker_by_its_own_chord_also_reverts() {
    // Toggling it shut is no more a choice than pressing Esc.
    let dir = tempfile::tempdir().expect("temp dir");
    let _guard = thurbox::paths::TestPathGuard::new(dir.path());
    let mut registry = Registry::default();
    let mut themes = Themes::load(None);
    let opened_with = themes.active_name().to_string();

    let mut modals = Modals::default();
    modals.toggle(ModalKind::Theme);
    modal_screen(&mut modals, &registry, &themes, 90, 30);
    send(
        &mut modals,
        press(KeyCode::Down),
        "down",
        &mut registry,
        &mut themes,
        None,
    );
    assert_ne!(themes.active_name(), opened_with);

    // What the loop does for the picker's own chord.
    {
        let mut world = World {
            settings_on_disk: &Default::default(),
            save_settings: &mut None,
            inventory: &[],
            interface_edit: &mut None,
            run_action: &mut None,
            registry: &mut registry,
            themes: &mut themes,
            db: None,
        };
        modals.abandon(&mut world);
    }
    modals.toggle(ModalKind::Theme);
    assert!(!modals.is_open());
    assert_eq!(themes.active_name(), opened_with);
}

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Feed a key to the modal layer, the way the binary does.
fn send(
    modals: &mut Modals,
    key: KeyEvent,
    chord: &str,
    registry: &mut Registry,
    themes: &mut Themes,
    db: Option<&Database>,
) -> Option<String> {
    let mut world = World {
        settings_on_disk: &Default::default(),
        save_settings: &mut None,
        inventory: &[],
        interface_edit: &mut None,
        run_action: &mut None,
        registry,
        themes,
        db,
    };
    modals.on_key(&key, chord, &mut world)
}

// --- the layer -------------------------------------------------------------

#[test]
fn no_modal_is_a_plugin_or_a_focus_stop() {
    // The whole point of the change: system chrome does not compete for a
    // layout slot, so it can never lengthen the focus ring.
    let host = host();
    let names: Vec<&str> = host.plugins.iter().map(|p| p.name.as_str()).collect();
    let focusable: Vec<&str> = host
        .focusable()
        .iter()
        .map(|i| host.plugins[*i].name.as_str())
        .collect();
    for gone in ["help", "settings", "themes"] {
        assert!(
            !names.contains(&gone),
            "{gone} is still a plugin: {names:?}"
        );
        assert!(!focusable.contains(&gone), "{gone} is still a focus stop");
    }
}

#[test]
fn every_modal_chord_v1_binds_opens_its_modal() {
    // Declared through the ordinary registry, so they are listed in help and
    // rebindable — but routed to the kernel, since no plugin owns them.
    let host = host();
    let registry = registry(&host);
    for (chord, kind) in [
        ("f1", ModalKind::Help),
        ("ctrl+g", ModalKind::Help),
        ("f4", ModalKind::Theme),
        ("ctrl+y", ModalKind::Theme),
        ("f6", ModalKind::Settings),
        ("ctrl+,", ModalKind::Settings),
        ("ctrl+p", ModalKind::Palette),
    ] {
        let binding = registry
            .resolve(&key_press(chord), None)
            .unwrap_or_else(|| panic!("{chord} is bound to nothing"));
        assert_eq!(
            ModalKind::from_action(&binding.action),
            Some(kind),
            "{chord}"
        );
    }
    assert!(
        registry.conflicts().is_empty(),
        "a modal chord clashes with a plugin's: {:?}",
        registry.conflicts()
    );
}

#[test]
fn one_modal_at_a_time_and_the_opening_chord_toggles() {
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Help);
    modals.toggle(ModalKind::Settings);
    assert_eq!(
        modals.kind(),
        Some(ModalKind::Settings),
        "opening one closes another"
    );
    modals.toggle(ModalKind::Settings);
    assert!(!modals.is_open(), "the opening chord toggles");
}

#[test]
fn a_modal_covers_the_interface_rather_than_replacing_it() {
    let host = host();
    let registry = registry(&host);
    let themes = Themes::load(None);
    publish(&host, &registry, &themes);

    // Rendered once first: a plugin settles state on its first frame (the list
    // picks a row, and the header then names it), and that settling would
    // otherwise read as the modal having repainted the row.
    let mut closed = Modals::default();
    screen(&host, &mut closed, &registry, &themes);
    let before = screen(&host, &mut closed, &registry, &themes);
    let mut open = Modals::default();
    open.toggle(ModalKind::Help);
    let after = screen(&host, &mut open, &registry, &themes);

    // The modal is centred, so the interface survives at the edges…
    assert_eq!(before[0], after[0], "the top row was repainted");
    assert_eq!(
        before[HEIGHT as usize - 1],
        after[HEIGHT as usize - 1],
        "the bottom row was repainted"
    );
    // …and only the middle changed.
    assert_ne!(
        before[HEIGHT as usize / 2],
        after[HEIGHT as usize / 2],
        "the modal drew nothing"
    );
    assert!(
        after.join("\n").contains("Keybindings"),
        "{}",
        after.join("\n")
    );
}

// --- help ------------------------------------------------------------------

#[test]
fn help_lists_keys_from_plugins_it_never_heard_of() {
    // The payoff of declaring keys as data: help renders the registry, so a
    // plugin added tomorrow appears with no edit to the modal.
    let host = host();
    let mut registry = registry(&host);
    let mut themes = Themes::load(None);
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Help);

    let screen = modal_screen(&mut modals, &registry, &themes, 120, 90);
    // Grouped by function, as v1 groups it — not by which plugin owns the key.
    assert!(screen.contains("Navigation"), "{screen}");
    assert!(screen.contains("Sessions"), "{screen}");
    // A key declared by a plugin the modal has never heard of.
    assert!(screen.contains("delete session"), "{screen}");
    // The kernel's own chords are declarations like any other.
    assert!(screen.contains("open keybindings help"), "{screen}");

    // The reserved chords are the last section, so scroll to them. That they
    // need scrolling is the point: help lists everything, not a summary.
    send(
        &mut modals,
        press(KeyCode::End),
        "end",
        &mut registry,
        &mut themes,
        None,
    );
    let bottom = modal_screen(&mut modals, &registry, &themes, 120, 90);
    assert!(bottom.contains("Fixed (not rebindable)"), "{bottom}");
    // Spelled as v1 spells a chord, not as the footer compacts one.
    assert!(bottom.contains("ctrl+q"), "{bottom}");
}

#[test]
fn a_key_declared_by_an_unknown_plugin_appears_without_touching_help() {
    let mut registry = Registry::default();
    registry.declare(
        vec![binding_from(
            "weather",
            "ctrl+shift+w",
            "weather.refresh",
            "refresh the forecast",
            None,
            false,
            Some("Weather"),
        )],
        Vec::new(),
    );
    let themes = Themes::load(None);
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Help);
    let screen = modal_screen(&mut modals, &registry, &themes, 120, 40);
    assert!(screen.contains("Weather"), "{screen}");
    assert!(screen.contains("refresh the forecast"), "{screen}");
    assert!(screen.contains("ctrl+shift+w"), "{screen}");
}

#[test]
fn one_row_per_action_joins_its_chords_the_way_v1_spells_them() {
    // `ctrl+g / f1`, not two unrelated rows and not the footer's compact `^G`.
    let host = host();
    let registry = registry(&host);
    let themes = Themes::load(None);
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Help);
    let screen = modal_screen(&mut modals, &registry, &themes, 120, 90);
    assert!(screen.contains("f1 / ctrl+g"), "{screen}");
    assert!(!screen.contains("^G"), "help must not use the compact form");
}

#[test]
fn capturing_ctrl_q_in_help_is_data_rather_than_a_quit() {
    // The reason the modal layer belongs to the kernel: while
    // capturing, EVERY chord reaches the modal — including the one chord the
    // loop otherwise handles before anything else.
    let host = host();
    let mut registry = registry(&host);
    let mut themes = Themes::load(None);
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Help);
    send(
        &mut modals,
        press(KeyCode::Char('r')),
        "r",
        &mut registry,
        &mut themes,
        None,
    );
    assert!(
        modals.captures_everything(),
        "a capture must outrank the kernel's reserved chords"
    );

    let message = send(
        &mut modals,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
        "ctrl+q",
        &mut registry,
        &mut themes,
        None,
    );
    // Consumed and answered rather than obeyed: the app is still running, and
    // the registry says why the chord cannot be taken.
    assert!(
        message.is_some_and(|m| m.contains("reserved")),
        "ctrl+q while capturing must be answered, not acted on"
    );
    assert!(
        !modals.captures_everything(),
        "the capture lasts one keystroke"
    );
}

#[test]
fn a_rebind_moves_the_chord_and_says_what_moved() {
    let dir = tempfile::tempdir().expect("temp dir");
    let _guard = thurbox::paths::TestPathGuard::new(dir.path());
    let host = host();
    let mut registry = registry(&host);
    let mut themes = Themes::load(None);
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Help);

    // Whatever the first listed action is, take a chord that already belongs to
    // something else and give it to that action.
    let first = registry.bindings()[0].action.clone();
    let victim = registry
        .bindings()
        .iter()
        .find(|binding| binding.action != first)
        .expect("a second binding")
        .clone();

    send(
        &mut modals,
        press(KeyCode::Enter),
        "enter",
        &mut registry,
        &mut themes,
        None,
    );
    let message = send(
        &mut modals,
        press(KeyCode::F(9)),
        &victim.chord,
        &mut registry,
        &mut themes,
        None,
    )
    .expect("a rebind reports");
    assert!(message.contains(&victim.action), "{message}");

    // The reassignment is real: the chord now fires the action it was moved to,
    // not the declaration that merely defaults to it.
    let selected = registry.bindings()[0].action.clone();
    assert_eq!(
        registry
            .resolve(&key_press(&victim.chord), None)
            .map(|b| b.action.clone()),
        Some(selected)
    );
}

#[test]
fn reset_puts_an_action_back_on_its_declared_chord() {
    let dir = tempfile::tempdir().expect("temp dir");
    let _guard = thurbox::paths::TestPathGuard::new(dir.path());
    let host = host();
    let mut registry = registry(&host);
    let mut themes = Themes::load(None);
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Help);

    let default_chord = registry.bindings()[0].default_chord.clone();
    for (key, chord) in [
        (press(KeyCode::Enter), "enter"),
        (press(KeyCode::F(9)), "f9"),
    ] {
        send(&mut modals, key, chord, &mut registry, &mut themes, None);
    }
    assert_eq!(registry.bindings()[0].chord, "f9");

    send(
        &mut modals,
        press(KeyCode::Char('d')),
        "d",
        &mut registry,
        &mut themes,
        None,
    );
    assert_eq!(registry.bindings()[0].chord, default_chord);
    assert!(!registry.bindings()[0].overridden);
}

// --- settings --------------------------------------------------------------

#[test]
fn settings_renders_what_the_bundled_plugins_declare() {
    // The modal is only worth having if something reaches it: at least one
    // shipped plugin must declare a setting.
    let host = host();
    let registry = registry(&host);
    let themes = Themes::load(None);
    assert!(
        !registry.settings().is_empty(),
        "no bundled plugin declares a setting, so the modal has nothing to show"
    );
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Settings);
    // Tall enough that the core rows above do not push the plugin's off screen.
    let screen = modal_screen(&mut modals, &registry, &themes, 130, 60);
    let first = &registry.settings()[0];
    assert!(screen.contains("Settings"), "{screen}");
    // The kernel's own settings are listed first, so `settings.toml` is editable
    // from the same screen as everything a plugin declared.
    assert!(screen.contains("CORE"), "{screen}");
    assert!(screen.contains("features.soft_delete"), "{screen}");
    // Grouped by the declaring plugin, which is the only grouping the registry
    // knows about.
    assert!(screen.contains(&first.plugin.to_uppercase()), "{screen}");
    assert!(screen.contains(&first.id), "{screen}");
}

#[test]
fn a_setting_declared_by_an_unknown_plugin_is_editable_without_touching_settings() {
    let dir = tempfile::tempdir().expect("temp dir");
    let _guard = thurbox::paths::TestPathGuard::new(dir.path());
    // Loaded, like the loop's own: writing `ui.json` back is a capability of a
    // registry that read it (`registry::Origin`), and persistence is the half of
    // this test that matters.
    let mut registry = Registry::load();
    registry.declare(
        Vec::new(),
        vec![Setting {
            plugin: "weather".into(),
            id: "units".into(),
            description: "metric or imperial".into(),
            default: Value::Bool(false),
            value: Value::Bool(false),
        }],
    );
    let mut themes = Themes::load(None);
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Settings);

    // The kernel's own core settings come first, so a plugin's row is the last
    // one — `End` is how the modal gets there, and it is what a user with a long
    // list does too.
    send(
        &mut modals,
        press(KeyCode::End),
        "end",
        &mut registry,
        &mut themes,
        None,
    );
    let screen = modal_screen(&mut modals, &registry, &themes, 100, 20);
    assert!(screen.contains("WEATHER"), "{screen}");
    assert!(screen.contains("metric or imperial"), "{screen}");

    send(
        &mut modals,
        press(KeyCode::Char(' ')),
        "space",
        &mut registry,
        &mut themes,
        None,
    );
    assert_eq!(registry.settings()[0].value, Value::Bool(true));
    // Written through the registry, so it is persisted where every other
    // override lives.
    let overrides = thurbox::paths::config_file()
        .and_then(|config| config.parent().map(|dir| dir.join("ui.json")))
        .expect("a config directory");
    assert!(
        overrides.exists(),
        "the override was not persisted to {}",
        overrides.display()
    );
}

// --- theme -----------------------------------------------------------------

#[test]
fn every_v1_theme_is_offered_by_the_picker() {
    let host = host();
    let registry = registry(&host);
    let themes = Themes::load(None);
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Theme);
    // Tall enough that the scroll window does not hide the entries asserted.
    let screen = modal_screen(&mut modals, &registry, &themes, 100, 60);
    for name in ["Default", "Tokyo Night", "Gruvbox Dark"] {
        assert!(screen.contains(name), "{name} missing:\n{screen}");
    }
    // v1's picker heads the list with a `matched/total` count and groups the
    // presets under Dark/Light headers.
    assert!(screen.contains("themes"), "{screen}");
    assert!(screen.contains("Dark"), "{screen}");
    assert!(screen.contains("Light"), "{screen}");
    // The swatch, which is what makes it a picker rather than a list.
    assert!(screen.contains("Accent"), "{screen}");
    assert!(screen.contains("theme id:"), "{screen}");
    assert!(themes.choices().len() >= 36, "v1 shipped 36 presets");
}

#[test]
fn the_filter_narrows_the_list_and_the_count_says_so() {
    let host = host();
    let mut registry = registry(&host);
    let mut themes = Themes::load(None);
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Theme);

    for (key, chord) in [
        (press(KeyCode::Char('/')), "/"),
        (press(KeyCode::Char('n')), "n"),
        (press(KeyCode::Char('o')), "o"),
        (press(KeyCode::Char('r')), "r"),
        (press(KeyCode::Char('d')), "d"),
    ] {
        send(&mut modals, key, chord, &mut registry, &mut themes, None);
    }
    let screen = modal_screen(&mut modals, &registry, &themes, 100, 40);
    assert!(screen.contains("Nord"), "{screen}");
    assert!(
        !screen.contains("Gruvbox"),
        "the filter did not narrow:\n{screen}"
    );
    assert!(
        screen.contains(&format!("/{} themes", themes.choices().len())),
        "the count must show matched/total:\n{screen}"
    );

    // Esc closes the filter first and the picker second, as v1's does.
    send(
        &mut modals,
        press(KeyCode::Esc),
        "esc",
        &mut registry,
        &mut themes,
        None,
    );
    assert!(modals.is_open(), "the first Esc belongs to the filter");
    send(
        &mut modals,
        press(KeyCode::Esc),
        "esc",
        &mut registry,
        &mut themes,
        None,
    );
    assert!(!modals.is_open());
}

#[test]
fn enter_applies_the_highlighted_theme_and_persists_it_where_v1_does() {
    let db = Database::open_in_memory().expect("in-memory database");
    let mut registry = Registry::default();
    let mut themes = Themes::load(Some(&db));
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Theme);

    send(
        &mut modals,
        press(KeyCode::Char('j')),
        "j",
        &mut registry,
        &mut themes,
        Some(&db),
    );
    let wanted = themes.choices()[1].name.clone();
    send(
        &mut modals,
        press(KeyCode::Enter),
        "enter",
        &mut registry,
        &mut themes,
        Some(&db),
    );

    assert_eq!(themes.active_name(), wanted);
    assert_eq!(db.get_active_theme().expect("read"), Some(wanted));
    // Applying closes the picker, so the palette just chosen is visible.
    assert!(!modals.is_open());
}

// --- the mouse -------------------------------------------------------------

/// Paint only the modal layer and keep the buffer, so a test can ask what
/// *colour* a cell ended up — which is all a hover affordance is.
fn modal_buffer(
    modals: &mut Modals,
    registry: &Registry,
    themes: &Themes,
    width: u16,
    height: u16,
) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| {
            modals.render(
                frame,
                frame.area(),
                registry,
                themes,
                &Default::default(),
                Files {
                    rows: &[],
                    dir: "ui",
                },
            )
        })
        .expect("draw");
    terminal.backend().buffer().clone()
}

/// Where `label` was painted, as the coordinates of its first cell.
///
/// A pill is drawn as ` Label `, so this is how a test finds a button to click
/// without the modal exposing its hitboxes — and it fails when the label is not
/// on screen, which is itself the assertion that the pill was drawn.
fn locate(buffer: &ratatui::buffer::Buffer, width: u16, height: u16, label: &str) -> (u16, u16) {
    for y in 0..height {
        let row: String = (0..width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect();
        if let Some(byte) = row.find(label) {
            // Columns, not bytes: the modal frame and its markers are
            // multi-byte glyphs.
            return (row[..byte].chars().count() as u16, y);
        }
    }
    panic!("'{label}' is nowhere on screen");
}

fn click(
    modals: &mut Modals,
    x: u16,
    y: u16,
    registry: &mut Registry,
    themes: &mut Themes,
    db: Option<&Database>,
) -> Option<String> {
    let mut world = World {
        settings_on_disk: &Default::default(),
        save_settings: &mut None,
        inventory: &[],
        interface_edit: &mut None,
        run_action: &mut None,
        registry,
        themes,
        db,
    };
    modals.on_click(x, y, &mut world)
}

#[test]
fn clicking_select_applies_the_theme_exactly_as_enter_does() {
    // The rule a button exists to keep: it replays a key through the same
    // handler the keyboard uses, so ` Select ` cannot come to mean something
    // `Enter` does not.
    let db = Database::open_in_memory().expect("in-memory database");
    let mut registry = Registry::default();
    let mut themes = Themes::load(Some(&db));
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Theme);

    send(
        &mut modals,
        press(KeyCode::Char('j')),
        "j",
        &mut registry,
        &mut themes,
        Some(&db),
    );
    let wanted = themes.choices()[1].name.clone();

    // Rendered first: a modal paints cell by cell, so its hitboxes exist only
    // once it has drawn — which is exactly the state a real click arrives in.
    let buffer = modal_buffer(&mut modals, &registry, &themes, 100, 40);
    let (x, y) = locate(&buffer, 100, 40, "Select");
    let message = click(&mut modals, x, y, &mut registry, &mut themes, Some(&db));

    assert_eq!(themes.active_name(), wanted);
    assert_eq!(db.get_active_theme().expect("read"), Some(wanted));
    assert!(!modals.is_open(), "applying closes the picker");
    assert!(message.is_some_and(|m| m.contains("theme:")), "no report");
}

#[test]
fn clicking_cancel_closes_the_picker_and_keeps_the_theme() {
    let db = Database::open_in_memory().expect("in-memory database");
    let mut registry = Registry::default();
    let mut themes = Themes::load(Some(&db));
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Theme);
    let before = themes.active_name().to_string();

    send(
        &mut modals,
        press(KeyCode::Char('j')),
        "j",
        &mut registry,
        &mut themes,
        Some(&db),
    );
    let buffer = modal_buffer(&mut modals, &registry, &themes, 100, 40);
    let (x, y) = locate(&buffer, 100, 40, "Cancel");
    click(&mut modals, x, y, &mut registry, &mut themes, Some(&db));

    assert!(!modals.is_open(), "Cancel replays the Esc that closes");
    assert_eq!(themes.active_name(), before, "Cancel applied something");
}

#[test]
fn clicking_close_closes_help_and_settings() {
    let host = host();
    let mut registry = registry(&host);
    let mut themes = Themes::load(None);
    for kind in [ModalKind::Help, ModalKind::Settings] {
        let mut modals = Modals::default();
        modals.toggle(kind);
        let buffer = modal_buffer(&mut modals, &registry, &themes, 140, 40);
        let (x, y) = locate(&buffer, 140, 40, "Close");
        click(&mut modals, x, y, &mut registry, &mut themes, None);
        assert!(!modals.is_open(), "{kind:?} stayed open");
    }
}

#[test]
fn clicking_reset_all_resets_every_override() {
    // v1 spells this button `Shift+D`, and the click has to go through that
    // key — otherwise the pill and the letter could drift apart.
    let dir = tempfile::tempdir().expect("temp dir");
    let _guard = thurbox::paths::TestPathGuard::new(dir.path());
    let host = host();
    let mut registry = registry(&host);
    let mut themes = Themes::load(None);
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Help);

    let default_chord = registry.bindings()[0].default_chord.clone();
    for (key, chord) in [
        (press(KeyCode::Enter), "enter"),
        (press(KeyCode::F(9)), "f9"),
    ] {
        send(&mut modals, key, chord, &mut registry, &mut themes, None);
    }
    assert!(registry.bindings()[0].overridden, "nothing to reset");

    let buffer = modal_buffer(&mut modals, &registry, &themes, 140, 40);
    let (x, y) = locate(&buffer, 140, 40, "Reset all");
    let message = click(&mut modals, x, y, &mut registry, &mut themes, None);

    assert_eq!(registry.bindings()[0].chord, default_chord);
    assert!(
        !registry.bindings().iter().any(|binding| binding.overridden),
        "an override survived: {message:?}"
    );
    assert!(modals.is_open(), "resetting is not closing");
}

#[test]
fn clicking_a_row_selects_it_and_a_miss_is_swallowed() {
    let host = host();
    let mut registry = registry(&host);
    let mut themes = Themes::load(None);
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Theme);
    let buffer = modal_buffer(&mut modals, &registry, &themes, 100, 60);

    // A row still selects rather than applies, so a stray click cannot repaint
    // the whole interface.
    let (x, y) = locate(&buffer, 100, 60, "Gruvbox Dark");
    click(&mut modals, x, y, &mut registry, &mut themes, None);
    let after = modal_screen(&mut modals, &registry, &themes, 100, 60);
    assert!(
        after.contains("▸ Gruvbox Dark"),
        "the click did not move the cursor:\n{after}"
    );
    assert!(modals.is_open(), "a row click must not apply and close");

    // The mouse half of capturing input: a press outside every hitbox is eaten
    // rather than reaching the pane the modal covers.
    click(&mut modals, 0, 0, &mut registry, &mut themes, None);
    assert!(modals.is_open(), "a miss closed the modal");
}

#[test]
fn the_pill_under_the_pointer_lights_up() {
    let host = host();
    let registry = registry(&host);
    let themes = Themes::load(None);
    let palette = themes.active().palette.clone();
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Theme);

    let resting = modal_buffer(&mut modals, &registry, &themes, 100, 40);
    let (sx, sy) = locate(&resting, 100, 40, "Select");
    let (cx, cy) = locate(&resting, 100, 40, "Cancel");
    assert_eq!(
        resting[(sx, sy)].bg,
        palette.accent,
        "a resting primary pill is the accent badge"
    );

    assert!(modals.on_hover(sx, sy), "the pointer reached no modal");
    let hovered = modal_buffer(&mut modals, &registry, &themes, 100, 40);
    assert_eq!(
        hovered[(sx, sy)].bg,
        palette.accent_bright,
        "the hovered pill did not brighten"
    );
    assert_eq!(
        hovered[(sx, sy)].fg,
        palette.inverted_fg,
        "the hovered label must stay legible on the new fill"
    );
    assert_eq!(
        hovered[(cx, cy)].bg,
        palette.selection_bg,
        "only the pill under the pointer lights up"
    );
}

#[test]
fn the_row_under_the_pointer_takes_a_band() {
    let host = host();
    let registry = registry(&host);
    let themes = Themes::load(None);
    let palette = themes.active().palette.clone();
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Theme);

    let resting = modal_buffer(&mut modals, &registry, &themes, 100, 60);
    let (x, y) = locate(&resting, 100, 60, "Gruvbox Dark");
    assert_eq!(resting[(x, y)].bg, palette.modal_bg, "banded at rest");

    assert!(modals.on_hover(x, y));
    let hovered = modal_buffer(&mut modals, &registry, &themes, 100, 60);
    assert_eq!(
        hovered[(x, y)].bg,
        palette.selection_bg,
        "the hovered row took no band"
    );
    // The band is the width of the row, not of its label — v1 tints the whole
    // hitbox, which is what makes it read as a row rather than as text.
    assert_eq!(hovered[(x + 3, y)].bg, palette.selection_bg);
    assert_ne!(
        hovered[(x, y + 1)].bg,
        palette.selection_bg,
        "the band spilled onto the next row"
    );
}

#[test]
fn a_pointer_crossing_one_target_costs_a_single_repaint() {
    // Motion reporting delivers one event per cell crossed, so the modal must
    // answer for the *target* under the pointer rather than the cell — the same
    // rule `App::hover` follows for everything else.
    let host = host();
    let registry = registry(&host);
    let themes = Themes::load(None);
    let mut modals = Modals::default();
    modals.toggle(ModalKind::Theme);
    let buffer = modal_buffer(&mut modals, &registry, &themes, 100, 40);
    let (sx, sy) = locate(&buffer, 100, 40, "Select");
    let (cx, cy) = locate(&buffer, 100, 40, "Cancel");

    assert!(modals.on_hover(sx, sy), "arriving on a pill is a change");
    assert!(
        !modals.on_hover(sx + 1, sy),
        "moving within one pill repainted"
    );
    assert!(
        modals.on_hover(cx, cy),
        "moving to another pill is a change"
    );
    assert!(modals.on_hover(0, 0), "leaving every target is a change");
    assert!(!modals.on_hover(1, 0), "moving over dead space repainted");
}

#[test]
fn a_hover_never_outlives_the_modal_that_resolved_it() {
    let host = host();
    let registry = registry(&host);
    let themes = Themes::load(None);
    let mut modals = Modals::default();
    assert!(
        !modals.on_hover(4, 4),
        "a closed layer has nothing to hover"
    );

    modals.toggle(ModalKind::Theme);
    let buffer = modal_buffer(&mut modals, &registry, &themes, 140, 40);
    let (x, y) = locate(&buffer, 140, 40, "Select");
    assert!(modals.on_hover(x, y), "the picker's first pill");

    // Both modals' first pill is "button 0", so a resolution kept across the
    // switch would read as "already hovered" and the new pill would never
    // light. The index means nothing outside the modal that recorded it.
    modals.toggle(ModalKind::Help);
    let buffer = modal_buffer(&mut modals, &registry, &themes, 140, 40);
    let (x, y) = locate(&buffer, 140, 40, "Reset all");
    assert!(modals.on_hover(x, y), "the stale hover was kept");
}

#[test]
fn help_and_settings_page_and_jump_through_their_rows() {
    // Both are lists long enough to scroll, so they take v1's list keys. The
    // observable is what `d` reports resetting: it names the selected row.
    let dir = tempfile::tempdir().expect("temp dir");
    let _guard = thurbox::paths::TestPathGuard::new(dir.path());
    let host = host();
    let mut registry = registry(&host);
    // Paging needs more rows than a screen, and the bundled set declares one
    // setting. These stand in for the panes that used to contribute the rest —
    // the modal's job is to page whatever it is given, not to have a long list.
    let extra: Vec<thurbox::kernel::registry::Setting> = (0..40)
        .map(|n| thurbox::kernel::registry::Setting {
            plugin: "probe".to_string(),
            id: format!("flag_{n:02}"),
            description: format!("a stand-in setting ({n})"),
            default: thurbox::kernel::registry::Value::Bool(true),
            value: thurbox::kernel::registry::Value::Bool(true),
        })
        .collect();
    // `declare` REPLACES both lists, so the bundled declarations are re-passed
    // alongside the stand-ins — dropping them would leave help with no rows.
    let (bindings, mut settings) = host.declarations();
    settings.extend(extra);
    registry.declare(bindings, settings);
    let mut themes = Themes::load(None);

    for kind in [ModalKind::Help, ModalKind::Settings] {
        let mut modals = Modals::default();
        modals.toggle(kind);
        // The page step is a screenful of rows, which only exists once drawn.
        modal_screen(&mut modals, &registry, &themes, 140, 20);

        let selected = |code: KeyCode,
                        chord: &str,
                        modals: &mut Modals,
                        registry: &mut Registry,
                        themes: &mut Themes| {
            send(modals, press(code), chord, registry, themes, None);
            // `d` reports the row it reset, which is how a test outside the
            // modal can see where the cursor is.
            send(
                modals,
                press(KeyCode::Char('d')),
                "d",
                registry,
                themes,
                None,
            )
            .unwrap_or_default()
        };

        let top = selected(
            KeyCode::Home,
            "home",
            &mut modals,
            &mut registry,
            &mut themes,
        );
        let paged = selected(
            KeyCode::PageDown,
            "pagedown",
            &mut modals,
            &mut registry,
            &mut themes,
        );
        let bottom = selected(KeyCode::End, "end", &mut modals, &mut registry, &mut themes);
        let back = selected(
            KeyCode::PageUp,
            "pageup",
            &mut modals,
            &mut registry,
            &mut themes,
        );
        let home_again = selected(
            KeyCode::Home,
            "home",
            &mut modals,
            &mut registry,
            &mut themes,
        );

        assert_ne!(top, "", "{kind:?}: nothing reported a selection");
        assert_ne!(top, paged, "{kind:?}: PageDown moved nothing");
        assert_ne!(top, bottom, "{kind:?}: End moved nothing");
        assert_ne!(bottom, back, "{kind:?}: PageUp moved nothing");
        assert_eq!(top, home_again, "{kind:?}: Home is not the top");
    }
}

/// The keypress a chord names, for `Registry::resolve`.
fn key_press(chord: &str) -> KeyPress {
    let mut press = KeyPress::default();
    for part in chord.split('+') {
        match part {
            "ctrl" => press.ctrl = true,
            "alt" => press.alt = true,
            "shift" => press.shift = true,
            name => {
                press.name = name.to_string();
                press.ch = (name.chars().count() == 1).then(|| name.chars().next().unwrap());
            }
        }
    }
    press
}

#[test]
fn a_modal_owns_the_caret_so_none_is_left_blinking_behind_it() {
    // The bug: a frame either shows the cursor at a position or hides it (see
    // ratatui's `apply_buffer_with_cursor`), and a modal never sets one. The
    // only thing in this interface that does is an `input` node — the search
    // strip's query, the creation flow's fields. So a pane underneath that claimed the caret — the
    // search strip, the creation flow — kept it, and the terminal drew a
    // blinking cursor behind the modal. It reads as the screen refreshing
    // wrongly rather than as a cursor in the wrong place, which is why it was
    // reported as a refresh-rate problem.
    //
    // The correction is made after the frame, because a `Frame`'s cursor can be
    // set but never unset. What is asserted here is the decision that drives it.
    let mut modals = Modals::default();
    assert_eq!(
        modals.caret(),
        None,
        "no modal paints its own caret yet, so the answer must be 'hide it'"
    );
    modals.toggle(ModalKind::Settings);
    assert!(modals.is_open());
    assert_eq!(
        modals.caret(),
        None,
        "an open modal with no field of its own must still claim the caret \
         (as absent), or the pane underneath keeps it"
    );
}
