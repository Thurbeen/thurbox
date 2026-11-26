use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use thurbox::kernel::bundled::{self, Source};
use thurbox::kernel::host::LuaHost;
use thurbox::kernel::inventory::{self, State};
use thurbox::kernel::layout::{divide_slot, resolve};
use thurbox::kernel::node::Axis;
use thurbox::kernel::registry::Registry;

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
    let dir = interface();
    let host = loaded(dir.path());
    let rows = inventory::rows(
        &host.mods,
        &sources(dir.path()),
        &HashSet::new(),
        &HashSet::new(),
        None,
        &|_| inventory::Trust::NotAsked,
        &|_| false,
    );

    let mut modal = thurbox::kernel::modals::settings::SettingsModal::default();
    modal.on_key(
        &KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE),
        &mut Registry::default(),
        &Default::default(),
        &rows,
    );

    let palette = thurbox::theme::ThemePreset::Default.palette();
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
        !screen.contains("lib/chrome.lua"),
        "untouched modules wait for `a`: {screen}"
    );
    assert!(screen.contains("all files"), "and `a` is offered: {screen}");
    assert!(
        screen.contains("thurbox/ui"),
        "the directory in use is named: {screen}"
    );
    assert!(
        screen.contains("turn off"),
        "the keys are offered: {screen}"
    );
}

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
        dir.path().join("mods/50_mine.lua"),
        pane("mine", "sessions"),
    )
    .expect("write");

    let host = loaded(dir.path());
    let mine = host
        .mods
        .iter()
        .find(|loaded| loaded.name == "mine")
        .expect("the added pane loads");
    assert_eq!(mine.path, "mods/50_mine.lua");
    assert!(host
        .focusable()
        .iter()
        .any(|index| host.mods[*index].name == "mine"));

    let report = bundled::materialize(dir.path());
    assert!(report.is_empty(), "{report:?}");
    assert_eq!(sources(dir.path())["mods/50_mine.lua"], Source::User);
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
        dir.path().join("mods/51_decor.lua"),
        "local mine = require(\"lib.mine\")\n\
         return { name = \"decor\", decorates = \"sessions\",\n\
         decorate = function(tree) tree.badge = mine.badge return tree end }\n",
    )
    .expect("write");

    let host = loaded(dir.path());
    let decor = host
        .mods
        .iter()
        .find(|loaded| loaded.name == "decor")
        .expect("the decorator loads");
    assert_eq!(decor.decorates.as_deref(), Some("sessions"));
    assert!(
        !host
            .in_slot("center")
            .iter()
            .any(|index| host.mods[*index].name == "decor"),
        "a decorator draws into another pane's tree, so it occupies no slot"
    );

    let report = bundled::materialize(dir.path());
    assert!(report.is_empty(), "{report:?}");
    assert!(dir.path().join("lib/mine.lua").exists());
}

#[test]
fn a_bundled_pane_is_replaced_by_a_differently_named_file() {
    let dir = interface();
    bundled::remove(dir.path(), "mods/10_sessions.lua").expect("remove");
    fs::write(
        dir.path().join("mods/15_my_sessions.lua"),
        pane("sessions", "sessions"),
    )
    .expect("write");

    bundled::materialize(dir.path());
    let host = loaded(dir.path());

    let occupants: Vec<&str> = host
        .mods
        .iter()
        .filter(|loaded| loaded.slot == "sessions")
        .map(|loaded| loaded.path.as_str())
        .collect();
    assert_eq!(occupants, vec!["mods/15_my_sessions.lua"]);
}

