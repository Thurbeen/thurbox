//! The new-session flow, against the real bundled plugin.
//!
//! What is worth testing here is not the pixels, but the two things v1 got
//! wrong often enough to have grown machinery for:
//! **which step the flow is on** after each keystroke, and **what the create
//! command it finally issues actually carries**. Both are observable from
//! outside: the step from what is drawn, the command from the queue it is pushed
//! onto.
//!
//! The flow is driven exactly as the loop drives it — declared chords through
//! the registry to `on_action`, everything else to `on_key` — so a binding that
//! resolves to the wrong handler fails here rather than in the terminal.

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::Terminal;

use thurbox::kernel::command::{BookmarkEdit, Command, InFlight, Phase};
use thurbox::kernel::host::{Click, KeyPress, LuaHost, Published, RenderContext};
use thurbox::kernel::node::Identity;
use thurbox::kernel::paint::Hit;
use thurbox::kernel::registry::Registry;
use thurbox::kernel::repos::{BookmarkRow, BrowseEntry, Listing, RepoStore, Wants};
use thurbox::kernel::snapshot::{AgentRow, HostRow, Snapshot};
use thurbox::kernel::theme::Themes;

const PLUGIN: &str = "new_session";

fn host() -> LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

fn index_of(host: &LuaHost, name: &str) -> usize {
    host.index_of(name)
        .unwrap_or_else(|| panic!("no {name} plugin"))
}

/// Fleet's `local` host as the snapshot publishes it: this machine, with an
/// empty backend.
fn local_host() -> HostRow {
    HostRow {
        name: "local".into(),
        detail: "this machine".into(),
        ..HostRow::default()
    }
}

fn snapshot() -> Snapshot {
    Snapshot {
        hosts: Some(vec![local_host()]),
        agents: Some(vec![
            AgentRow {
                name: "claude".into(),
            },
            AgentRow {
                name: "codex".into(),
            },
        ]),
        ..Snapshot::default()
    }
}

/// Where an installed interface lives, as `offered` reports it.
const INTERFACE_DIR: &str = "/home/me/.config/thurbox/ui";

fn bookmark(path: &str, is_git: Option<bool>) -> BookmarkRow {
    BookmarkRow {
        name: path.rsplit('/').next().unwrap_or(path).to_string(),
        path: path.to_string(),
        parent: None,
        is_parent: false,
        is_git,
        label: None,
        offered: false,
    }
}

/// The interface directory, which the kernel offers at the top of the local list
/// under a name of its own rather than reading it from memory.
fn offered() -> BookmarkRow {
    BookmarkRow {
        label: Some("Thurbox interface — edit your panes".into()),
        offered: true,
        ..bookmark(INTERFACE_DIR, Some(false))
    }
}

/// A store holding the three reads, as if every worker had already answered.
fn store_with(rows: Vec<BookmarkRow>) -> RepoStore {
    let mut repos = RepoStore::new();
    repos.set_bookmarks_for_test("", rows);
    repos
}

/// A world whose remembered repositories are `rows`.
fn world_with(rows: Vec<BookmarkRow>) -> World {
    World {
        repos: store_with(rows),
        ..World::default()
    }
}

/// A folder of repositories: the header row plus one member under it.
fn folder_rows() -> Vec<BookmarkRow> {
    vec![
        BookmarkRow {
            path: "/src".into(),
            name: "src".into(),
            parent: None,
            is_parent: true,
            is_git: None,
            label: None,
            offered: false,
        },
        BookmarkRow {
            path: "/src/thurbox".into(),
            name: "thurbox".into(),
            parent: Some("/src".into()),
            is_parent: false,
            is_git: Some(true),
            label: None,
            offered: false,
        },
    ]
}

/// Everything the loop publishes, with the wants the flow is currently asking
/// for — which is what makes the three reads visible to it.
struct World {
    snapshot: Snapshot,
    repos: RepoStore,
    wants: Wants,
    inflight: Vec<InFlight>,
}

impl Default for World {
    fn default() -> Self {
        Self {
            snapshot: snapshot(),
            repos: store_with(vec![
                bookmark("/src/thurbox", Some(true)),
                bookmark("/src/notes", Some(false)),
            ]),
            wants: Wants {
                bookmarks: Some(String::new()),
                ..Default::default()
            },
            inflight: Vec::new(),
        }
    }
}

fn publish(host: &LuaHost, world: &World) {
    publish_hovered(host, world, None);
}

/// Publish with the pointer over `hovered`, the identity the kernel resolved
/// through the same hitboxes a click is routed by.
fn publish_hovered(host: &LuaHost, world: &World, hovered: Option<&Identity>) {
    let themes = Themes::load(None);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    host.publish(&Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
        snapshot: &world.snapshot,
        attach_errors: &Default::default(),
        inflight: &world.inflight,
        themes: &themes,
        registry: &registry,
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
        repos: &world.repos,
        wants: &world.wants,
        focus: None,
        selection: None,
        hovered,
        printing: &Default::default(),
    })
    .expect("publish");
}

/// What the flow draws, as text. Empty when it is closed — a closed modal draws
/// nothing at all, which is how the kernel knows it is not floating.
fn drawn(host: &LuaHost, world: &World) -> String {
    match painted(host, world, None) {
        Some((buffer, _)) => screen_of(&buffer),
        None => String::new(),
    }
}

/// The flow painted into its own rect, with the hitboxes the paint recorded —
/// what the loop routes a press through. `None` while the flow is closed.
fn painted(
    host: &LuaHost,
    world: &World,
    hovered: Option<&Identity>,
) -> Option<(Buffer, Vec<Hit>)> {
    publish_hovered(host, world, hovered);
    let index = index_of(host, PLUGIN);
    let rendered = host
        .render(
            index,
            RenderContext {
                width: 120,
                height: 40,
                focused: true,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render");
    let float = rendered.float?;
    // The flow asks in cells for both, and cells win over the percentage just
    // as they do in the kernel — a dump wider than the real modal hides every
    // truncation the user would see.
    let rows = float.rows.expect("the flow sizes its own height");
    let width = float
        .cols
        .unwrap_or((120.0 * float.width_pct / 100.0) as u16);
    let mut hits = Vec::new();
    let mut terminal = Terminal::new(TestBackend::new(width, rows)).expect("terminal");
    terminal
        .draw(|frame| {
            thurbox::kernel::paint::render_recording(
                frame,
                Rect::new(0, 0, width, rows),
                &rendered.node,
                &thurbox::kernel::terminal::Terminals::with_registry(
                    std::sync::Arc::new(
                        thurbox::backend::wiring::configured(&Default::default()).0,
                    ),
                    &Default::default(),
                ),
                &mut hits,
            );
        })
        .expect("draw");
    Some((terminal.backend().buffer().clone(), hits))
}

fn screen_of(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The cell where `needle` starts, counted in cells rather than bytes.
fn locate(buffer: &Buffer, needle: &str) -> (u16, u16) {
    let first = needle.chars().next().expect("a needle");
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let fits = needle.chars().enumerate().all(|(offset, ch)| {
                let at = x + offset as u16;
                at < buffer.area.width && buffer[(at, y)].symbol().starts_with(ch)
            });
            if buffer[(x, y)].symbol().starts_with(first) && fits {
                return (x, y);
            }
        }
    }
    panic!("{needle:?} is not on screen:\n{}", screen_of(buffer));
}

/// The identity the kernel resolves at a cell: the innermost hitbox, which is
/// the last one recorded — `App::target_at`'s rule.
fn identity_at(hits: &[Hit], x: u16, y: u16) -> Identity {
    let position = ratatui::layout::Position::new(x, y);
    hits.iter()
        .rev()
        .find(|hit| hit.rect.contains(position))
        .map(|hit| hit.identity.clone())
        .unwrap_or_default()
}

/// A left press on the cell where `needle` is drawn, resolved against the
/// paint's own hitboxes and delivered as the loop delivers it. `below` moves the
/// press that many rows down, for a field whose label is its frame's title.
fn click_on(host: &LuaHost, world: &World, needle: &str, below: u16) -> Identity {
    let (buffer, hits) = painted(host, world, None).expect("the flow is open");
    let (x, y) = locate(&buffer, needle);
    let identity = identity_at(&hits, x, y + below);
    let click = Click {
        id: identity.id.clone(),
        classes: identity.classes.clone(),
        role: identity.role.clone(),
        clicks: 1,
        ..Click::default()
    };
    host.on_click(index_of(host, PLUGIN), &click)
        .expect("on_click");
    identity
}

/// Press a key the way the loop does: a declared chord resolves through the
/// registry to `on_action`, and anything else goes to `on_key`.
fn press(host: &LuaHost, world: &World, chord: &str) {
    publish(host, world);
    let index = index_of(host, PLUGIN);
    let key = key_press(chord);
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

fn key_press(chord: &str) -> KeyPress {
    let mut key = KeyPress::default();
    let mut name = chord;
    loop {
        if let Some(rest) = name.strip_prefix("ctrl+") {
            key.ctrl = true;
            name = rest;
        } else if let Some(rest) = name.strip_prefix("alt+") {
            key.alt = true;
            name = rest;
        } else {
            break;
        }
    }
    key.name = name.to_string();
    if name.chars().count() == 1 {
        key.ch = name.chars().next();
    }
    if name == "space" {
        key.ch = Some(' ');
    }
    key
}

/// Type a word, character by character, as a terminal delivers it.
fn type_text(host: &LuaHost, world: &World, text: &str) {
    for ch in text.chars() {
        let mut key = KeyPress {
            name: ch.to_lowercase().to_string(),
            ch: Some(ch),
            ..KeyPress::default()
        };
        if ch == ' ' {
            key.name = "space".to_string();
        }
        publish(host, world);
        let index = index_of(host, PLUGIN);
        // Typed text is never a declared chord in a field: the flow returns
        // `false` from those actions while a field has focus, which is the
        // behaviour this exercises.
        let mut registry = Registry::default();
        let (bindings, settings) = host.declarations();
        registry.declare(bindings, settings);
        let consumed = match registry.resolve(&key, Some(PLUGIN)) {
            Some(binding) => {
                let action = binding.action.clone();
                host.on_action(index, &action).expect("on_action")
            }
            None => false,
        };
        if !consumed {
            host.on_key(index, &key).expect("on_key");
        }
    }
}

/// Open the flow and take the host step's first row, for tests of later steps.
fn open(host: &LuaHost, world: &World) {
    press(host, world, "ctrl+n");
    if drawn(host, world).contains("Run On") {
        press(host, world, "enter");
    }
}

fn devbox() -> HostRow {
    HostRow {
        name: "devbox".into(),
        detail: "me@devbox".into(),
        backend: "ssh:devbox".into(),
    }
}

// ── Opening and closing ────────────────────────────────────────────────────

#[test]
fn the_flow_draws_nothing_until_it_is_opened() {
    let host = host();
    let world = World::default();
    assert_eq!(
        drawn(&host, &world),
        "",
        "a closed flow must not float, or it would take every key"
    );
}

/// The host step is never skipped: this machine is a choice at once, and the
/// hosts Fleet names later join the same open form, so a form opened before
/// Fleet answers neither waits nor loses them.
#[test]
fn the_host_step_offers_this_machine_at_once_and_fleets_hosts_when_they_arrive() {
    let host = host();
    let mut world = World::default();
    press(&host, &world, "ctrl+n");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Run On"), "{screen}");
    assert!(screen.contains("this machine"), "{screen}");
    world.snapshot.hosts = Some(vec![local_host(), devbox()]);
    let screen = drawn(&host, &world);
    assert!(screen.contains("devbox"), "{screen}");
    press(&host, &world, "enter");
    assert!(drawn(&host, &world).contains("Select Repo"));
}

/// With no host list published at all, the step says it is asking rather than
/// offering nothing, and the hosts appear once they arrive.
#[test]
fn a_host_list_not_yet_published_shows_the_wait() {
    let host = host();
    let mut world = World::default();
    world.snapshot.hosts = None;
    press(&host, &world, "ctrl+n");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Asking Fleet for its hosts"), "{screen}");
    world.snapshot.hosts = Some(vec![local_host(), devbox()]);
    let screen = drawn(&host, &world);
    assert!(screen.contains("devbox"), "{screen}");
}

/// A Fleet that defines no `local` host offers none.
#[test]
fn local_is_offered_only_when_fleet_defines_it() {
    let host = host();
    let mut world = World::default();
    world.snapshot.hosts = Some(vec![devbox(), {
        let mut other = devbox();
        other.name = "build".into();
        other.backend = "build".into();
        other
    }]);
    press(&host, &world, "ctrl+n");
    let screen = drawn(&host, &world);
    assert!(screen.contains("devbox"), "{screen}");
    assert!(!screen.contains("local"), "{screen}");
}

#[test]
fn opening_with_hosts_asks_where_to_run_first() {
    let host = host();
    let mut world = World::default();
    world.snapshot.hosts = Some(vec![local_host(), devbox()]);
    press(&host, &world, "ctrl+n");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Run On"), "{screen}");
    assert!(
        screen.contains("local"),
        "the local machine is a choice: {screen}"
    );
    assert!(
        screen.contains("devbox") && screen.contains("me@devbox"),
        "a host is shown with the detail that tells two apart: {screen}"
    );
}

#[test]
fn escape_closes_the_flow_and_stops_asking() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    assert_ne!(drawn(&host, &world), "");
    press(&host, &world, "esc");
    assert_eq!(drawn(&host, &world), "");
    // And the reads stop being requested, so a closed flow costs nothing.
    assert_eq!(host.shared_string("want_bookmarks"), None);
    assert_eq!(host.shared_string("want_browse"), None);
}

#[test]
fn a_second_open_does_not_discard_what_was_chosen() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "space");
    press(&host, &world, "ctrl+n");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("[x] /src/thurbox"),
        "a stray ctrl+n must not reset the flow: {screen}"
    );
}

