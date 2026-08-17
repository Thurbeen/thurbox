//! Adding, replacing and removing parts of the interface.
//!
//! Every one of these already worked, or nearly did, and none of them was held
//! by anything: the bundled set is changing pane by pane under `v2-parity-gaps`,
//! so "you can drop a file in" is exactly the kind of promise that regresses
//! without anyone noticing until a user's plugin stops loading.
//!
//! The interface is built in a temporary directory the same way the binary
//! builds the user's, so these exercise delivery and loading together.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use thurbox::kernel::bundled::{self, Source};
use thurbox::kernel::host::LuaHost;
use thurbox::kernel::inventory::{self, State};
use thurbox::kernel::layout::resolve;
use thurbox::kernel::registry::Registry;

/// A pane that draws nothing, in as few lines as a plugin can be written.
fn pane(name: &str, slot: &str) -> String {
    format!(
        "return {{ name = \"{name}\", slot = \"{slot}\", focusable = true,\n\
         render = function() return {{ type = \"text\", text = \"{name}\" }} end }}"
    )
}

fn interface() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let report = bundled::materialize(dir.path());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    dir
}

fn loaded(dir: &Path) -> LuaHost {
    let host = LuaHost::new(dir);
    assert!(
        host.error.is_none(),
        "the interface must load: {:?}",
        host.error
    );
    host
}

fn placed(host: &LuaHost, width: u16, height: u16) -> HashSet<String> {
    let region = host.arrangement(width, height).expect("arrangement");
    resolve(&region, Rect::new(0, 0, width, height))
        .into_iter()
        .map(|slot| slot.slot)
        .collect()
}

fn sources(dir: &Path) -> BTreeMap<String, Source> {
    bundled::sources(dir)
}

#[test]
fn the_inventory_view_draws_what_the_kernel_computed() {
    // The view of the interface's own files is a tab of the settings modal, not
    // a pane, so this drives it the way the loop does: build the rows, hand them
    // to the modal, and read the cells it painted.
    let dir = interface();
    let host = loaded(dir.path());
    let rows = inventory::rows(
        &host.plugins,
        &sources(dir.path()),
        &HashSet::new(),
        &HashSet::new(),
        None,
        &|_| inventory::Trust::NotAsked,
        &|_| false,
    );

    let mut modal = thurbox::kernel::modals::settings::SettingsModal::default();
    // `]` moves to the Interface tab; the settings half is the one that opens.
    modal.on_key(
        &KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE),
        &mut Registry::default(),
        &Default::default(),
        &rows,
    );

    let palette = thurbox::session::theme_config::ThemePreset::Default.palette();
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 26)).expect("terminal");
    terminal
        .draw(|frame| {
            modal.render(
                frame,
                frame.area(),
                &Registry::default(),
                &Default::default(),
                thurbox::kernel::modals::interface::Files {
                    rows: &rows,
                    dir: "/home/me/.config/thurbox/ui",
                },
                thurbox::kernel::modals::chrome::Chrome::new(&palette),
            );
        })
        .expect("draw");
    let screen = screen_text(terminal.backend());

    assert!(
        screen.contains("10_sessions.lua"),
        "every file is listed: {screen}"
    );
    assert!(
        screen.contains("theme.lua"),
        "modules too, since they are restorable: {screen}"
    );
    assert!(
        screen.contains("thurbox/ui"),
        "the directory in use is named: {screen}"
    );
    assert!(screen.contains("restore"), "the keys are offered: {screen}");
}