#[test]
fn the_arrangement_decides_which_panes_have_a_rect() {
    let dir = interface();
    fs::write(dir.path().join("mods/52_notes.lua"), pane("notes", "notes")).expect("write");

    let host = loaded(dir.path());
    assert!(host.mods.iter().any(|loaded| loaded.slot == "notes"));
    assert!(
        !placed(&host, 120, 40).contains("notes"),
        "the stock arrangement has no `notes` region"
    );

    let rows = inventory::rows(
        &host.mods,
        &sources(dir.path()),
        &HashSet::new(),
        &placed(&host, 120, 40),
        None,
        &|_| inventory::Trust::NotAsked,
        &|_| false,
    );
    let notes = rows
        .iter()
        .find(|row| row.path == "mods/52_notes.lua")
        .expect("listed");
    assert_eq!(notes.state, State::Unplaced);

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
    let dir = interface();
    for (relative, _) in bundled::BUNDLED {
        if relative.starts_with("mods/") {
            bundled::remove(dir.path(), relative).expect("remove");
        }
    }
    bundled::materialize(dir.path());

    let host = loaded(dir.path());
    assert!(host.mods.is_empty(), "{:?}", host.error);
    assert!(host.error.is_none());

    let rows = inventory::rows(
        &host.mods,
        &sources(dir.path()),
        &HashSet::new(),
        &HashSet::new(),
        None,
        &|_| inventory::Trust::NotAsked,
        &|_| false,
    );
    assert!(
        rows.iter()
            .any(|row| row.path == "mods/10_sessions.lua" && row.state == State::Removed),
        "a removed pane stays listed so it can be brought back"
    );
}

#[test]
fn the_view_that_lists_the_files_is_not_one_of_them() {
    let dir = interface();
    let host = loaded(dir.path());
    assert!(
        !host.mods.iter().any(|loaded| loaded.name == "plugins"),
        "the file list is a settings tab, not a plugin"
    );

    for path in ["mods/10_sessions.lua", "mods/20_agent.lua"] {
        bundled::remove(dir.path(), path).expect("remove");
    }
    bundled::materialize(dir.path());
    let rows = bundled::sources(dir.path());
    assert!(rows.len() > 1, "the inventory the tab lists still has rows");
}

#[test]
fn restoring_brings_a_removed_pane_back() {
    let dir = interface();
    bundled::remove(dir.path(), "mods/20_agent.lua").expect("remove");
    bundled::materialize(dir.path());
    assert!(!loaded(dir.path())
        .mods
        .iter()
        .any(|loaded| loaded.path == "mods/20_agent.lua"));

    bundled::restore(dir.path(), "mods/20_agent.lua").expect("restore");
    let host = loaded(dir.path());
    assert!(host
        .mods
        .iter()
        .any(|loaded| loaded.path == "mods/20_agent.lua"));
    assert_eq!(
        sources(dir.path())["mods/20_agent.lua"],
        Source::Bundled,
        "restored means shipped, not edited"
    );
}

#[test]
fn a_broken_interface_loads_again_once_it_is_fixed() {
    let dir = interface();
    let broken = dir.path().join("mods/50_broken.lua");
    fs::write(&broken, "this is not lua(((").expect("write");

    let mut host = LuaHost::new(dir.path());
    assert!(host.error.is_some(), "a syntax error must fail the build");

    let floor = bundled::fallback_dir().expect("fallback");
    let mut running = LuaHost::new(&floor);
    assert!(running.error.is_none(), "{:?}", running.error);

    fs::write(&broken, pane("fixed", "center")).expect("fix");
    running.reload();
    assert!(
        running.index_of("fixed").is_none(),
        "the floor rebuilds itself; this is why the reload has to be re-pointed"
    );

    running.reload_from(dir.path());
    assert!(running.error.is_none(), "{:?}", running.error);
    assert!(
        running.index_of("fixed").is_some(),
        "a fixed interface must come back without a restart"
    );

    host.reload();
    assert!(host.error.is_none(), "{:?}", host.error);

    let _ = fs::remove_dir_all(&floor);
}