// ── The repository step ────────────────────────────────────────────────────

#[test]
fn remembered_repositories_are_listed_with_their_kind() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    let screen = drawn(&host, &world);
    assert!(screen.contains("Repos (2)"), "{screen}");
    assert!(screen.contains("/src/thurbox"), "{screen}");
    // A known non-repository reads as a plain directory.
    assert!(screen.contains("/src/notes"), "{screen}");
    assert!(screen.contains("(dir)"), "{screen}");
}

#[test]
fn a_folder_of_repositories_renders_as_a_group_that_folds() {
    let host = host();
    let world = world_with(folder_rows());
    open(&host, &world);
    let screen = drawn(&host, &world);
    assert!(screen.contains("▾ /src"), "an open group: {screen}");
    assert!(screen.contains("(parent)"), "{screen}");
    assert!(screen.contains("/src/thurbox"), "{screen}");

    // Space on the header folds it, and its children go with it.
    press(&host, &world, "space");
    let folded = drawn(&host, &world);
    assert!(folded.contains("▸ /src"), "a folded group: {folded}");
    assert!(
        !folded.contains("/src/thurbox"),
        "a folded group hides its members: {folded}"
    );
}

#[test]
fn search_filters_and_counts_what_it_matched() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    type_text(&host, &world, "note");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Search (1/2)"), "{screen}");
    assert!(screen.contains("/src/notes"), "{screen}");
    assert!(!screen.contains("thurbox"), "{screen}");
}

/// The repository rows as drawn, top to bottom, without their checkboxes.
fn listed(screen: &str) -> Vec<String> {
    screen
        .lines()
        .filter_map(|line| line.split_once("[ ] ").or_else(|| line.split_once("[x] ")))
        .map(|(_, rest)| rest.trim_end_matches(['│', ' ', '┐', '┘']).to_string())
        .collect()
}

#[test]
fn a_match_in_the_repository_name_ranks_above_one_in_its_path() {
    // Every path shares its leading directories, so a query that happens to be
    // spelled across them matched every row — in list order, with the row that
    // is actually called that somewhere down the list. The cursor starts on the
    // first row, and enter takes it, so the best match has to be first.
    let host = host();
    let world = world_with(vec![
        bookmark("/home/me/capital/web", Some(true)),
        bookmark("/home/me/work/api-gateway", Some(true)),
        bookmark("/home/me/work/api", Some(true)),
        bookmark("/home/me/work/legacy-api", Some(true)),
    ]);
    open(&host, &world);
    type_text(&host, &world, "api");
    assert_eq!(
        listed(&drawn(&host, &world)),
        vec![
            "/home/me/work/api",         // the name, exactly
            "/home/me/work/api-gateway", // the name starts with it
            "/home/me/work/legacy-api",  // the name contains it
            "/home/me/capital/web",      // only the path does
        ]
    );
}

#[test]
fn a_query_every_path_matches_keeps_the_list_order() {
    let host = host();
    let world = world_with(vec![
        bookmark("/home/me/projects/web", Some(true)),
        bookmark("/home/me/projects/cli", Some(true)),
    ]);
    open(&host, &world);
    type_text(&host, &world, "proj");
    assert_eq!(
        listed(&drawn(&host, &world)),
        vec!["/home/me/projects/web", "/home/me/projects/cli"]
    );
}

#[test]
fn a_search_ranks_folder_members_without_their_header() {
    // The ranked list is flat: a header is a folder, not something to pick, and
    // keeping it would split the ranking into groups.
    let host = host();
    let world = world_with(folder_rows());
    open(&host, &world);
    type_text(&host, &world, "thurbox");
    let screen = drawn(&host, &world);
    assert!(!screen.contains("(parent)"), "{screen}");
    assert!(screen.contains("/src/thurbox"), "{screen}");
}

#[test]
fn the_search_is_focused_as_soon_as_the_flow_opens() {
    // No `/` first: the flow opens on the repositories to pick from, and the
    // first thing a hand does there is start typing one's name.
    let host = host();
    let world = World::default();
    open(&host, &world);
    let screen = drawn(&host, &world);
    assert!(screen.contains("Search (2/2)"), "{screen}");
}

#[test]
fn letters_that_used_to_be_shortcuts_are_typed_into_the_search() {
    // j/k/w/d were list commands; with the search focused they are part of a
    // repository's name, which is what they are far more often.
    let host = host();
    let world = world_with(vec![
        bookmark("/src/jkwd", Some(true)),
        bookmark("/src/other", Some(true)),
    ]);
    open(&host, &world);
    type_text(&host, &world, "jkwd");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Search (1/2)"), "{screen}");
    assert!(!screen.contains("[wt]"), "{screen}");
    assert!(
        host.drain_commands().is_empty(),
        "`d` must not forget anything"
    );
}

#[test]
fn escape_clears_the_query_before_it_closes_the_flow() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    type_text(&host, &world, "note");
    press(&host, &world, "esc");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("Search (2/2)"),
        "cleared, still open: {screen}"
    );
    press(&host, &world, "esc");
    assert_eq!(drawn(&host, &world), "");
}

#[test]
fn enter_in_the_search_carries_the_ticked_repositories_on() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "space");
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Session Name"), "{screen}");
}

#[test]
fn shift_tab_from_the_path_returns_to_the_search() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "tab");
    press(&host, &world, "backtab");
    type_text(&host, &world, "note");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Search (1/2)"), "{screen}");
}

#[test]
fn a_letter_typed_into_a_field_is_not_a_command() {
    // `w` is worktree mode in the list and a letter in the path input. The flow
    // declares it as an action and declines it while a field has focus, which is
    // the only reason typing a path with a `w` in it works.
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "/srv/www");
    let screen = drawn(&host, &world);
    assert!(screen.contains("/srv/www"), "{screen}");
    assert!(!screen.contains("[wt]"), "{screen}");
}

#[test]
fn the_typed_path_is_asked_about_by_directory() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "/srv/rep");
    // The want carries the host and the DIRECTORY component: the listing that
    // both the dropdown and the completion are built from.
    assert_eq!(host.shared_string("want_browse").as_deref(), Some("\0/srv"));
}

#[test]
fn the_completion_offers_the_one_entry_that_extends_what_was_typed() {
    let host = host();
    let mut world = World::default();
    world.repos.set_listing_for_test(
        "",
        "/srv",
        Listing::Ready(vec![
            BrowseEntry {
                name: "repos".into(),
                is_git: false,
            },
            BrowseEntry {
                name: "other".into(),
                is_git: true,
            },
        ]),
    );
    world.wants.browse = Some((String::new(), "/srv".into()));
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "/srv/rep");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("/srv/rep") && screen.contains("os/"),
        "the ghost completes the typed prefix: {screen}"
    );

    // Tab takes it.
    press(&host, &world, "tab");
    assert!(drawn(&host, &world).contains("/srv/repos/"));
}

#[test]
fn the_browse_dropdown_lists_subdirectories_and_marks_the_repositories() {
    let host = host();
    let mut world = World::default();
    world.repos.set_listing_for_test(
        "",
        "/srv",
        Listing::Ready(vec![
            BrowseEntry {
                name: "repos".into(),
                is_git: false,
            },
            BrowseEntry {
                name: "thing".into(),
                is_git: true,
            },
        ]),
    );
    world.wants.browse = Some((String::new(), "/srv".into()));
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "/srv/");
    press(&host, &world, "tab");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Browse /srv"), "{screen}");
    assert!(screen.contains("repos/"), "{screen}");
    assert!(screen.contains("●git"), "a repository is marked: {screen}");
}

#[test]
fn the_dropdown_narrows_to_what_has_been_typed() {
    let host = host();
    let mut world = World::default();
    world.repos.set_listing_for_test(
        "",
        "/srv",
        Listing::Ready(vec![
            BrowseEntry {
                name: "repos".into(),
                is_git: false,
            },
            BrowseEntry {
                name: "other".into(),
                is_git: true,
            },
            BrowseEntry {
                name: ".cache".into(),
                is_git: false,
            },
        ]),
    );
    world.wants.browse = Some((String::new(), "/srv".into()));
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "/srv/");
    press(&host, &world, "tab");
    let all = drawn(&host, &world);
    assert!(all.contains("repos/") && all.contains("other/"), "{all}");
    assert!(
        !all.contains(".cache"),
        "a dotfile is offered only to a dotted prefix: {all}"
    );

    type_text(&host, &world, "re");
    let narrowed = drawn(&host, &world);
    assert!(narrowed.contains("repos/"), "{narrowed}");
    assert!(
        !narrowed.contains("other/"),
        "the dropdown lists the directory filtered by what was typed: {narrowed}"
    );
}

#[test]
fn a_dotted_prefix_offers_the_hidden_entries() {
    let host = host();
    let mut world = World::default();
    world.repos.set_listing_for_test(
        "",
        "/srv",
        Listing::Ready(vec![BrowseEntry {
            name: ".cache".into(),
            is_git: false,
        }]),
    );
    world.wants.browse = Some((String::new(), "/srv".into()));
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "/srv/.");
    press(&host, &world, "tab");
    assert!(drawn(&host, &world).contains(".cache"));
}