/// Every cell of a rendered frame as one string.
fn screen_text(backend: &ratatui::backend::TestBackend) -> String {
    backend
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn a_pane_is_added_by_adding_a_file() {
    let dir = interface();
    fs::write(
        dir.path().join("plugins/50_mine.lua"),
        pane("mine", "sessions"),
    )
    .expect("write");

    let host = loaded(dir.path());
    let mine = host
        .plugins
        .iter()
        .find(|plugin| plugin.name == "mine")
        .expect("the added pane loads");
    assert_eq!(mine.path, "plugins/50_mine.lua");
    assert!(host
        .focusable()
        .iter()
        .any(|index| host.plugins[*index].name == "mine"));

    // And delivery has no opinion about a file it did not write.
    let report = bundled::materialize(dir.path());
    assert!(report.is_empty(), "{report:?}");
    assert_eq!(sources(dir.path())["plugins/50_mine.lua"], Source::User);
}

#[test]
fn a_decorator_and_a_module_are_added_the_same_way() {
    let dir = interface();
    fs::write(
        dir.path().join("lib/mine.lua"),
        "return { badge = \"*\" }\n",
    )
    .expect("write");
    fs::write(
        dir.path().join("plugins/51_decor.lua"),
        "local mine = require(\"lib.mine\")\n\
         return { name = \"decor\", decorates = \"sessions\",\n\
         decorate = function(tree) tree.badge = mine.badge return tree end }\n",
    )
    .expect("write");

    let host = loaded(dir.path());
    let decor = host
        .plugins
        .iter()
        .find(|plugin| plugin.name == "decor")
        .expect("the decorator loads");
    assert_eq!(decor.decorates.as_deref(), Some("sessions"));
    // It draws into another pane's tree, so it is nobody's slot occupant — the
    // default slot must not make it compete with the pane it decorates.
    assert!(!host
        .in_slot("center")
        .iter()
        .any(|index| host.plugins[*index].name == "decor"));

    let report = bundled::materialize(dir.path());
    assert!(report.is_empty(), "{report:?}");
    assert!(dir.path().join("lib/mine.lua").exists());
}

#[test]
fn a_bundled_pane_is_replaced_by_a_differently_named_file() {
    let dir = interface();
    bundled::remove(dir.path(), "plugins/10_sessions.lua").expect("remove");
    fs::write(
        dir.path().join("plugins/15_my_sessions.lua"),
        pane("sessions", "sessions"),
    )
    .expect("write");

    // Delivery runs on every start, and this is the start that used to undo it.
    bundled::materialize(dir.path());
    let host = loaded(dir.path());

    let occupants: Vec<&str> = host
        .plugins
        .iter()
        .filter(|plugin| plugin.slot == "sessions")
        .map(|plugin| plugin.path.as_str())
        .collect();
    assert_eq!(occupants, vec!["plugins/15_my_sessions.lua"]);
}

#[test]
fn the_arrangement_decides_which_panes_have_a_rect() {
    let dir = interface();
    fs::write(
        dir.path().join("plugins/52_notes.lua"),
        pane("notes", "notes"),
    )
    .expect("write");

    // The stock arrangement has no `notes` region, so the pane loads and is
    // simply not placed — the silent drop this inventory exists to name.
    let host = loaded(dir.path());
    assert!(host.plugins.iter().any(|plugin| plugin.slot == "notes"));
    assert!(!placed(&host, 120, 40).contains("notes"));

    let rows = inventory::rows(
        &host.plugins,
        &sources(dir.path()),
        &HashSet::new(),
        &placed(&host, 120, 40),
        None,
        &|_| inventory::Trust::NotAsked,
        &|_| false,
    );
    let notes = rows
        .iter()
        .find(|row| row.path == "plugins/52_notes.lua")
        .expect("listed");
    assert_eq!(notes.state, State::Unplaced);

    // Give it a region and it has one.
    fs::write(
        dir.path().join("layout.lua"),
        "return function()\n\
         return { axis = \"horizontal\", children = { { slot = \"notes\", pct = 30 }, { slot = \"center\" } } }\n\
         end\n",
    )
    .expect("write");
    let host = loaded(dir.path());
    assert!(placed(&host, 120, 40).contains("notes"));
}

#[test]
fn removing_every_pane_is_an_empty_interface_rather_than_a_fault() {
    // The failure this guards: an empty plugin set used to be an error, the
    // error summoned the recovery floor, and the floor delivered the bundled
    // interface again — undoing the removal it was asked to make.
    let dir = interface();
    for (relative, _) in bundled::BUNDLED {
        if relative.starts_with("plugins/") {
            bundled::remove(dir.path(), relative).expect("remove");
        }
    }
    bundled::materialize(dir.path());

    let host = loaded(dir.path());
    assert!(host.plugins.is_empty(), "{:?}", host.error);
    assert!(host.error.is_none());

    // And every one of them is still listed, so they can be brought back.
    let rows = inventory::rows(
        &host.plugins,
        &sources(dir.path()),
        &HashSet::new(),
        &HashSet::new(),
        None,
        &|_| inventory::Trust::NotAsked,
        &|_| false,
    );
    assert!(rows
        .iter()
        .any(|row| row.path == "plugins/10_sessions.lua" && row.state == State::Removed));
}

#[test]
fn the_view_that_lists_the_files_is_not_one_of_them() {
    // It used to be a pane, and being a pane made it removable — which is a fine
    // property for a pane and a fatal one for the view you would use to undo a
    // removal. It is chrome now (`kernel::modals::interface`), so no file in the
    // interface can take it away.
    let dir = interface();
    let host = loaded(dir.path());
    assert!(
        !host.plugins.iter().any(|plugin| plugin.name == "plugins"),
        "the file list is a settings tab, not a plugin"
    );

    // Removing every pane there is must still leave it reachable, since it is
    // the only way back from exactly that state.
    for path in ["plugins/10_sessions.lua", "plugins/20_agent.lua"] {
        bundled::remove(dir.path(), path).expect("remove");
    }
    bundled::materialize(dir.path());
    let rows = bundled::sources(dir.path());
    assert!(rows.len() > 1, "the inventory the tab lists still has rows");
}

#[test]
fn restoring_brings_a_removed_pane_back() {
    let dir = interface();
    bundled::remove(dir.path(), "plugins/20_agent.lua").expect("remove");
    bundled::materialize(dir.path());
    assert!(!loaded(dir.path())
        .plugins
        .iter()
        .any(|plugin| plugin.path == "plugins/20_agent.lua"));

    bundled::restore(dir.path(), "plugins/20_agent.lua").expect("restore");
    let host = loaded(dir.path());
    assert!(host
        .plugins
        .iter()
        .any(|plugin| plugin.path == "plugins/20_agent.lua"));
    assert_eq!(
        sources(dir.path())["plugins/20_agent.lua"],
        Source::Bundled,
        "restored means shipped, not edited"
    );
}

/// The recovery floor is a state, not a one-way door.
///
/// A user copy that will not load is swapped for the bundled interface so thurbox
/// stays usable while the file is fixed from inside it. But a `LuaHost` reloads
/// from the directory it was *built* from, so once the floor was installed every
/// later reload rebuilt the bundled copy: the watcher fired on the user's fix,
/// nothing changed, and no error said why — while the watcher, the inventory and
/// every Interface-tab action still pointed at the user's own directory. Only a
/// restart recovered, and a `./ui` checkout took the same path, so it cost a
/// plugin author their edit-reload loop too.
#[test]
fn a_broken_interface_loads_again_once_it_is_fixed() {
    let dir = interface();
    let broken = dir.path().join("plugins/50_broken.lua");
    fs::write(&broken, "this is not lua(((").expect("write");

    let mut host = LuaHost::new(dir.path());
    assert!(host.error.is_some(), "a syntax error must fail the build");

    // What the binary does while it cannot load the user's copy: run the bundled
    // interface instead, from somewhere else entirely.
    let floor = bundled::fallback_dir().expect("fallback");
    let mut running = LuaHost::new(&floor);
    assert!(running.error.is_none(), "{:?}", running.error);

    // Now the user fixes their file. Reloading from the directory the host was
    // built from — the floor — can never notice.
    fs::write(&broken, pane("fixed", "center")).expect("fix");
    running.reload();
    assert!(
        running.index_of("fixed").is_none(),
        "the floor rebuilds itself; this is why the reload has to be re-pointed"
    );

    // Re-pointed at the authoritative directory, it picks the fix up.
    running.reload_from(dir.path());
    assert!(running.error.is_none(), "{:?}", running.error);
    assert!(
        running.index_of("fixed").is_some(),
        "a fixed interface must come back without a restart"
    );

    // And the plain path still works: a host on the user's directory reloads it.
    host.reload();
    assert!(host.error.is_none(), "{:?}", host.error);

    let _ = fs::remove_dir_all(&floor);
}

/// Turning a pane off must not leave its column reserved and empty.
///
/// The interface's central promise is that the arrangement closes up around what
/// is left, and it did not: `ui/layout.lua` gated the session column on the F9
/// panel toggle alone, which is arrangement state, and knew nothing about the
/// disabled set, which is delivery state. Disabling `10_sessions.lua` therefore
/// produced a quarter-width rect with nothing in it — reproduced by a UI review
/// and visible in the shipped demo recording.
#[test]
fn a_disabled_pane_does_not_reserve_its_column() {
    let dir = interface();
    let mut host = loaded(dir.path());

    // Wide enough for the two-column arrangement, so the column is there to lose.
    let before = placed(&host, 150, 40);
    assert!(
        before.contains("sessions"),
        "the session column is placed while its plugin loads: {before:?}"
    );
    assert!(before.contains("center"), "{before:?}");

    host.set_disabled(vec!["plugins/10_sessions.lua".to_string()]);
    host.reload();
    assert!(host.error.is_none(), "{:?}", host.error);
    assert!(
        !host
            .plugins
            .iter()
            .any(|p| p.path == "plugins/10_sessions.lua"),
        "a disabled file is not loaded at all"
    );

    let after = placed(&host, 150, 40);
    assert!(
        !after.contains("sessions"),
        "no plugin fills `sessions`, so no rect may be reserved for it: {after:?}"
    );
    assert!(
        after.contains("center"),
        "the centre pane is never dropped: {after:?}"
    );

    // The point of not reserving it: the space goes to the pane that is left.
    let full = resolve(
        &host.arrangement(150, 40).expect("arrangement"),
        Rect::new(0, 0, 150, 40),
    );
    let center = full
        .iter()
        .find(|slot| slot.slot == "center")
        .expect("center is placed");
    assert_eq!(
        center.rect.width, 150,
        "the centre takes the whole width once nothing else claims any"
    );
    assert_eq!(center.rect.x, 0, "and starts at the left edge");
}

/// A float claiming a slot must not make the arrangement reserve one.
///
/// A float draws *above* the arrangement, so it never fills a rect carved out of
/// the screen. Counting one as an occupant would put back exactly the bug this
/// pair of tests exists for: a reserved column with nothing in it.
#[test]
fn a_float_does_not_make_its_slot_count_as_occupied() {
    let dir = interface();
    // Replace the session list with a FLOAT that claims the same slot, so the
    // slot is claimed but nothing will ever paint into the column.
    fs::write(
        dir.path().join("plugins/10_sessions.lua"),
        "return { name = \"floaty\", slot = \"sessions\", floats = true,\n\
         render = function() return { type = \"text\", text = \"x\" } end }",
    )
    .expect("write");
    let host = loaded(dir.path());
    assert!(
        host.plugins.iter().any(|p| p.name == "floaty"),
        "the float loaded"
    );

    let slots = host.occupied_slots();
    assert!(
        !slots.contains("sessions"),
        "a float occupies no slot: {slots:?}"
    );
    let placed = placed(&host, 150, 40);
    assert!(
        !placed.contains("sessions"),
        "so its column is not reserved: {placed:?}"
    );
}

/// A float that painted nothing must not be reported as being on screen.
///
/// The inventory exists to answer "which of these files is drawing", and for the
/// two floats — the confirmation dialog and the creation wizard — it answered
/// `on screen` permanently, because `focus::is_drawn` returned true for anything
/// *allowed* to float rather than for what had floated. Being the screen a user
/// reaches for when the interface looks wrong, that is the worst place for it.
#[test]
fn a_float_drawing_nothing_is_reported_on_demand_rather_than_on_screen() {
    let dir = interface();
    let host = loaded(dir.path());

    // No float painted, which is the state at rest: nothing in `visible`.
    let rows = inventory::rows(
        &host.plugins,
        &sources(dir.path()),
        &HashSet::new(),
        &["sessions".to_string(), "center".to_string()]
            .into_iter()
            .collect(),
        None,
        &|_| inventory::Trust::NotAsked,
        &|_| false,
    );
    let state_of = |path: &str| {
        rows.iter()
            .find(|row| row.path == path)
            .unwrap_or_else(|| panic!("{path} is listed"))
            .state
    };

    for float in ["plugins/60_confirm.lua", "plugins/70_new_session.lua"] {
        assert_eq!(
            state_of(float),
            State::OnDemand,
            "{float} paints nothing until invoked"
        );
        assert_ne!(state_of(float), State::Visible, "{float}");
    }

    // The distinction is the point: `Hidden` still means a slot held by someone
    // else or a closed column, which is a different thing from a modal at rest.
    assert!(
        thurbox::kernel::focus::is_drawn(thurbox::kernel::focus::Placement {
            floats: true,
            float_open: true,
            slot_placed: false,
            chosen_in_switch: None,
        }),
        "an open float is on screen"
    );
}