#[test]
fn a_disabled_pane_does_not_reserve_its_column() {
    let dir = interface();
    let mut host = loaded(dir.path());

    let before = placed(&host, 150, 40);
    assert!(
        before.contains("sessions"),
        "the session column is placed while its plugin loads: {before:?}"
    );
    assert!(before.contains("center"), "{before:?}");

    host.set_disabled(vec!["mods/10_sessions.lua".to_string()]);
    host.reload();
    assert!(host.error.is_none(), "{:?}", host.error);
    assert!(
        !host.mods.iter().any(|p| p.path == "mods/10_sessions.lua"),
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

#[test]
fn a_float_does_not_make_its_slot_count_as_occupied() {
    let dir = interface();
    fs::write(
        dir.path().join("mods/10_sessions.lua"),
        "return { name = \"floaty\", slot = \"sessions\", floats = true,\n\
         render = function() return { type = \"text\", text = \"x\" } end }",
    )
    .expect("write");
    let host = loaded(dir.path());
    assert!(
        host.mods.iter().any(|p| p.name == "floaty"),
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

#[test]
fn a_float_drawing_nothing_is_reported_on_demand_rather_than_on_screen() {
    let dir = interface();
    let host = loaded(dir.path());

    let rows = inventory::rows(
        &host.mods,
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

    for float in [
        "mods/60_confirm.lua",
        "mods/62_rename.lua",
        "mods/64_menu.lua",
        "mods/70_new_session.lua",
    ] {
        assert_eq!(
            state_of(float),
            State::OnDemand,
            "{float} paints nothing until invoked"
        );
        assert_ne!(state_of(float), State::Visible, "{float}");
    }

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

#[test]
fn the_interface_ships_guidance_and_lists_it_as_a_doc() {
    let dir = interface();
    let readme = dir.path().join("README.md");
    assert!(readme.is_file(), "delivery writes it like any other file");
    let text = fs::read_to_string(&readme).expect("read");
    assert!(
        text.contains("mod check"),
        "it points at the command that catches a broken edit"
    );

    let host = loaded(dir.path());
    let rows = inventory::rows(
        &host.mods,
        &sources(dir.path()),
        &HashSet::new(),
        &HashSet::new(),
        None,
        &|_| inventory::Trust::NotAsked,
        &|_| false,
    );
    let row = rows
        .iter()
        .find(|row| row.path == "README.md")
        .expect("a shipped file the inventory cannot see is one it cannot restore");
    assert_eq!(row.kind, inventory::Kind::Doc);
    assert_eq!(
        row.source,
        Source::Bundled,
        "unedited, so delivery owns it and may update it"
    );
    assert_eq!(row.state, State::Present, "prose does not draw");

    fs::write(&readme, "# mine\n").expect("edit");
    assert_eq!(sources(dir.path())["README.md"], Source::Edited);
}

#[test]
fn the_demo_examples_load_and_stack_where_the_layout_says() {
    let dir = interface();
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    fs::copy(
        root.join("examples/lua/layout.lua"),
        dir.path().join("layout.lua"),
    )
    .expect("layout");
    for (from, to) in [
        ("examples/panes/tasks/tasks.lua", "mods/80_tasks.lua"),
        ("examples/panes/top/top.lua", "mods/85_top.lua"),
    ] {
        fs::copy(root.join(from), dir.path().join(to)).expect(from);
    }

    let host = loaded(dir.path());
    for name in ["tasks", "top"] {
        assert!(
            host.mods.iter().any(|p| p.name == name),
            "{name} must load: {:?}",
            host.error
        );
    }
    let top = host
        .mods
        .iter()
        .find(|p| p.name == "top")
        .expect("top loaded");
    assert!(
        top.capabilities
            .contains(&thurbox::kernel::host::Capability::Run),
        "the example declares `run`, so the trust prompt has something to be about"
    );

    let wide = resolve(
        &host.arrangement(200, 40).expect("arrangement"),
        Rect::new(0, 0, 200, 40),
    );
    let rect = |slot: &str| {
        wide.iter()
            .find(|placed| placed.slot == slot)
            .map(|placed| placed.rect)
            .unwrap_or_else(|| panic!("{slot} is not placed: {wide:?}"))
    };
    let (sessions, center, tasks, top_rect) =
        (rect("sessions"), rect("center"), rect("tasks"), rect("top"));

    assert!(sessions.x < center.x, "{sessions:?} {center:?}");
    assert!(
        center.x + center.width <= tasks.x,
        "the stack is to the RIGHT of the centre: {center:?} {tasks:?}"
    );
    assert_eq!(tasks.x, top_rect.x, "one column");
    assert_eq!(tasks.width, top_rect.width, "one width");
    assert_eq!(
        tasks.y + tasks.height,
        top_rect.y,
        "tasks sits directly on top of top: {tasks:?} {top_rect:?}"
    );

    let narrow = resolve(
        &host.arrangement(120, 40).expect("arrangement"),
        Rect::new(0, 0, 120, 40),
    );
    let slots: Vec<&str> = narrow.iter().map(|placed| placed.slot.as_str()).collect();
    assert!(!slots.contains(&"tasks"), "{slots:?}");
    assert!(!slots.contains(&"top"), "{slots:?}");
    assert!(
        slots.contains(&"center"),
        "the centre is never dropped: {slots:?}"
    );
}

#[test]
fn the_demo_layout_drops_a_stack_slot_whose_mod_is_missing() {
    let dir = interface();
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    fs::copy(
        root.join("examples/lua/layout.lua"),
        dir.path().join("layout.lua"),
    )
    .expect("layout");
    fs::copy(
        root.join("examples/panes/tasks/tasks.lua"),
        dir.path().join("mods/80_tasks.lua"),
    )
    .expect("tasks");

    let host = loaded(dir.path());
    let placed = placed(&host, 200, 40);
    assert!(placed.contains("tasks"), "{placed:?}");
    assert!(
        !placed.contains("top"),
        "no `top` plugin, so no rect reserved for one: {placed:?}"
    );
}

fn strip_mod(name: &str, slot: &str, extra: &str) -> String {
    format!(
        "return {{ name = \"{name}\", slot = \"{slot}\", strip = true, {extra}\n\
         render = function() return {{ type = \"text\", text = \"{name}\" }} end }}"
    )
}

fn rects(host: &LuaHost, width: u16, height: u16) -> Vec<(String, Rect)> {
    let region = host.arrangement(width, height).expect("arrangement");
    resolve(&region, Rect::new(0, 0, width, height))
        .into_iter()
        .map(|slot| (slot.slot, slot.rect))
        .collect()
}

fn rect_of(rects: &[(String, Rect)], slot: &str) -> Rect {
    rects
        .iter()
        .find(|(name, _)| name == slot)
        .unwrap_or_else(|| panic!("{slot} is not placed: {rects:?}"))
        .1
}

#[test]
fn a_strip_is_placed_above_the_bars_at_its_declared_height_with_no_layout_edit() {
    let dir = interface();
    fs::write(
        dir.path().join("mods/50_strip.lua"),
        strip_mod("dictation", "dictation", "size = { len = 2 },"),
    )
    .expect("write");
    let host = loaded(dir.path());

    let placed = rects(&host, 150, 40);
    let strip = rect_of(&placed, "dictation");
    let footer = rect_of(&placed, "footer");
    assert_eq!(strip.height, 2, "the height the pane declared");
    assert_eq!(strip.width, 150, "full width");
    assert_eq!(
        strip.y + strip.height,
        footer.y,
        "directly above the bars: {placed:?}"
    );
    assert!(
        rect_of(&placed, "center").y + rect_of(&placed, "center").height <= strip.y,
        "and below the panes, which give up the rows"
    );
}

#[test]
fn strips_stack_in_load_order_and_default_to_one_row() {
    let dir = interface();
    fs::write(
        dir.path().join("mods/50_first.lua"),
        strip_mod("first", "first", ""),
    )
    .expect("write");
    fs::write(
        dir.path().join("mods/51_second.lua"),
        strip_mod("second", "second", "size = { len = 3 },"),
    )
    .expect("write");
    let host = loaded(dir.path());

    let placed = rects(&host, 150, 40);
    let (first, second) = (rect_of(&placed, "first"), rect_of(&placed, "second"));
    assert_eq!(first.height, 1, "no size declared is one row");
    assert_eq!(second.height, 3);
    assert!(first.y < second.y, "load order, top to bottom: {placed:?}");
}

#[test]
fn a_float_is_not_a_strip() {
    let dir = interface();
    fs::write(
        dir.path().join("mods/50_strip.lua"),
        strip_mod("hover", "hover", "floats = true, size = { len = 2 },"),
    )
    .expect("write");
    let host = loaded(dir.path());
    let placed = placed(&host, 150, 40);
    assert!(
        !placed.contains("hover"),
        "a float reserves no strip: {placed:?}"
    );
}

fn pane_heights(host: &LuaHost, placed: &[(String, Rect)], slot: &str) -> Vec<u16> {
    let sizes: Vec<_> = host
        .in_slot(slot)
        .iter()
        .map(|index| host.mods[*index].size)
        .collect();
    divide_slot(rect_of(placed, slot), Axis::Vertical, &sizes, 0)
        .into_iter()
        .map(|rect| rect.height)
        .collect()
}

fn assert_no_blank_rows(placed: &[(String, Rect)], top: &str) {
    let center = rect_of(placed, "center");
    assert_eq!(
        center.y + center.height,
        rect_of(placed, top).y,
        "the panes end where the first strip starts: {placed:?}"
    );
}

#[test]
fn two_strips_sharing_a_stack_slot_get_one_rect_with_both_heights() {
    let dir = interface();
    fs::write(
        dir.path().join("mods/50_first.lua"),
        strip_mod("first", "ticker", "size = { len = 2 },"),
    )
    .expect("write");
    fs::write(
        dir.path().join("mods/51_second.lua"),
        strip_mod("second", "ticker", "size = { len = 3 },"),
    )
    .expect("write");
    let host = loaded(dir.path());

    let placed = rects(&host, 150, 40);
    assert_eq!(rect_of(&placed, "ticker").height, 5, "{placed:?}");
    assert_eq!(pane_heights(&host, &placed, "ticker"), vec![2, 3]);
    assert_eq!(
        rect_of(&placed, "ticker").y + 5,
        rect_of(&placed, "footer").y,
        "directly above the bars: {placed:?}"
    );
    assert_no_blank_rows(&placed, "ticker");
}

#[test]
fn two_strips_sharing_a_switch_slot_get_the_tallest_height() {
    let dir = interface();
    fs::write(
        dir.path().join("mods/50_first.lua"),
        strip_mod(
            "first",
            "ticker",
            "slot_mode = \"switch\", size = { len = 2 },",
        ),
    )
    .expect("write");
    fs::write(
        dir.path().join("mods/51_second.lua"),
        strip_mod("second", "ticker", "size = { len = 3 },"),
    )
    .expect("write");
    let host = loaded(dir.path());

    let placed = rects(&host, 150, 40);
    assert_eq!(rect_of(&placed, "ticker").height, 3, "{placed:?}");
    assert_no_blank_rows(&placed, "ticker");
}

#[test]
fn a_strip_placed_by_hand_is_not_placed_again_by_the_loop() {
    let dir = interface();
    fs::write(
        dir.path().join("mods/50_strip.lua"),
        strip_mod("ticker", "ticker", "size = { len = 2 },"),
    )
    .expect("write");
    let layout = dir.path().join("layout.lua");
    let shipped = fs::read_to_string(&layout).expect("read layout");
    let pinned = shipped.replacen(
        "local children = {}",
        "local children = { { slot = \"ticker\", len = 2 } }",
        1,
    );
    assert_ne!(
        pinned, shipped,
        "the shipped layout still builds `children`"
    );
    fs::write(&layout, pinned).expect("write layout");
    let host = loaded(dir.path());

    let placed = rects(&host, 150, 40);
    assert_eq!(rect_of(&placed, "ticker").y, 0, "where the user put it");
    let center = rect_of(&placed, "center");
    assert_eq!(
        center.y + center.height,
        rect_of(&placed, "footer").y,
        "no blank row above the bars: {placed:?}"
    );
}

#[test]
fn a_strip_placed_by_hand_below_the_loop_is_not_placed_again() {
    let dir = interface();
    fs::write(
        dir.path().join("mods/50_strip.lua"),
        strip_mod("ticker", "ticker", "size = { len = 2 },"),
    )
    .expect("write");
    let layout = dir.path().join("layout.lua");
    let shipped = fs::read_to_string(&layout).expect("read layout");
    let pinned = shipped.replacen(
        "  if height >= FOOTER_MIN_ROWS then",
        "  children[#children + 1] = { slot = \"ticker\", len = 2 }\n  \
         if height >= FOOTER_MIN_ROWS then",
        1,
    );
    assert_ne!(
        pinned, shipped,
        "the shipped layout still places the footer"
    );
    fs::write(&layout, pinned).expect("write layout");
    let host = loaded(dir.path());

    let placed = rects(&host, 150, 40);
    assert_eq!(
        rect_of(&placed, "ticker").y + 2,
        rect_of(&placed, "footer").y,
        "where the user put it: {placed:?}"
    );
    assert_no_blank_rows(&placed, "ticker");
}