#[test]
fn tab_on_a_fresh_field_browses_home_when_memory_is_empty() {
    // The regression behind "tab no longer browses directories": the flow asked
    // for a listing only once something had been typed, so `tab` on a fresh
    // field opened a dropdown whose want was never published — it sat on "(no
    // subdirectories)" forever. With nothing remembered the field starts at
    // home, exactly as a bare `~` does.
    let host = host();
    let mut world = world_with(Vec::new());
    world.repos.set_listing_for_test(
        "",
        "~",
        Listing::Ready(vec![BrowseEntry {
            name: "src".into(),
            is_git: false,
        }]),
    );
    world.wants.browse = Some((String::new(), "~".into()));
    open(&host, &world);
    press(&host, &world, "tab");
    // Nothing typed yet, and nothing to complete — so this `tab` opens the
    // browser rather than taking a suggestion.
    press(&host, &world, "tab");
    assert_eq!(
        host.shared_string("want_browse").as_deref(),
        Some("\0~"),
        "the open dropdown asks for home"
    );
    let screen = drawn(&host, &world);
    assert!(screen.contains("src/"), "home is listed: {screen}");
}

#[test]
fn a_flow_picking_from_memory_asks_for_no_listing() {
    // A flow only picking from memory must not pay for a directory read — even
    // though the path field now holds its starting directory the whole time.
    // The want is restated on every render, so what a frame asks for is read
    // after one; leaving the field stops the asking.
    let host = host();
    let world = World::default();
    open(&host, &world);
    drawn(&host, &world);
    assert_eq!(host.shared_string("want_browse"), None);
    press(&host, &world, "tab");
    drawn(&host, &world);
    assert_eq!(host.shared_string("want_browse").as_deref(), Some("\0/src"));
    press(&host, &world, "backtab");
    drawn(&host, &world);
    assert_eq!(host.shared_string("want_browse"), None);
}

/// What the path field holds once focus has moved into it.
fn path_field(host: &LuaHost, world: &World) -> String {
    open(host, world);
    press(host, world, "tab");
    let screen = drawn(host, world);
    let lines: Vec<&str> = screen.lines().collect();
    let title = lines
        .iter()
        .position(|line| line.contains("Add Repo Path"))
        .unwrap_or_else(|| panic!("no path field: {screen}"));
    lines[title + 1]
        .trim_matches(|c| c == '│' || c == ' ')
        .to_string()
}

#[test]
fn the_path_field_starts_at_the_directory_every_repository_shares() {
    let world = world_with(vec![
        bookmark("/home/me/code/work/api", Some(true)),
        bookmark("/home/me/code/perso/thurbox", Some(true)),
    ]);
    assert_eq!(path_field(&host(), &world), "/home/me/code/");
}

#[test]
fn the_shared_directory_is_found_by_whole_components() {
    // `/src/app` and `/src/apple` share `/src`, not `/src/app`.
    let world = world_with(vec![
        bookmark("/src/app/one", Some(true)),
        bookmark("/src/apple/two", Some(true)),
    ]);
    assert_eq!(path_field(&host(), &world), "/src/");
}

#[test]
fn a_single_repository_starts_the_field_at_its_parent() {
    let world = world_with(vec![bookmark("/srv/code/thurbox", Some(true))]);
    assert_eq!(path_field(&host(), &world), "/srv/code/");
}

#[test]
fn repositories_with_nothing_in_common_start_at_the_root() {
    let world = world_with(vec![
        bookmark("/srv/one", Some(true)),
        bookmark("/opt/two", Some(true)),
    ]);
    assert_eq!(path_field(&host(), &world), "/");
}

#[test]
fn folder_headers_and_the_offered_interface_do_not_move_the_start() {
    // The header is a folder, not a repository in it, and the interface
    // directory is offered by the kernel, not remembered by the user.
    let mut rows = folder_rows();
    rows.insert(0, offered());
    assert_eq!(path_field(&host(), &world_with(rows)), "/src/");
    assert_eq!(path_field(&host(), &world_with(vec![offered()])), "~/");
}

#[test]
fn a_name_typed_into_the_untouched_field_lands_under_the_start() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "new-thing");
    assert!(drawn(&host, &world).contains("/src/new-thing"));
}

#[test]
fn a_new_path_typed_into_the_untouched_field_replaces_the_start() {
    // `/` or `~` first is a path of its own, not a name under the start — the
    // way an address bar takes a new address.
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "~/elsewhere");
    let screen = drawn(&host, &world);
    assert!(screen.contains("~/elsewhere"), "{screen}");
    assert!(!screen.contains("/src/~"), "{screen}");
}

#[test]
fn a_field_that_was_typed_in_is_not_refilled() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "/opt/x");
    press(&host, &world, "backtab");
    press(&host, &world, "tab");
    assert!(drawn(&host, &world).contains("/opt/x"));
}

#[test]
fn a_bare_tilde_browses_home_rather_than_filtering_by_it() {
    // v1's exception: `~` is where to look, not something to match names against.
    let host = host();
    let mut world = World::default();
    world.repos.set_listing_for_test(
        "",
        "~",
        Listing::Ready(vec![BrowseEntry {
            name: "src".into(),
            is_git: false,
        }]),
    );
    world.wants.browse = Some((String::new(), "~".into()));
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "~");
    assert_eq!(host.shared_string("want_browse").as_deref(), Some("\0~"));
    press(&host, &world, "tab");
    assert!(
        drawn(&host, &world).contains("src/"),
        "home is listed, not filtered by a literal tilde"
    );
}

#[test]
fn a_wait_for_a_listing_is_visible_rather_than_looking_empty() {
    let host = host();
    let mut world = World::default();
    world
        .repos
        .set_listing_for_test("", "/srv", Listing::Pending);
    world.wants.browse = Some((String::new(), "/srv".into()));
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "/srv/");
    press(&host, &world, "tab");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("listing…"),
        "a slow host must not read as an empty directory: {screen}"
    );
}

#[test]
fn a_missing_directory_is_reported_in_place_of_its_entries() {
    let host = host();
    let mut world = World::default();
    world.repos.set_listing_for_test(
        "",
        "/srv",
        Listing::Failed("No such directory: /srv".into()),
    );
    world.wants.browse = Some((String::new(), "/srv".into()));
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "/srv/");
    press(&host, &world, "tab");
    assert!(drawn(&host, &world).contains("No such directory"));
}

#[test]
fn the_dropdown_keeps_the_arrows_while_it_is_open() {
    // The arrows move the repository list even while the path field has focus,
    // which is the one place that rule has an exception: an open dropdown is a
    // list of its own, and it must not have the list behind it move instead.
    let host = host();
    let mut world = World::default();
    world.repos.set_listing_for_test(
        "",
        "/srv",
        Listing::Ready(vec![
            BrowseEntry {
                name: "alpha".into(),
                is_git: false,
            },
            BrowseEntry {
                name: "beta".into(),
                is_git: false,
            },
        ]),
    );
    world.wants.browse = Some((String::new(), "/srv".into()));
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "/srv/");
    press(&host, &world, "tab");

    press(&host, &world, "down");
    press(&host, &world, "enter");
    assert_eq!(
        host.drain_commands(),
        vec![Command::Bookmark {
            host: String::new(),
            path: "/srv/beta".into(),
            edit: BookmarkEdit::Add,
        }],
        "Enter selects the dropdown row reached by the arrow"
    );
}

#[test]
fn escape_closes_the_dropdown_before_it_closes_the_flow() {
    let host = host();
    let mut world = World::default();
    world
        .repos
        .set_listing_for_test("", "/srv", Listing::Ready(Vec::new()));
    world.wants.browse = Some((String::new(), "/srv".into()));
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "/srv/");
    press(&host, &world, "tab");
    assert!(drawn(&host, &world).contains("Browse"));
    press(&host, &world, "esc");
    let screen = drawn(&host, &world);
    assert!(!screen.contains("Browse"), "{screen}");
    assert!(
        screen.contains("Select Repo"),
        "one Esc closes one thing: {screen}"
    );
}

// ── The commands the flow issues ───────────────────────────────────────────

#[test]
fn a_typed_path_is_committed_as_a_bookmark_rather_than_trusted() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "~/src/new");
    press(&host, &world, "enter");
    let issued = host.drain_commands();
    assert_eq!(
        issued,
        vec![Command::Bookmark {
            host: String::new(),
            path: "~/src/new".into(),
            edit: BookmarkEdit::Add,
        }],
        "the tilde is left for the target machine to expand"
    );
}

#[test]
fn a_path_just_added_is_the_row_that_gets_selected() {
    // The flow finds the row it just added by recency — memory is published
    // most-recent-first — and the kernel offers the interface directory ahead of
    // memory. Taking row 1 therefore selected the INTERFACE for every repository
    // added, leaving the typed one unchecked.
    let host = host();
    let mut world = world_with(vec![offered()]);
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "/src/new");
    press(&host, &world, "enter");

    // The write lands: the added path is now the newest thing in memory, still
    // behind the offered row.
    world.repos = store_with(vec![offered(), bookmark("/src/new", Some(true))]);
    let screen = drawn(&host, &world);
    let selected: Vec<&str> = screen.lines().filter(|line| line.contains("[x]")).collect();
    assert_eq!(selected.len(), 1, "one row is chosen:\n{screen}");
    assert!(
        selected[0].contains("new"),
        "the path just typed is the one chosen:\n{screen}"
    );
}

// ── A path that does not exist yet ─────────────────────────────────────────

/// A world whose `/src` holds the two remembered repositories and nothing else,
/// with that listing served — the directory the path field starts in.
fn world_listing_src() -> World {
    let mut world = World::default();
    world.repos.set_listing_for_test(
        "",
        "/src",
        Listing::Ready(vec![
            BrowseEntry {
                name: "thurbox".into(),
                is_git: true,
            },
            BrowseEntry {
                name: "notes".into(),
                is_git: false,
            },
        ]),
    );
    world.wants.browse = Some((String::new(), "/src".into()));
    world
}

#[test]
fn a_name_that_is_not_there_offers_to_create_the_folder() {
    let host = host();
    let world = world_listing_src();
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "brand-new");
    let screen = drawn(&host, &world);
    assert!(screen.contains("[ Create folder ]"), "{screen}");

    // A name that IS there is still an add.
    let host = self::host();
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "notes");
    assert!(drawn(&host, &world).contains("[ Add repo ]"));
}

#[test]
fn creating_a_folder_asks_what_goes_into_it() {
    let host = host();
    let world = world_listing_src();
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "brand-new");
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(screen.contains("New Folder"), "{screen}");
    assert!(screen.contains("/src/brand-new"), "{screen}");
    assert!(screen.contains("git init"), "{screen}");
    assert!(screen.contains("Leave it empty"), "{screen}");
    assert!(
        host.drain_commands().is_empty(),
        "nothing is made before the answer"
    );
}

#[test]
fn a_new_git_repository_is_one_command_and_lands_ticked() {
    let host = host();
    let world = world_listing_src();
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "brand-new");
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    assert_eq!(
        host.drain_commands(),
        vec![Command::Bookmark {
            host: String::new(),
            path: "/src/brand-new".into(),
            edit: BookmarkEdit::Init,
        }]
    );
    // Back on the repositories, where the new row is picked once it lands —
    // the same select-or-add a typed path gets.
    assert!(drawn(&host, &world).contains("Select Repo"));
}

#[test]
fn adding_a_repository_clears_the_search_that_would_hide_it() {
    // The new row is ticked and the cursor put on it — which is no use behind a
    // query typed before the path was, one that the new row does not match.
    let host = host();
    let world = world_listing_src();
    open(&host, &world);
    type_text(&host, &world, "zzz");
    press(&host, &world, "tab");
    type_text(&host, &world, "brand-new");
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    assert!(drawn(&host, &world).contains("Search (2/2)"));

    let host = self::host();
    open(&host, &world);
    type_text(&host, &world, "zzz");
    press(&host, &world, "tab");
    type_text(&host, &world, "/srv/typed");
    press(&host, &world, "enter");
    assert!(drawn(&host, &world).contains("Search (2/2)"));
}

#[test]
fn a_folder_can_be_left_empty() {
    let host = host();
    let world = world_listing_src();
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "scratch");
    press(&host, &world, "enter");
    // The last choice; the selector stops there rather than wrapping.
    press(&host, &world, "j");
    press(&host, &world, "j");
    press(&host, &world, "enter");
    assert_eq!(
        host.drain_commands(),
        vec![Command::Bookmark {
            host: String::new(),
            path: "/src/scratch".into(),
            edit: BookmarkEdit::Create,
        }]
    );
}

#[test]
fn escape_from_the_folder_question_keeps_what_was_typed() {
    let host = host();
    let world = world_listing_src();
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "brand-new");
    press(&host, &world, "enter");
    press(&host, &world, "esc");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Select Repo"), "{screen}");
    assert!(screen.contains("/src/brand-new"), "{screen}");
    assert!(host.drain_commands().is_empty());
}

/// Walk to the new-folder question for `/src/<name>` and pick "clone".
fn to_the_clone_step(host: &LuaHost, world: &World, name: &str) {
    open(host, world);
    press(host, world, "tab");
    type_text(host, world, name);
    press(host, world, "enter");
    press(host, world, "down");
    press(host, world, "enter");
}

#[test]
fn a_new_folder_can_have_a_repository_cloned_into_it() {
    let host = host();
    let world = world_listing_src();
    to_the_clone_step(&host, &world, "fork-of-it");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Clone Repository"), "{screen}");
    assert!(
        screen.contains("/src/fork-of-it"),
        "the destination: {screen}"
    );
    assert!(host.drain_commands().is_empty());

    type_text(&host, &world, "git@github.com:me/it.git");
    press(&host, &world, "enter");
    assert_eq!(
        host.drain_commands(),
        vec![Command::Bookmark {
            host: String::new(),
            path: "/src/fork-of-it".into(),
            edit: BookmarkEdit::Clone {
                url: "git@github.com:me/it.git".into()
            },
        }]
    );
    assert!(drawn(&host, &world).contains("Select Repo"));
}

#[test]
fn a_clone_needs_a_url() {
    let host = host();
    let world = world_listing_src();
    to_the_clone_step(&host, &world, "fork-of-it");
    let screen = drawn(&host, &world);
    assert!(
        !screen.contains("[ Clone ]"),
        "nothing to clone yet: {screen}"
    );
    press(&host, &world, "enter");
    assert!(host.drain_commands().is_empty());
    assert!(drawn(&host, &world).contains("Clone Repository"));
    type_text(&host, &world, "https://example.com/it.git");
    assert!(drawn(&host, &world).contains("[ Clone ]"));
}

#[test]
fn escape_from_the_clone_goes_back_to_the_folder_question() {
    let host = host();
    let world = world_listing_src();
    to_the_clone_step(&host, &world, "fork-of-it");
    press(&host, &world, "esc");
    assert!(drawn(&host, &world).contains("New Folder"));
}

#[test]
fn a_clone_under_way_says_so_where_the_path_is_typed() {
    let host = host();
    let mut world = world_listing_src();
    to_the_clone_step(&host, &world, "fork-of-it");
    type_text(&host, &world, "https://example.com/it.git");
    press(&host, &world, "enter");
    world.inflight.push(InFlight {
        id: 1,
        kind: "bookmark",
        session: String::new(),
        subject: None,
        host: None,
        phase: Phase::Running,
        error: None,
    });
    let screen = drawn(&host, &world);
    assert!(screen.contains("cloning…"), "{screen}");
}

#[test]
fn a_write_that_failed_ticks_nothing_in_its_place() {
    // The row a write lands as is found by recency, so a write that never
    // landed would tick whichever row was newest before it — and a confirm
    // after the reported failure would open a session somewhere nobody chose.
    let host = host();
    let mut world = world_listing_src();
    to_the_clone_step(&host, &world, "fork-of-it");
    type_text(&host, &world, "https://example.com/gone.git");
    press(&host, &world, "enter");
    world.inflight.push(InFlight {
        id: 7,
        kind: "bookmark",
        session: String::new(),
        subject: Some("/src/fork-of-it".into()),
        host: None,
        phase: Phase::Failed,
        error: Some("git clone failed: not found".into()),
    });
    let screen = drawn(&host, &world);
    assert!(!screen.contains("▸ /src/fork-of-it"), "{screen}");

    // Nor does the tick arrive late, once the failure is swept from the list.
    world.inflight.clear();
    let screen = drawn(&host, &world);
    assert!(
        !screen.contains("▸ /src/fork-of-it"),
        "no stale pick later either: {screen}"
    );
}

#[test]
fn a_failed_addition_preserves_a_path_edited_while_it_was_running() {
    let host = host();
    let mut world = World::default();
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "/opt/refused");
    press(&host, &world, "enter");
    world.inflight.push(InFlight {
        id: 7,
        kind: "bookmark",
        session: String::new(),
        subject: Some("/opt/refused".into()),
        host: None,
        phase: Phase::Running,
        error: None,
    });
    type_text(&host, &world, "/opt/newer");
    world.inflight[0].phase = Phase::Failed;
    world.inflight[0].error = Some("Path not found: /opt/refused".into());
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("/opt/newer"),
        "keep the newer input:\n{screen}"
    );
    assert!(
        screen.contains("Path not found: /opt/refused"),
        "show the refusal:\n{screen}"
    );
}

#[test]
fn an_earlier_failure_does_not_stop_the_next_write_being_ticked() {
    let host = host();
    let mut world = world_listing_src();
    world.inflight.push(InFlight {
        id: 3,
        kind: "bookmark",
        session: String::new(),
        subject: Some("/src/notes".into()),
        host: None,
        phase: Phase::Failed,
        error: Some("an older refusal of the same path".into()),
    });
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "notes");
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(screen.contains("[x]"), "{screen}");
}

#[test]
fn another_write_failing_meanwhile_does_not_count_as_this_ones() {
    // Writes run independently: a forget that fails while a clone is running
    // says nothing about the clone.
    let host = host();
    let mut world = world_listing_src();
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "notes");
    press(&host, &world, "enter");
    world.inflight.push(InFlight {
        id: 9,
        kind: "bookmark",
        session: String::new(),
        subject: Some("/src/elsewhere".into()),
        host: None,
        phase: Phase::Failed,
        error: Some("not a remembered repository".into()),
    });
    let screen = drawn(&host, &world);
    assert!(screen.contains("[x]"), "{screen}");
}

#[test]
fn a_parent_that_is_missing_too_is_still_a_folder_to_create() {
    // `mkdir -p` makes the parents, so a failed listing is no reason to refuse.
    let host = host();
    let mut world = World::default();
    world.repos.set_listing_for_test(
        "",
        "/src/new",
        Listing::Failed("No such directory: /src/new".into()),
    );
    world.wants.browse = Some((String::new(), "/src/new".into()));
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "new/deeper");
    assert!(drawn(&host, &world).contains("[ Create folder ]"));
}

#[test]
fn a_listing_still_on_its_way_leaves_enter_an_add() {
    // Until the listing says otherwise the path may well exist, and the kernel
    // is the one that checks: the old behaviour, unchanged.
    let host = host();
    let mut world = World::default();
    world.wants.browse = Some((String::new(), "/src".into()));
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "maybe");
    press(&host, &world, "enter");
    assert_eq!(
        host.drain_commands(),
        vec![Command::Bookmark {
            host: String::new(),
            path: "/src/maybe".into(),
            edit: BookmarkEdit::Add,
        }]
    );
}

#[test]
fn alt_p_imports_the_typed_path_as_a_folder() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "/src");
    press(&host, &world, "alt+p");
    assert_eq!(
        host.drain_commands(),
        vec![Command::Bookmark {
            host: String::new(),
            path: "/src".into(),
            edit: BookmarkEdit::Parent,
        }]
    );
}

#[test]
fn alt_d_forgets_a_remembered_repository() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "alt+d");
    assert_eq!(
        host.drain_commands(),
        vec![Command::Bookmark {
            host: String::new(),
            path: "/src/thurbox".into(),
            edit: BookmarkEdit::Remove,
        }]
    );
}

#[test]
fn delete_forgets_too_once_there_is_nothing_ahead_of_the_caret() {
    // `delete` still edits the query while there is text ahead of the caret —
    // only past the end, where it would do nothing, does it mean "forget".
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "delete");
    assert_eq!(
        host.drain_commands(),
        vec![Command::Bookmark {
            host: String::new(),
            path: "/src/thurbox".into(),
            edit: BookmarkEdit::Remove,
        }]
    );
    type_text(&host, &world, "src");
    press(&host, &world, "left");
    press(&host, &world, "delete");
    assert!(host.drain_commands().is_empty());
    assert!(drawn(&host, &world).contains("Search (2/2)"));
}

#[test]
fn a_member_of_a_folder_cannot_be_forgotten_on_its_own() {
    let host = host();
    let world = world_with(folder_rows());
    open(&host, &world);
    press(&host, &world, "down");
    press(&host, &world, "alt+d");
    assert!(
        host.drain_commands().is_empty(),
        "a child has no memory of its own to forget"
    );
    assert!(drawn(&host, &world).contains("forget the folder header instead"));
}

// ── Through to creation ────────────────────────────────────────────────────

/// Walk a plain (no worktree) selection through to the create command.
#[test]
fn a_plain_selection_names_the_session_then_the_agent() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "down"); // /src/notes, no repository
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("Session Name"),
        "a plain selection asks no branch question: {screen}"
    );

    type_text(&host, &world, "read the code");
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Coding Agent"), "{screen}");
    // The configured default is preselected, and the command it wraps is shown.
    assert!(screen.contains("▸ claude"), "{screen}");

    press(&host, &world, "enter");
    assert_eq!(
        host.drain_commands(),
        vec![Command::Create {
            name: "read the code".into(),
            repo: "/src/notes".into(),
            agent: Some("claude".into()),
            host: None,
            worktree: Some(false),
            branch: None,
            base: None,
            extras: Vec::new(),
        }]
    );
    assert_eq!(drawn(&host, &world), "", "the flow closes when it commits");
}

/// A world whose repository `/src/thurbox` has the branches `listing`, and
/// whose flow is asking for them, as the loop serves `store.want_branches`.
fn world_with_branches(listing: thurbox::kernel::repos::BranchListing) -> World {
    let mut world = World::default();
    world.repos.set_branches_for_test("/src/thurbox", listing);
    world.wants.branches = Some("/src/thurbox".into());
    world
}

fn main_and_dev() -> thurbox::kernel::repos::BranchListing {
    thurbox::kernel::repos::BranchListing::Ready(thurbox::git::Branches {
        default: Some("main".into()),
        names: vec!["dev".into(), "main".into()],
        worktrees: Vec::new(),
    })
}

/// The upstream's default branch is under the cursor and is the base sent,
/// and a fetch that failed says so above the list it left as it was.
#[test]
fn the_base_branch_defaults_to_origin_and_says_when_the_fetch_failed() {
    let host = host();
    let mut world = world_with_branches(thurbox::kernel::repos::BranchListing::Ready(
        thurbox::git::Branches {
            default: Some("origin/main".into()),
            names: vec!["main".into(), "origin/dev".into(), "origin/main".into()],
            worktrees: Vec::new(),
        },
    ));
    world
        .repos
        .set_fetch_note_for_test("/src/thurbox", "fatal: unable to access origin");
    open(&host, &world);
    press(&host, &world, "alt+w");
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("▸ origin/main")
            && screen.contains("not fetched: fatal: unable to access origin"),
        "{screen}"
    );
    press(&host, &world, "enter");
    type_text(&host, &world, "Fix tooltips");
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    let sent = host.drain_commands();
    assert!(
        matches!(
            sent.as_slice(),
            [Command::Create { base: Some(base), .. }] if base == "origin/main"
        ),
        "{sent:?}"
    );
}

/// v2's worktree flow: `alt+w` gives the repository a worktree (`[wt]`),
/// then the base branch is picked (the default under the cursor), the session
/// named, and its branch named, prefilled from the name.
#[test]
fn a_worktree_asks_the_base_branch_then_names_the_branch() {
    let host = host();
    let world = world_with_branches(main_and_dev());
    open(&host, &world);
    press(&host, &world, "alt+w");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("[x]") && screen.contains("[wt]"),
        "{screen}"
    );
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("Base Branch") && screen.contains("▸ main"),
        "{screen}"
    );
    press(&host, &world, "enter");
    assert!(drawn(&host, &world).contains("Session Name"));
    type_text(&host, &world, "Fix tooltips");
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("Branch Name") && screen.contains("fix-tooltips"),
        "{screen}"
    );
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    assert_eq!(
        host.drain_commands(),
        vec![Command::Create {
            name: "Fix tooltips".into(),
            repo: "/src/thurbox".into(),
            agent: Some("claude".into()),
            host: None,
            worktree: Some(true),
            branch: Some("fix-tooltips".into()),
            base: Some("main".into()),
            extras: Vec::new(),
        }]
    );
}

/// Every ticked repository goes into the one session: the worktrees first,
/// the plain ones after, in the list's order. The first is where the agent
/// starts and the base branch is asked for; the rest ride along as extras.
#[test]
fn ticked_repositories_all_go_into_the_session() {
    let host = host();
    let world = world_with_branches(main_and_dev());
    open(&host, &world);
    press(&host, &world, "alt+w"); // /src/thurbox, a worktree
    press(&host, &world, "down");
    press(&host, &world, "space"); // /src/notes, as it is
    press(&host, &world, "enter");
    press(&host, &world, "enter"); // the base branch
    type_text(&host, &world, "both");
    press(&host, &world, "enter");
    press(&host, &world, "enter"); // the branch, `both`
    press(&host, &world, "enter");
    assert!(matches!(
        &host.drain_commands()[..],
        [Command::Create { repo, worktree: Some(true), branch: Some(b), extras, .. }]
            if repo == "/src/thurbox"
                && b == "both"
                && extras == &[thurbox::kernel::command::ExtraMember {
                    path: "/src/notes".into(),
                    worktree: false,
                }]
    ),);
}

/// A directory with no repository is refused a worktree, in v2's words, and
/// stays ticked as a plain directory.
#[test]
fn a_directory_without_git_is_refused_a_worktree() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "down"); // /src/notes
    press(&host, &world, "alt+w");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Not a git repo"), "{screen}");
    assert!(!screen.contains("[wt]"), "{screen}");
}

/// A repository's existing worktrees open under it with `→`, and `enter` on
/// one runs the session in it as it is, named after its directory.
#[test]
fn an_existing_worktree_opens_as_it_is() {
    let host = host();
    let world = world_with_branches(thurbox::kernel::repos::BranchListing::Ready(
        thurbox::git::Branches {
            default: Some("main".into()),
            names: vec!["feat".into(), "main".into()],
            worktrees: vec![thurbox::git::Worktree {
                path: "/src/thurbox-feat".into(),
                branch: Some("feat".into()),
            }],
        },
    ));
    open(&host, &world);
    press(&host, &world, "right");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("↳ thurbox-feat") && screen.contains("feat"),
        "{screen}"
    );
    press(&host, &world, "down");
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    assert!(matches!(
        &host.drain_commands()[..],
        [Command::Create { name, repo, worktree: Some(false), extras, .. }]
            if name == "thurbox-feat" && repo == "/src/thurbox-feat" && extras.is_empty()
    ),);
}

/// The same plain selection on the Fleet engine (ADR-41) becomes a Task.
/// `fleet` here is a stand-in that records what it was asked.
#[cfg(unix)]
#[test]
fn a_plain_selection_on_the_fleet_engine_applies_a_task() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "down");
    press(&host, &world, "enter");
    type_text(&host, &world, "read the code");
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    let commands = host.drain_commands();
    assert!(
        matches!(&commands[..], [Command::Create { .. }]),
        "the flow creates one session: {commands:?}"
    );

    let fleet = StubFleet::new("[\"local\"]");
    fleet.run(commands).expect("the create");
    let task = fleet.applied();
    // A Task's name is a DNS-like name: the typed one, made one.
    assert_eq!(task["metadata"]["name"], "read-the-code");
    // `/src/notes` is no repository, so it runs as it is; `tui_e2e`
    // covers a repository's worktree.
    assert_eq!(
        task["spec"]["workspace"],
        serde_json::json!(["file:///src/notes"])
    );
    assert_eq!(task["spec"]["harness"], "claude");
    assert_eq!(task["spec"]["host"], "local");
}

/// A `fleet` that answers what the executor asks before it applies (no Task
/// yet, the harnesses with a profile, the hosts `hosts` names, as JSON) and
/// records the rest, with what it read on its standard input.
#[cfg(unix)]
struct StubFleet {
    dir: tempfile::TempDir,
    program: std::path::PathBuf,
}

#[cfg(unix)]
impl StubFleet {
    fn new(hosts: &str) -> StubFleet {
        let dir = tempfile::tempdir().expect("dir");
        // The interface's own files (a bookmark) land in this profile.
        for (var, sub) in [
            ("THURBOX_CONFIG_DIR", "config"),
            ("THURBOX_DATA_DIR", "data"),
        ] {
            std::fs::create_dir_all(dir.path().join(sub)).expect("dir");
            std::env::set_var(var, dir.path().join(sub));
        }
        let program = dir.path().join("fleet");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  get) exit 3 ;;\n  version) echo '{{\"harnesses\":[\"claude\",\"claude-code\",\"codex\"]}}' ;;\n  runtime) echo '{{\"hosts\":{hosts}}}' ;;\n  repositories) echo '{{\"repositories\":{{\"https://example.invalid/thurbox\":\"/src/thurbox\"}}}}' ;;\n  *) echo \"$@\" >> '{0}'; cat >> '{0}' ;;\nesac\n",
                dir.path().join("asked").display()
            ),
        )
        .expect("fleet");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        StubFleet { dir, program }
    }

    /// Dispatch `commands` on the Fleet engine's bus, one by one, and
    /// answer the first failure.
    fn run(&self, commands: Vec<Command>) -> Result<(), String> {
        let mut bus = thurbox::kernel::command::CommandBus::fleet(
            thurbox::fleet_link::FleetLink::local(self.program.to_string_lossy()),
        );
        for command in commands {
            let id = bus.dispatch(command);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                bus.poll();
                match bus.inflight().into_iter().find(|entry| entry.id == id) {
                    None => break,
                    Some(entry) if entry.phase == Phase::Failed => {
                        return Err(entry.error.unwrap_or_default())
                    }
                    Some(_) => {}
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the command never finished"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        Ok(())
    }

    /// The Task the one `apply` carried.
    fn applied(&self) -> serde_json::Value {
        let asked =
            std::fs::read_to_string(self.dir.path().join("asked")).expect("fleet was asked");
        let (args, manifest) = asked.split_once('\n').expect("args, then the manifest");
        assert_eq!(args, "apply --create-only -f -");
        serde_json::from_str(manifest).expect("a Task")
    }

    fn asked(&self) -> bool {
        self.dir.path().join("asked").exists()
    }
}

/// A repository path typed into the flow is remembered before the session
/// is made: on the Fleet engine too, since the bookmark is the interface's,
/// not a session's.
#[cfg(unix)]
#[test]
fn a_typed_repository_is_remembered_on_the_fleet_engine() {
    let fleet = StubFleet::new("[\"local\"]");
    let srv = fleet.dir.path().join("srv");
    std::fs::create_dir_all(srv.join("beta")).expect("repo");
    let host = host();
    let mut world = World::default();
    let srv_text = format!("{}/", srv.display());
    world.repos.set_listing_for_test(
        "",
        &srv.to_string_lossy(),
        Listing::Ready(vec![BrowseEntry {
            name: "beta".into(),
            is_git: false,
        }]),
    );
    world.wants.browse = Some((String::new(), srv.to_string_lossy().into_owned()));
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, &srv_text);
    press(&host, &world, "tab");
    press(&host, &world, "down");
    press(&host, &world, "enter");
    let commands = host.drain_commands();
    assert!(
        matches!(&commands[..], [Command::Bookmark { .. }]),
        "{commands:?}"
    );
    fleet.run(commands).expect("the bookmark");
    assert!(!fleet.asked(), "a bookmark asks nothing of Fleet");

    // And the flow sees it saved, as it waits to.
    let mut repos = RepoStore::new();
    let beta = srv.join("beta").to_string_lossy().into_owned();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        repos.request_bookmarks("");
        repos.poll();
        if repos
            .bookmarks("")
            .is_some_and(|rows| rows.iter().any(|row| row.path == beta))
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the bookmark never read back: {:?}",
            repos.bookmarks("")
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The host picker names a route (`ssh:devbox`); Fleet names the same
/// machine `devbox`, and refuses a host its config does not define.
#[cfg(unix)]
#[test]
fn a_session_on_a_host_names_the_fleet_host() {
    let flow = || {
        let host = host();
        let mut world = World::default();
        world.snapshot.hosts = Some(vec![local_host(), devbox()]);
        press(&host, &world, "ctrl+n");
        press(&host, &world, "down");
        press(&host, &world, "enter");
        press(&host, &world, "space");
        press(&host, &world, "enter");
        press(&host, &world, "enter");
        press(&host, &world, "enter");
        host.drain_commands()
    };
    let commands = flow();
    assert!(
        matches!(&commands[..], [Command::Create { host: Some(h), .. }] if h == "ssh:devbox"),
        "{commands:?}"
    );

    let fleet = StubFleet::new("[\"devbox\",\"local\"]");
    fleet.run(commands).expect("the create");
    assert_eq!(fleet.applied()["spec"]["host"], "devbox");

    let unknown = StubFleet::new("[\"local\"]");
    let refused = unknown.run(flow()).expect_err("Fleet defines no devbox");
    assert!(refused.contains("devbox"), "{refused}");
    assert!(!unknown.asked(), "nothing was applied");
}

/// A session on another Fleet host runs in the repository directory there,
/// as it is: its Task names a `file://` URL of the path given.
#[cfg(unix)]
#[test]
fn a_session_on_another_fleet_host_runs_in_its_directory() {
    let fleet = StubFleet::new("[\"devbox\",\"local\"]");
    fleet
        .run(vec![Command::Create {
            name: "far".into(),
            repo: "/home/me/far-repo".into(),
            agent: Some("claude".into()),
            host: Some("ssh:devbox".into()),
            worktree: None,
            branch: None,
            base: None,
            extras: Vec::new(),
        }])
        .expect("a session on devbox is not refused");
    let task = fleet.applied();
    assert_eq!(task["spec"]["host"], "devbox");
    assert_eq!(
        task["spec"]["workspace"],
        serde_json::json!(["file:///home/me/far-repo"])
    );
}

/// A directory that is no git repository, such as the installed interface
/// the picker offers to edit, has no worktree to make: its session runs in it
/// as it is.
#[cfg(unix)]
#[test]
fn a_directory_that_is_no_repository_runs_as_it_is() {
    let fleet = StubFleet::new("[\"local\"]");
    let plain = tempfile::tempdir().expect("a plain directory");
    let path = plain.path().to_string_lossy().into_owned();
    fleet
        .run(vec![Command::Create {
            name: "panes".into(),
            repo: path.clone(),
            agent: Some("claude".into()),
            host: None,
            worktree: None,
            branch: None,
            base: None,
            extras: Vec::new(),
        }])
        .expect("a plain directory is not refused");
    assert_eq!(
        fleet.applied()["spec"]["workspace"],
        serde_json::json!([format!("file://{path}")])
    );
}

/// A bare repository is a repository: it has no `.git` beside its files, but
/// its session still gets a worktree, never a run inside the bare
/// repository.
#[cfg(unix)]
#[test]
fn a_bare_repository_is_not_run_as_a_plain_directory() {
    let fleet = StubFleet::new("[\"local\"]");
    let bare = tempfile::tempdir().expect("a bare repository");
    std::fs::write(bare.path().join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    std::fs::create_dir_all(bare.path().join("objects")).expect("objects");
    std::fs::create_dir_all(bare.path().join("refs")).expect("refs");
    let path = bare.path().to_string_lossy().into_owned();
    fleet
        .run(vec![Command::Create {
            name: "bare".into(),
            repo: path.clone(),
            agent: Some("claude".into()),
            host: None,
            worktree: None,
            branch: None,
            base: None,
            extras: Vec::new(),
        }])
        .expect("a bare repository is not refused");
    assert_eq!(
        fleet.applied()["spec"]["workspace"],
        serde_json::json!([format!("git+file://{path}?branch=bare")])
    );
}

/// Only an agent Fleet has a harness profile for becomes a Task: `shell`
/// is a command of thurbox's registry, not a harness.
#[cfg(unix)]
#[test]
fn an_agent_with_no_fleet_harness_is_refused_by_name() {
    let flow = |down: usize| {
        let host = host();
        let mut world = World::default();
        world.snapshot.agents.as_mut().unwrap().push(AgentRow {
            name: "shell".into(),
        });
        open(&host, &world);
        // `/src/notes`: no repository, so no worktree to make here.
        press(&host, &world, "down");
        press(&host, &world, "enter");
        press(&host, &world, "enter");
        for _ in 0..down {
            press(&host, &world, "down");
        }
        press(&host, &world, "enter");
        host.drain_commands()
    };

    let codex = StubFleet::new("[\"local\"]");
    codex.run(flow(1)).expect("codex has a profile");
    assert_eq!(codex.applied()["spec"]["harness"], "codex");

    let shell = StubFleet::new("[\"local\"]");
    let commands = flow(2);
    assert!(
        matches!(&commands[..], [Command::Create { agent: Some(a), .. }] if a == "shell"),
        "{commands:?}"
    );
    let refused = shell.run(commands).expect_err("no harness for shell");
    assert!(
        refused.contains("shell") && refused.contains("harness"),
        "{refused}"
    );
    assert!(!shell.asked(), "nothing was applied");
}

#[test]
fn an_untouched_name_takes_the_repository_it_just_picked() {
    // The step used to open on an empty field with no default and no placeholder,
    // one step after the repository — the obvious answer — had been chosen. Enter
    // straight through now uses the repository's own leaf, and the placeholder
    // shows what that will be before the key is pressed.
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "space");
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("thurbox"),
        "the suggestion is visible while the field is empty: {screen}"
    );

    press(&host, &world, "enter");
    assert!(
        drawn(&host, &world).contains("Coding Agent"),
        "an untouched field is a valid answer, so the flow advances"
    );
    press(&host, &world, "enter");
    assert_eq!(
        host.drain_commands(),
        vec![Command::Create {
            // Named after the repository rather than left unnamed.
            name: "thurbox".into(),
            repo: "/src/thurbox".into(),
            agent: Some("claude".into()),
            host: None,
            worktree: Some(false),
            branch: None,
            base: None,
            extras: Vec::new(),
        }]
    );
}

#[test]
fn a_name_is_still_refused_when_there_is_nothing_to_name_it_after() {
    // The guarantee the old test was really protecting: nothing is ever created
    // unnamed. With no repository selected the flow falls back to the home
    // directory, which is no kind of session name, so there is no default to take
    // and the refusal stands.
    let host = host();
    let world = world_with(Vec::new());
    open(&host, &world);
    // An empty list: nothing to tick or point at, so `enter` takes home.
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(screen.contains("cannot be empty"), "{screen}");
    assert!(
        screen.contains("Session Name"),
        "the step stays open: {screen}"
    );
    assert!(host.drain_commands().is_empty());
}

#[test]
fn a_host_is_carried_into_the_create_and_scopes_the_memory() {
    let host = host();
    let mut world = World::default();
    world.snapshot.hosts = Some(vec![
        local_host(),
        HostRow {
            name: "devbox".into(),
            detail: "me@devbox".into(),
            backend: "ssh:devbox".into(),
        },
    ]);
    // The memory that matters here is the HOST's, not the local machine's.
    world
        .repos
        .set_bookmarks_for_test("ssh:devbox", vec![bookmark("/srv/thurbox", Some(true))]);
    press(&host, &world, "ctrl+n");
    press(&host, &world, "j"); // local → devbox
    press(&host, &world, "enter");

    // Repository memory is scoped to the machine the repositories live on.
    assert_eq!(
        host.shared_string("want_bookmarks").as_deref(),
        Some("ssh:devbox")
    );
    world.wants.bookmarks = Some("ssh:devbox".into());
    assert!(drawn(&host, &world).contains("Repos on ssh:devbox"));

    press(&host, &world, "space");
    press(&host, &world, "enter");
    type_text(&host, &world, "remote");
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    let issued = host.drain_commands();
    let Some(Command::Create { host: target, .. }) = issued.first() else {
        panic!("expected a create, got {issued:?}");
    };
    // The BACKEND name, because that is the key repository memory is scoped by
    // and the flow carries one host string, not two. `spawn::resolve_host` is
    // therefore required to accept this spelling as well as the bare `devbox`
    // the CLI's `--host` takes — it accepted only the bare one once, and every
    // session created on a host failed with "Unknown host 'ssh:devbox'".
    assert_eq!(target.as_deref(), Some("ssh:devbox"));
}

#[test]
fn nothing_to_pick_locally_still_creates_a_session() {
    // v1 spawns in the home directory when no repository is chosen. With rows on
    // screen the cursor row is the choice, so that fallback is an empty list's.
    let host = host();
    let world = world_with(Vec::new());
    open(&host, &world);
    press(&host, &world, "enter");
    assert!(drawn(&host, &world).contains("Session Name"));
    type_text(&host, &world, "scratch");
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    let issued = host.drain_commands();
    let Some(Command::Create { repo, .. }) = issued.first() else {
        panic!("expected a create, got {issued:?}");
    };
    assert_eq!(repo, "~", "the home directory, as v1 does");
}

/// Fleet's agents arrive from a worker. A name confirmed before they have
/// is not a choice of no agent: the flow waits on the agent step, issues
/// nothing, and goes on once Fleet answers.
#[test]
fn a_name_confirmed_before_fleets_agents_arrive_waits_for_them() {
    let host = host();
    let mut world = World::default();
    world.snapshot.agents = None;
    open(&host, &world);
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    type_text(&host, &world, "early");
    press(&host, &world, "enter");
    assert!(host.drain_commands().is_empty(), "nothing is created yet");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Asking Fleet"), "{screen}");

    world.snapshot.agents = Some(vec![AgentRow {
        name: "claude".into(),
    }]);
    press(&host, &world, "enter");
    let issued = host.drain_commands();
    assert!(
        matches!(issued.first(), Some(Command::Create { agent, .. }) if agent.as_deref() == Some("claude")),
        "{issued:?}"
    );
}

#[test]
fn one_agent_is_not_a_question() {
    let host = host();
    let mut world = World::default();
    world.snapshot.agents = Some(vec![AgentRow {
        name: "claude".into(),
    }]);
    open(&host, &world);
    press(&host, &world, "space");
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    type_text(&host, &world, "solo");
    press(&host, &world, "enter");
    let issued = host.drain_commands();
    assert!(
        matches!(issued.first(), Some(Command::Create { agent, .. }) if agent.as_deref() == Some("claude")),
        "with one agent the flow creates rather than asking: {issued:?}"
    );
    assert_eq!(drawn(&host, &world), "");
}

// ── What the flow is, structurally ─────────────────────────────────────────

#[test]
fn the_empty_session_list_names_the_chord_that_creates_one() {
    // v1's empty state is two lines: "No sessions yet" and the chord. The second
    // was omitted while nothing answered `ctrl+n`; now that the flow does, it is
    // back — and it reads the chord out of the registry, so a rebind or a removed
    // flow cannot leave it advertising a dead key.
    let host = host();
    let world = World::default();
    publish(&host, &world);
    let index = index_of(&host, "sessions");
    let rendered = host
        .render(
            index,
            RenderContext {
                width: 30,
                height: 10,
                focused: false,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render");
    let mut terminal = Terminal::new(TestBackend::new(30, 10)).expect("terminal");
    terminal
        .draw(|frame| {
            thurbox::kernel::paint::render_recording(
                frame,
                Rect::new(0, 0, 30, 10),
                &rendered.node,
                &thurbox::kernel::terminal::Terminals::with_registry(
                    std::sync::Arc::new(
                        thurbox::backend::wiring::configured(&Default::default()).0,
                    ),
                    &Default::default(),
                ),
                &mut Vec::new(),
            );
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let screen: String = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(screen.contains("No sessions yet"), "{screen}");
    assert!(screen.contains("ctrl+n"), "{screen}");
}

#[test]
fn what_the_flow_asks_for_is_what_the_loop_reads() {
    // The two halves of the request wire format, checked against each other: the
    // flow writes `store.want_*`, and `Wants::new` is what the loop turns those
    // strings back into. A format change on either side fails here rather than in
    // a pane that silently never gets its rows.
    let host = host();
    let mut world = World::default();
    world.snapshot.hosts = Some(vec![
        local_host(),
        HostRow {
            name: "devbox".into(),
            detail: "me@devbox".into(),
            backend: "ssh:devbox".into(),
        },
    ]);
    world
        .repos
        .set_bookmarks_for_test("ssh:devbox", vec![bookmark("/srv/thurbox", Some(true))]);
    press(&host, &world, "ctrl+n");
    press(&host, &world, "j");
    press(&host, &world, "enter");
    press(&host, &world, "tab");
    type_text(&host, &world, "/srv/th");

    let wants = Wants::new(
        host.shared_string("want_bookmarks"),
        host.shared_string("want_browse"),
    );
    assert_eq!(wants.bookmarks.as_deref(), Some("ssh:devbox"));
    assert_eq!(wants.browse, Some(("ssh:devbox".into(), "/srv".into())));

    // And serving those wants is what the loop does with them — idempotently, so
    // a flow that asks every frame costs one request.
    world.repos.serve(&wants);
    world.repos.serve(&wants);
    assert!(
        world.repos.listing("ssh:devbox", "/srv").is_some(),
        "the request the flow made is the one the store received"
    );
}

#[test]
fn the_flow_is_a_float_that_never_joins_the_focus_ring() {
    let host = host();
    let index = index_of(&host, PLUGIN);
    let plugin = &host.plugins[index];
    assert!(plugin.floats, "the flow overlays rather than taking a slot");
    assert!(
        !plugin.focusable,
        "a closed modal must not be a tab stop, as v1's pickers are not"
    );
}

#[test]
fn every_key_the_flow_uses_is_declared_rather_than_only_handled() {
    // Declared keys are what help can list and a user can rebind; the fixed ones
    // (enter/esc/tab) are v1's fixed ones too.
    let host = host();
    let index = index_of(&host, PLUGIN);
    let declared: Vec<&str> = host.plugins[index]
        .bindings
        .iter()
        .map(|binding| binding.chord.as_str())
        .collect();
    for chord in ["ctrl+n", "j", "k", "space", "alt+d", "delete", "alt+p"] {
        assert!(
            declared.contains(&chord),
            "{chord} is not declared: {declared:?}"
        );
    }
}

#[test]
fn the_arrows_pick_a_host_as_well_as_j_and_k() {
    // Only `j`/`k` were declared, so anyone who reached for an arrow first — and
    // every other list in this interface takes both — found a picker that looked
    // broken.
    let host = host();
    let mut world = World::default();
    world.snapshot.hosts = Some(vec![
        local_host(),
        HostRow {
            name: "devbox".into(),
            detail: "me@devbox".into(),
            backend: "ssh:devbox".into(),
        },
        HostRow {
            name: "builder".into(),
            detail: "me@builder".into(),
            backend: "ssh:builder".into(),
        },
    ]);
    press(&host, &world, "ctrl+n");

    // The cursor starts on the local machine; one step down reaches the first
    // host, whichever key does the stepping.
    press(&host, &world, "down");
    let with_arrow = drawn(&host, &world);
    press(&host, &world, "up");
    press(&host, &world, "j");
    let with_letter = drawn(&host, &world);
    assert_eq!(
        with_arrow, with_letter,
        "an arrow and `j` must land on the same choice"
    );
}

#[test]
fn the_arrows_still_move_the_list_while_a_field_has_focus() {
    // On the repository step the filter takes letters, so `j` types a `j`. An
    // arrow cannot be typed, so it keeps moving the selection — otherwise
    // picking a row means leaving the field first, which v1 never asked of
    // anyone.
    //
    // The cursor is drawn by styling, which the text dump cannot see, so the
    // selection is observed through `space`, which checks the row under it —
    // after `enter` leaves the field, since a space is a character the filter
    // is entitled to take.
    let host = host();
    let world = world_with(vec![
        bookmark("/src/alpha", Some(true)),
        bookmark("/src/beta", Some(true)),
    ]);
    // No hosts configured, so the flow opens on the repository step.
    open(&host, &world);

    press(&host, &world, "down");
    press(&host, &world, "space");
    let picked = drawn(&host, &world);
    assert!(
        picked.contains("[x] /src/beta"),
        "the arrow moved the selection onto the second row: {picked}"
    );
    assert!(
        picked.contains("[ ] /src/alpha"),
        "and off the first: {picked}"
    );
}

// ── What the confirm pill says ─────────────────────────────────────────────
//
// The pills replay `enter` and `esc`, so their labels are claims about what
// those keys do. Both keys mean something different in nearly every state of
// this flow, and a fixed pair of labels made most of those claims false.

#[test]
fn the_repository_list_offers_the_next_step_rather_than_done() {
    // `enter` on the list carries the ticked repositories into the next
    // question; it does not finish anything, and there are three more steps
    // behind it.
    let h = host();
    let world = World::default();
    open(&h, &world);
    let screen = drawn(&h, &world);
    assert!(screen.contains("[ Next ]"), "{screen}");
}

#[test]
fn the_repo_step_footer_names_every_key_it_offers_in_full() {
    // The hints share one row with the pills, and the ones that did not fit
    // were cut off at the edge — forgetting a repository was the one hint
    // nobody could see.
    let h = host();
    let world = World::default();
    open(&h, &world);
    let screen = drawn(&h, &world);
    let footer = screen
        .lines()
        .find(|line| line.contains("[ Next ]"))
        .unwrap_or_else(|| panic!("no footer: {screen}"));
    for hint in ["nav", "forget", "path"] {
        assert!(screen.contains(hint), "`{hint}` is not shown: {screen}");
    }
    assert!(footer.contains("[ Cancel ]"), "{footer}");
}

#[test]
fn the_typed_path_field_offers_to_add_the_repository() {
    // `enter` here adds what was typed to memory and leaves the flow on this
    // very step — the one place the old label read most like "finish". An empty
    // field has nothing to add, so it offers no pill rather than an inert one.
    let h = host();
    let world = World::default();
    open(&h, &world);
    press(&h, &world, "tab");
    let empty = drawn(&h, &world);
    assert!(
        !empty.contains("[ Add repo ]"),
        "nothing typed yet — the starting directory is not a choice: {empty}"
    );
    assert!(!empty.contains("[ Next ]"), "{empty}");

    type_text(&h, &world, "/srv/thing");
    let screen = drawn(&h, &world);
    assert!(screen.contains("[ Add repo ]"), "{screen}");
    // `enter` adds, so the key that goes on from here is named beside it.
    assert!(screen.contains("alt+⏎ next"), "{screen}");
}

#[test]
fn the_search_pills_name_the_filter_they_act_on() {
    // With a query typed `esc` clears it rather than closing the flow, so the
    // dismiss pill must not claim to cancel — until there is nothing to clear.
    let h = host();
    let world = World::default();
    open(&h, &world);
    assert!(drawn(&h, &world).contains("[ Cancel ]"));
    type_text(&h, &world, "src");
    let screen = drawn(&h, &world);
    assert!(screen.contains("[ Next ]"), "{screen}");
    assert!(screen.contains("[ Clear ]"), "{screen}");
    assert!(!screen.contains("[ Cancel ]"), "{screen}");
}

#[test]
fn tab_descends_into_the_highlighted_directory_without_adding_it() {
    let h = host();
    let mut world = World::default();
    world.repos.set_listing_for_test(
        "",
        "/srv",
        Listing::Ready(vec![
            BrowseEntry {
                name: "notes".into(),
                is_git: false,
            },
            BrowseEntry {
                name: "repo".into(),
                is_git: true,
            },
        ]),
    );
    world.wants.browse = Some((String::new(), "/srv".into()));
    open(&h, &world);
    press(&h, &world, "tab");
    type_text(&h, &world, "/srv/");
    press(&h, &world, "tab");
    press(&h, &world, "down");
    press(&h, &world, "tab");
    assert!(drawn(&h, &world).contains("/srv/repo/"));
    assert!(
        h.drain_commands().is_empty(),
        "browsing never adds a bookmark"
    );
}

#[test]
fn the_browse_pill_offers_to_add_plain_directories_and_repositories() {
    let h = host();
    let mut world = World::default();
    world.repos.set_listing_for_test(
        "",
        "/srv",
        Listing::Ready(vec![
            BrowseEntry {
                name: "repos".into(),
                is_git: false,
            },
            BrowseEntry {
                name: "thing".into(),
                is_git: true,
            },
        ]),
    );
    world.wants.browse = Some((String::new(), "/srv".into()));
    open(&h, &world);
    press(&h, &world, "tab");
    type_text(&h, &world, "/srv/");
    press(&h, &world, "tab");
    let screen = drawn(&h, &world);
    assert!(
        screen.contains("[ Add directory ]"),
        "a plain directory: {screen}"
    );
    assert!(screen.contains("[ Close ]"), "{screen}");
    assert!(!screen.contains("[ Cancel ]"), "{screen}");

    press(&h, &world, "down");
    let screen = drawn(&h, &world);
    assert!(screen.contains("[ Add repo ]"), "a repository: {screen}");
}

#[test]
fn a_listing_that_has_not_arrived_offers_no_pill_to_press() {
    let h = host();
    let mut world = World::default();
    world
        .repos
        .set_listing_for_test("", "/srv", Listing::Pending);
    world.wants.browse = Some((String::new(), "/srv".into()));
    open(&h, &world);
    press(&h, &world, "tab");
    type_text(&h, &world, "/srv/");
    press(&h, &world, "tab");
    let screen = drawn(&h, &world);
    assert!(
        !screen.contains("[ Open ]") && !screen.contains("[ Add repo ]"),
        "there is no row to act on yet: {screen}"
    );
}

#[test]
fn the_last_question_says_that_answering_it_creates_the_session() {
    // The agent step spawns on `enter`; it does not merely settle the agent.
    let h = host();
    let world = World::default();
    open(&h, &world);
    press(&h, &world, "space");
    press(&h, &world, "enter");
    let named = drawn(&h, &world);
    assert!(
        named.contains("Session Name") && named.contains("[ Next ]"),
        "the agent is still to come: {named}"
    );
    type_text(&h, &world, "x");
    press(&h, &world, "enter");
    let screen = drawn(&h, &world);
    assert!(
        screen.contains("Coding Agent") && screen.contains("[ Create ]"),
        "{screen}"
    );
}

#[test]
fn a_name_that_is_the_last_question_says_so_too() {
    // One agent is not a question, so `enter` on the name field spawns.
    let h = host();
    let mut world = World::default();
    world.snapshot.agents.as_mut().unwrap().truncate(1);
    open(&h, &world);
    press(&h, &world, "space");
    press(&h, &world, "enter");
    let screen = drawn(&h, &world);
    assert!(
        screen.contains("Session Name") && screen.contains("[ Create ]"),
        "{screen}"
    );
}

#[test]
fn a_host_with_nothing_ticked_offers_nothing_to_advance_to() {
    // `enter` carries the ticked rows, and with none ticked on a host
    // `after_repos` refuses: there is no local home to stand in for a
    // repository that has to exist on the other machine. An empty memory on a
    // host is that state permanently, until a path is added.
    let h = host();
    let mut world = World::default();
    world.snapshot.hosts = Some(vec![
        local_host(),
        HostRow {
            name: "devbox".into(),
            detail: "me@devbox".into(),
            backend: "ssh:devbox".into(),
        },
    ]);
    world.repos.set_bookmarks_for_test("ssh:devbox", Vec::new());
    press(&h, &world, "ctrl+n");
    press(&h, &world, "j"); // local → devbox
    press(&h, &world, "enter"); // devbox: on to its repositories
    world.wants.bookmarks = Some("ssh:devbox".into());

    let empty = drawn(&h, &world);
    assert!(empty.contains("No bookmarks"), "the list is empty: {empty}");
    assert!(
        !empty.contains("[ Next ]"),
        "nothing to advance to on a host: {empty}"
    );

    // A row on screen is a choice even unticked: `enter` takes the cursor row.
    world
        .repos
        .set_bookmarks_for_test("ssh:devbox", vec![bookmark("/srv/thurbox", Some(true))]);
    let listed = drawn(&h, &world);
    assert!(listed.contains("[ Next ]"), "{listed}");
}

#[test]
fn ctrl_enter_confirms_from_the_path_field() {
    // No trip back to the list first: the ticked rows go on from any focus.
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "space");
    press(&host, &world, "tab");
    type_text(&host, &world, "/half/typed");
    press(&host, &world, "ctrl+enter");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Session Name"), "{screen}");
}

#[test]
fn alt_enter_is_the_same_confirm_for_terminals_that_cannot_send_ctrl_enter() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "space");
    press(&host, &world, "tab");
    press(&host, &world, "alt+enter");
    assert!(drawn(&host, &world).contains("Session Name"));
}

#[test]
fn with_nothing_ticked_the_cursor_row_is_the_choice() {
    // Type part of a name, confirm: the best match is the repository, with no
    // `space` in between.
    let host = host();
    let world = World::default();
    open(&host, &world);
    type_text(&host, &world, "note");
    press(&host, &world, "ctrl+enter");
    type_text(&host, &world, "n");
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    let issued = host.drain_commands();
    let Some(Command::Create { repo, .. }) = issued.first() else {
        panic!("expected a create, got {issued:?}");
    };
    assert_eq!(repo, "/src/notes");
}

#[test]
fn nothing_ticked_locally_still_offers_the_next_step() {
    // The local half of the rule above: `after_repos` falls back to the home
    // directory, so `enter` really does advance and the pill must stay.
    let h = host();
    let world = World::default();
    open(&h, &world);
    let screen = drawn(&h, &world);
    assert!(screen.contains("[ Next ]"), "{screen}");
}

#[test]
fn a_name_with_no_default_to_fall_back_on_offers_no_pill() {
    // Nothing selected locally, so the flow names the home directory — which is
    // no kind of session name, leaving the field with an empty value AND an
    // empty placeholder. `enter` is refused there, so nothing is offered.
    let h = host();
    let world = world_with(Vec::new());
    open(&h, &world);
    press(&h, &world, "enter");
    let screen = drawn(&h, &world);
    assert!(screen.contains("Session Name"), "{screen}");
    assert!(
        !screen.contains("[ Create ]") && !screen.contains("[ Next ]"),
        "validation would refuse this: {screen}"
    );

    // Two agents in this world, so the agent step is still ahead of it.
    type_text(&h, &world, "x");
    let typed = drawn(&h, &world);
    assert!(typed.contains("[ Next ]"), "{typed}");
}

#[test]
fn a_name_the_repository_can_answer_for_keeps_its_pill() {
    // The other side of it: an untouched field whose placeholder IS the answer
    // `enter` takes, so the pill is honest about the empty field.
    let h = host();
    let world = World::default();
    open(&h, &world);
    press(&h, &world, "space");
    press(&h, &world, "enter");
    let screen = drawn(&h, &world);
    assert!(screen.contains("thurbox"), "the suggestion shows: {screen}");
    assert!(screen.contains("[ Next ]"), "{screen}");
}

// ── What is missing, said before the user commits ──────────────────────────

// ── Left click and hover, field by field ───────────────────────────────────
//
// A press is resolved against the hitboxes the paint recorded, exactly as the
// loop resolves it, so each test proves both halves: the field is a target, and
// pressing it does what its key already does. "What its key does" is checked
// by comparing against a second flow driven by that key alone.

fn browse_world() -> World {
    let mut world = World::default();
    world.repos.set_listing_for_test(
        "",
        "/srv",
        Listing::Ready(vec![
            BrowseEntry {
                name: "repos".into(),
                is_git: false,
            },
            BrowseEntry {
                name: "other".into(),
                is_git: true,
            },
        ]),
    );
    world.wants.browse = Some((String::new(), "/srv".into()));
    world
}

#[test]
fn clicking_the_path_field_focuses_it_as_tab_does() {
    let world = World::default();
    let clicked = host();
    open(&clicked, &world);
    let hit = click_on(&clicked, &world, "Add Repo Path", 0);
    assert!(
        hit.id.is_some(),
        "the path field is a click target: {hit:?}"
    );
    type_text(&clicked, &world, "zz");

    let keyed = host();
    open(&keyed, &world);
    press(&keyed, &world, "tab");
    type_text(&keyed, &world, "zz");

    let screen = drawn(&clicked, &world);
    assert_eq!(screen, drawn(&keyed, &world), "a click is tab, no more");
    assert!(screen.contains("zz"), "typing went to the path: {screen}");
}

#[test]
fn clicking_the_search_field_takes_focus_back_as_backtab_does() {
    let world = World::default();
    let clicked = host();
    open(&clicked, &world);
    press(&clicked, &world, "tab");
    click_on(&clicked, &world, "Search (", 0);
    type_text(&clicked, &world, "no");

    let keyed = host();
    open(&keyed, &world);
    press(&keyed, &world, "tab");
    press(&keyed, &world, "backtab");
    type_text(&keyed, &world, "no");

    let screen = drawn(&clicked, &world);
    assert_eq!(screen, drawn(&keyed, &world), "a click is backtab, no more");
    assert!(
        screen.contains("(1/2)"),
        "typing filtered the list: {screen}"
    );
}

#[test]
fn clicking_a_browsed_folder_selects_it_as_the_arrows_do() {
    let world = browse_world();
    let open_browse = |host: &LuaHost| {
        open(host, &world);
        press(host, &world, "tab");
        type_text(host, &world, "/srv/");
        press(host, &world, "tab");
    };
    let clicked = host();
    open_browse(&clicked);
    let hit = click_on(&clicked, &world, "other/", 0);
    assert!(hit.id.is_some(), "a browsed folder is a click target");

    let keyed = host();
    open_browse(&keyed);
    press(&keyed, &world, "down");

    assert_eq!(drawn(&clicked, &world), drawn(&keyed, &world));
    // And `enter` then acts on the clicked row, as it would on the arrowed one.
    press(&clicked, &world, "enter");
    press(&keyed, &world, "enter");
    let issued = |host: &LuaHost| format!("{:?}", host.drain_commands());
    let command = issued(&clicked);
    assert!(
        command.contains("/srv/other"),
        "enter adds the clicked repo: {command}"
    );
    assert_eq!(command, issued(&keyed));
}

/// The flow as painted with the pointer over `hovered`, for its colours.
fn colours(host: &LuaHost, world: &World, hovered: Option<&Identity>) -> Buffer {
    painted(host, world, hovered).expect("the flow is open").0
}

/// The identity drawn at `needle`, as the kernel would publish it on hover.
fn identity_of(host: &LuaHost, world: &World, needle: &str, below: u16) -> Identity {
    let (buffer, hits) = painted(host, world, None).expect("the flow is open");
    let (x, y) = locate(&buffer, needle);
    identity_at(&hits, x, y + below)
}

#[test]
fn a_hovered_repository_row_is_banded_and_keeps_its_colours() {
    let world = World::default();
    let host = host();
    open(&host, &world);
    // The second row: the first carries the cursor, whose bar would hide a band.
    let hovered = identity_of(&host, &world, "notes", 0);
    assert!(hovered.id.is_some(), "a repository row is a target");
    let resting = colours(&host, &world, None);
    let lit = colours(&host, &world, Some(&hovered));
    let (_, y) = locate(&resting, "notes");
    let row = |b: &Buffer| {
        (0..b.area.width)
            .map(|x| b[(x, y)].clone())
            .collect::<Vec<_>>()
    };
    assert!(
        row(&resting)
            .iter()
            .zip(row(&lit))
            .any(|(a, b)| a.bg != b.bg),
        "hovering the row bands it"
    );
    assert!(
        row(&resting)
            .iter()
            .zip(row(&lit))
            .all(|(a, b)| a.fg == b.fg),
        "a band repaints no foreground"
    );
    let (_, first) = locate(&resting, "thurbox");
    assert!(
        (0..resting.area.width).all(|x| resting[(x, first)] == lit[(x, first)]),
        "the row under the cursor is untouched"
    );
}

#[test]
fn a_hovered_host_row_is_banded() {
    let mut world = World::default();
    world.snapshot.hosts = Some(vec![
        local_host(),
        HostRow {
            name: "devbox".into(),
            detail: "me@devbox".into(),
            backend: "ssh:devbox".into(),
        },
    ]);
    let host = host();
    press(&host, &world, "ctrl+n");
    let hovered = identity_of(&host, &world, "devbox", 0);
    assert!(hovered.id.is_some(), "a host row is a target");
    let resting = colours(&host, &world, None);
    let lit = colours(&host, &world, Some(&hovered));
    let (x, y) = locate(&resting, "devbox");
    assert_ne!(
        resting[(x, y)].bg,
        lit[(x, y)].bg,
        "hovering the row bands it"
    );
}

#[test]
fn a_hovered_browsed_folder_is_banded() {
    let world = browse_world();
    let host = host();
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "/srv/");
    press(&host, &world, "tab");
    let hovered = identity_of(&host, &world, "other/", 0);
    let resting = colours(&host, &world, None);
    let lit = colours(&host, &world, Some(&hovered));
    let (x, y) = locate(&resting, "other/");
    assert_ne!(
        resting[(x, y)].bg,
        lit[(x, y)].bg,
        "hovering the folder bands it"
    );
}

#[test]
fn a_hovered_field_lights_its_border_and_focus_outranks_it() {
    let world = World::default();
    let host = host();
    open(&host, &world);
    let path = identity_of(&host, &world, "Add Repo Path", 0);
    let resting = colours(&host, &world, None);
    let lit = colours(&host, &world, Some(&path));
    let (x, y) = locate(&resting, "Add Repo Path");
    // The corner, which is border and nothing else.
    let corner = (x - 2, y);
    assert_ne!(
        resting[corner].fg, lit[corner].fg,
        "hovering the unfocused field lights its border"
    );

    // The focused search field already wears the focused border: hovering it
    // must not replace that with the weaker hover colour.
    let search = identity_of(&host, &world, "Search (", 0);
    let (sx, sy) = locate(&resting, "Search (");
    let search_corner = (sx - 2, sy);
    let lit_search = colours(&host, &world, Some(&search));
    assert_eq!(
        resting[search_corner].fg, lit_search[search_corner].fg,
        "the focused field keeps its focused border under the pointer"
    );
}

#[test]
fn a_hovered_pill_lights_and_its_neighbour_does_not() {
    let world = World::default();
    let host = host();
    open(&host, &world);
    let cancel = identity_of(&host, &world, "[ Cancel ]", 0);
    assert_eq!(cancel.role.as_deref(), Some("key:esc"));
    let resting = colours(&host, &world, None);
    let lit = colours(&host, &world, Some(&cancel));
    let (x, y) = locate(&resting, "[ Cancel ]");
    assert_ne!(
        resting[(x, y)].bg,
        lit[(x, y)].bg,
        "the hovered pill lights"
    );
    let (px, py) = locate(&resting, "[ Next ]");
    assert_eq!(
        resting[(px, py)],
        lit[(px, py)],
        "the pill beside it is untouched"
    );
}

#[test]
fn a_bookmark_named_like_a_reserved_target_is_still_selected_by_a_click() {
    // A remembered path can be relative, and a relative path can be any string
    // — including one spelled like the flow's own field or folder targets.
    let world = World {
        repos: store_with(vec![
            bookmark("/src/thurbox", Some(true)),
            bookmark("browse:notes", Some(false)),
            bookmark("field:input", Some(false)),
        ]),
        ..World::default()
    };
    let pick = |needle: &str, downs: usize| {
        let clicked = host();
        open(&clicked, &world);
        click_on(&clicked, &world, needle, 0);
        let keyed = host();
        open(&keyed, &world);
        for _ in 0..downs {
            press(&keyed, &world, "down");
        }
        assert_eq!(drawn(&clicked, &world), drawn(&keyed, &world), "{needle}");
    };
    pick("browse:notes", 1);
    pick("field:input", 2);
}
