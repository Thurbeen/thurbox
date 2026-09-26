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
use ratatui::layout::Rect;
use ratatui::Terminal;

use thurbox::agent::preflight::Presence;
use thurbox::git::ExistingWorktree;
use thurbox::kernel::command::{BookmarkEdit, Command, InFlight, Phase};
use thurbox::kernel::host::{KeyPress, LuaHost, Published, RenderContext};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::repos::{
    BookmarkRow, Branches, BrowseEntry, Listing, RepoStore, Wants, Worktrees,
};
use thurbox::kernel::snapshot::{AgentRow, HostRow, SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;
use thurbox::session::SessionState;

const PLUGIN: &str = "new_session";

fn host() -> LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

fn index_of(host: &LuaHost, name: &str) -> usize {
    host.index_of(name)
        .unwrap_or_else(|| panic!("no {name} plugin"))
}

fn snapshot() -> Snapshot {
    Snapshot {
        agents: vec![
            AgentRow {
                name: "claude".into(),
                command: "claude".into(),
                presence: Presence::Present,
            },
            AgentRow {
                name: "codex".into(),
                command: "codex".into(),
                presence: Presence::Present,
            },
        ],
        agent_default: "claude".into(),
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

/// A session to fork — its own agent is what the fork would actually run,
/// never the first row of `agents()`.
fn source_session() -> SessionRow {
    SessionRow {
        id: "fix-osc52-0000-0000-0000-000000000000".into(),
        name: "fix-osc52".into(),
        agent: "aider".into(),
        status: SessionState::Idle,
        cwd: Some("/src/thurbox".into()),
        repo: Some("thurbox".into()),
        repos: Vec::new(),
        member_dirs: Vec::new(),
        branch: Some("feat/fix-osc52".into()),
        base_branch: None,
        backend: "local-tmux".into(),
        backend_id: Some("%1".into()),
        remote_host: None,
        agent_session_id: None,
        parent_id: None,
        display_order: None,
        worktree_count: 1,
        git: None,
        stopped: false,
        hook_state: None,
        reports_as: None,
        detected_agent: None,
        shell_backend_id: None,
    }
}

/// A store holding the three reads, as if every worker had already answered.
fn store_with(rows: Vec<BookmarkRow>) -> RepoStore {
    let mut repos = RepoStore::with_hosts(Default::default());
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
        hovered: None,
        printing: &Default::default(),
    })
    .expect("publish");
}

/// What the flow draws, as text. Empty when it is closed — a closed modal draws
/// nothing at all, which is how the kernel knows it is not floating.
fn drawn(host: &LuaHost, world: &World) -> String {
    publish(host, world);
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
    let Some(float) = rendered.float else {
        return String::new();
    };
    // The flow asks in cells for both, and cells win over the percentage just
    // as they do in the kernel — a dump wider than the real modal hides every
    // truncation the user would see.
    let rows = float.rows.expect("the flow sizes its own height");
    let width = float
        .cols
        .unwrap_or((120.0 * float.width_pct / 100.0) as u16);
    let mut terminal = Terminal::new(TestBackend::new(width, rows)).expect("terminal");
    terminal
        .draw(|frame| {
            thurbox::kernel::paint::render_recording(
                frame,
                Rect::new(0, 0, width, rows),
                &rendered.node,
                &thurbox::kernel::terminal::Terminals::new(),
                &mut Vec::new(),
            );
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
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

fn open(host: &LuaHost, world: &World) {
    press(host, world, "ctrl+n");
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

#[test]
fn opening_with_no_hosts_starts_at_the_repositories() {
    // v1 skips the host question entirely rather than offering one answer.
    let host = host();
    let world = World::default();
    open(&host, &world);
    let screen = drawn(&host, &world);
    assert!(screen.contains("Select Repos"), "{screen}");
}

#[test]
fn opening_with_hosts_asks_where_to_run_first() {
    let host = host();
    let mut world = World::default();
    world.snapshot.hosts = vec![HostRow {
        name: "devbox".into(),
        detail: "me@devbox".into(),
        backend: "ssh:devbox".into(),
    }];
    open(&host, &world);
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
    assert_eq!(host.shared_string("want_branches"), None);
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
    assert!(screen.contains("[ ] /src/thurbox"), "{screen}");
    // A known non-repository reads as a plain directory.
    assert!(screen.contains("/src/notes"), "{screen}");
    assert!(screen.contains("(dir)"), "{screen}");
}

#[test]
fn space_selects_and_alt_w_gives_it_a_worktree() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "space");
    assert!(drawn(&host, &world).contains("[x] /src/thurbox"));
    press(&host, &world, "alt+w");
    let screen = drawn(&host, &world);
    assert!(screen.contains("[wt]"), "{screen}");
}

#[test]
fn worktree_mode_is_refused_for_a_directory_that_is_not_a_repository() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "down");
    press(&host, &world, "alt+w");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("Not a git repo"),
        "the refusal is shown, not silent: {screen}"
    );
    assert!(!screen.contains("[wt]"), "{screen}");
    // And it stays selectable as a plain member.
    press(&host, &world, "space");
    assert!(drawn(&host, &world).contains("[x] /src/notes"));
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
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("/srv/beta/"),
        "the arrow moved the dropdown's own selection: {screen}"
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
        screen.contains("Select Repos"),
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
    assert!(drawn(&host, &world).contains("Select Repos"));
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
    assert!(screen.contains("Select Repos"), "{screen}");
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
    assert!(drawn(&host, &world).contains("Select Repos"));
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
    press(&host, &world, "space"); // /src/thurbox, no worktree
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("Session Name"),
        "no worktree means no branch question: {screen}"
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
            repo: "/src/thurbox".into(),
            branch: None,
            base: None,
            worktree_path: None,
            agent: Some("claude".into()),
            host: None,
            extras: Vec::new(),
        }]
    );
    assert_eq!(drawn(&host, &world), "", "the flow closes when it commits");
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
            branch: None,
            base: None,
            worktree_path: None,
            agent: Some("claude".into()),
            host: None,
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
fn an_existing_worktree_is_offered_under_its_repo_and_opens_with_no_questions() {
    // The whole feature in one flow: the repo the cursor is on has a worktree
    // an agent cut earlier, it shows as a child row, and choosing it asks for
    // neither a base branch, nor a session name, nor a branch name.
    let host = host();
    let mut world = World::default();
    world.repos.set_worktrees_for_test(
        "",
        "/src/thurbox",
        Worktrees::Ready(vec![ExistingWorktree {
            path: "/src/thurbox/.worktrees/dynamic-tooltips".into(),
            branch: "feat/dynamic-tooltips-15307729713678226529".into(),
        }]),
    );
    open(&host, &world);

    // The flow asks about whichever repo the cursor is on.
    assert_eq!(
        host.shared_string("want_worktrees").as_deref(),
        Some("\0/src/thurbox")
    );
    world.wants.worktrees = Some((String::new(), "/src/thurbox".into()));

    let screen = drawn(&host, &world);
    assert!(
        screen.contains("dynamic-tooltips"),
        "the existing worktree is listed under its repo: {screen}"
    );

    // Down onto the child row, then choose it.
    press(&host, &world, "down");
    press(&host, &world, "enter");
    press(&host, &world, "enter"); // agent step, default preselected

    assert_eq!(
        host.drain_commands(),
        vec![Command::Create {
            // Empty: the kernel names it after the worktree directory.
            name: String::new(),
            repo: "/src/thurbox".into(),
            branch: Some("feat/dynamic-tooltips-15307729713678226529".into()),
            // Nothing is branched off anything.
            base: None,
            worktree_path: Some("/src/thurbox/.worktrees/dynamic-tooltips".into()),
            agent: Some("claude".into()),
            host: None,
            extras: Vec::new(),
        }]
    );
}

#[test]
fn a_repo_with_no_existing_worktrees_is_unchanged() {
    // No child rows, and the create flow still asks its usual questions.
    let host = host();
    let mut world = World::default();
    world
        .repos
        .set_worktrees_for_test("", "/src/thurbox", Worktrees::Ready(Vec::new()));
    world.wants.worktrees = Some((String::new(), "/src/thurbox".into()));
    open(&host, &world);
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("Session Name"),
        "the plain flow is untouched: {screen}"
    );
}

#[test]
fn a_worktree_selection_asks_for_a_base_branch_and_a_branch_name() {
    let host = host();
    let mut world = World::default();
    world.repos.set_branches_for_test(
        "",
        "/src/thurbox",
        Branches::Ready(vec!["origin/main".into(), "main".into(), "feat/x".into()]),
    );
    open(&host, &world);
    press(&host, &world, "space");
    press(&host, &world, "alt+w");
    press(&host, &world, "enter");

    // The flow reaches the branch step and asks for the list; the answer is
    // published under the same key.
    assert_eq!(
        host.shared_string("want_branches").as_deref(),
        Some("\0/src/thurbox")
    );
    world.wants.branches = Some((String::new(), "/src/thurbox".into()));
    let screen = drawn(&host, &world);
    assert!(screen.contains("Base Branch"), "{screen}");
    assert!(
        screen.contains("▸ origin/main"),
        "the remote default leads the list: {screen}"
    );

    press(&host, &world, "enter");
    type_text(&host, &world, "Fix OSC 52");
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Branch Name"), "{screen}");
    assert!(
        screen.contains("fix-osc-52"),
        "prefilled with a branch-safe form of the name: {screen}"
    );

    press(&host, &world, "enter");
    press(&host, &world, "enter"); // agent step, default preselected
    assert_eq!(
        host.drain_commands(),
        vec![Command::Create {
            name: "Fix OSC 52".into(),
            repo: "/src/thurbox".into(),
            branch: Some("fix-osc-52".into()),
            base: Some("origin/main".into()),
            worktree_path: None,
            agent: Some("claude".into()),
            host: None,
            extras: Vec::new(),
        }]
    );
}

#[test]
fn a_second_repository_travels_as_an_extra_member_with_its_own_mode() {
    let host = host();
    let mut world = world_with(vec![
        bookmark("/src/thurbox", Some(true)),
        bookmark("/src/website", Some(true)),
        bookmark("/src/notes", Some(false)),
    ]);
    world
        .repos
        .set_branches_for_test("", "/src/thurbox", Branches::Ready(vec!["main".into()]));
    world.wants.branches = Some((String::new(), "/src/thurbox".into()));
    open(&host, &world);
    // thurbox: worktree. website: worktree. notes: attached as it is.
    press(&host, &world, "alt+w");
    press(&host, &world, "down");
    press(&host, &world, "alt+w");
    press(&host, &world, "down");
    press(&host, &world, "space");
    press(&host, &world, "enter");
    press(&host, &world, "enter"); // base branch
    type_text(&host, &world, "wide");
    press(&host, &world, "enter"); // name → branch name
    press(&host, &world, "enter"); // branch name → agent
    press(&host, &world, "enter"); // agent

    let issued = host.drain_commands();
    let Some(Command::Create { repo, extras, .. }) = issued.first() else {
        panic!("expected a create, got {issued:?}");
    };
    assert_eq!(repo, "/src/thurbox", "the first worktree repo is primary");
    assert_eq!(
        extras
            .iter()
            .map(|extra| (extra.path.as_str(), extra.worktree))
            .collect::<Vec<_>>(),
        vec![("/src/website", true), ("/src/notes", false)],
        "each member keeps the mode it was given"
    );
}

#[test]
fn a_host_is_carried_into_the_create_and_scopes_the_memory() {
    let host = host();
    let mut world = World::default();
    world.snapshot.hosts = vec![HostRow {
        name: "devbox".into(),
        detail: "me@devbox".into(),
        backend: "ssh:devbox".into(),
    }];
    // The memory that matters here is the HOST's, not the local machine's.
    world
        .repos
        .set_bookmarks_for_test("ssh:devbox", vec![bookmark("/srv/thurbox", Some(true))]);
    open(&host, &world);
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

#[test]
fn one_agent_is_not_a_question() {
    let host = host();
    let mut world = World::default();
    world.snapshot.agents = vec![AgentRow {
        name: "claude".into(),
        command: "claude".into(),
        presence: Presence::Present,
    }];
    open(&host, &world);
    press(&host, &world, "space");
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
                &thurbox::kernel::terminal::Terminals::new(),
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
    world.snapshot.hosts = vec![HostRow {
        name: "devbox".into(),
        detail: "me@devbox".into(),
        backend: "ssh:devbox".into(),
    }];
    world
        .repos
        .set_bookmarks_for_test("ssh:devbox", vec![bookmark("/srv/thurbox", Some(true))]);
    open(&host, &world);
    press(&host, &world, "j");
    press(&host, &world, "enter");
    press(&host, &world, "tab");
    type_text(&host, &world, "/srv/th");

    let wants = Wants::new(
        host.shared_string("want_bookmarks"),
        host.shared_string("want_browse"),
        host.shared_string("want_branches"),
        host.shared_string("want_worktrees"),
    );
    assert_eq!(wants.bookmarks.as_deref(), Some("ssh:devbox"));
    assert_eq!(wants.browse, Some(("ssh:devbox".into(), "/srv".into())));
    assert_eq!(wants.branches, None, "no branch is being asked for yet");

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
    for chord in [
        "ctrl+n", "j", "k", "space", "alt+w", "alt+d", "delete", "alt+p",
    ] {
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
    world.snapshot.hosts = vec![
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
    ];
    open(&host, &world);

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
    for hint in ["nav", "tick", "worktree", "forget", "path"] {
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
fn the_browse_pill_follows_the_row_the_dropdown_is_on() {
    // "open/pick" is two actions: a repository is remembered, a plain directory
    // is descended into. And `esc` closes the dropdown, not the flow.
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
    assert!(screen.contains("[ Open ]"), "a plain directory: {screen}");
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
fn an_existing_worktree_row_offers_to_open_it() {
    // The one row in this list that is not a thing to tick — but `enter` only
    // opens it straight away when nothing is left to ask; with 2+ agents
    // configured it still has the agent step ahead of it, same as every other
    // row.
    let h = host();
    let mut world = World::default();
    world.repos.set_worktrees_for_test(
        "",
        "/src/thurbox",
        Worktrees::Ready(vec![ExistingWorktree {
            path: "/src/thurbox/.worktrees/dynamic-tooltips".into(),
            branch: "feat/dynamic-tooltips".into(),
        }]),
    );
    open(&h, &world);
    world.wants.worktrees = Some((String::new(), "/src/thurbox".into()));
    assert!(drawn(&h, &world).contains("[ Next ]"), "on the repo row");
    press(&h, &world, "down");
    let screen = drawn(&h, &world);
    assert!(
        screen.contains("[ Next ]"),
        "an agent is still to come with 2+ agents configured: {screen}"
    );
}

#[test]
fn an_existing_worktree_row_offers_to_open_it_directly_with_one_agent() {
    // With only one agent configured there is nothing left to ask, so `enter`
    // on the worktree row spawns straight away.
    let h = host();
    let mut world = World::default();
    world.snapshot.agents.truncate(1);
    world.repos.set_worktrees_for_test(
        "",
        "/src/thurbox",
        Worktrees::Ready(vec![ExistingWorktree {
            path: "/src/thurbox/.worktrees/dynamic-tooltips".into(),
            branch: "feat/dynamic-tooltips".into(),
        }]),
    );
    open(&h, &world);
    world.wants.worktrees = Some((String::new(), "/src/thurbox".into()));
    press(&h, &world, "down");
    let screen = drawn(&h, &world);
    assert!(screen.contains("[ Open ]"), "{screen}");
}

#[test]
fn the_branch_step_offers_nothing_to_select_while_it_is_still_fetching() {
    let h = host();
    let mut world = World::default();
    world
        .repos
        .set_branches_for_test("", "/src/thurbox", Branches::Pending);
    open(&h, &world);
    press(&h, &world, "space");
    press(&h, &world, "alt+w");
    press(&h, &world, "enter");
    world.wants.branches = Some((String::new(), "/src/thurbox".into()));
    let screen = drawn(&h, &world);
    assert!(
        !screen.contains("[ Select ]"),
        "a pill here would do nothing when pressed: {screen}"
    );

    world.repos.set_branches_for_test(
        "",
        "/src/thurbox",
        Branches::Ready(vec!["origin/main".into(), "main".into()]),
    );
    let screen = drawn(&h, &world);
    assert!(
        screen.contains("[ Select ]"),
        "once there is a list: {screen}"
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
    world.snapshot.agents.truncate(1);
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
fn the_branch_name_is_not_the_last_question_when_an_agent_is_still_to_come() {
    let h = host();
    let mut world = World::default();
    world.repos.set_branches_for_test(
        "",
        "/src/thurbox",
        Branches::Ready(vec!["origin/main".into()]),
    );
    open(&h, &world);
    press(&h, &world, "space");
    press(&h, &world, "alt+w");
    press(&h, &world, "enter");
    world.wants.branches = Some((String::new(), "/src/thurbox".into()));
    press(&h, &world, "enter");
    let screen = drawn(&h, &world);
    assert!(
        screen.contains("Session Name") && screen.contains("[ Next ]"),
        "the branch name comes after the session name: {screen}"
    );
    type_text(&h, &world, "x");
    press(&h, &world, "enter");
    let screen = drawn(&h, &world);
    assert!(
        screen.contains("Branch Name") && screen.contains("[ Next ]"),
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
    world.snapshot.hosts = vec![HostRow {
        name: "devbox".into(),
        detail: "me@devbox".into(),
        backend: "ssh:devbox".into(),
    }];
    world.repos.set_bookmarks_for_test("ssh:devbox", Vec::new());
    open(&h, &world);
    press(&h, &world, "j"); // local → devbox
    press(&h, &world, "enter");
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
    let Some(Command::Create { repo, extras, .. }) = issued.first() else {
        panic!("expected a create, got {issued:?}");
    };
    assert_eq!(repo, "/src/notes");
    assert!(extras.is_empty());
}

#[test]
fn ticked_rows_win_over_the_cursor_row() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "space"); // ticks /src/thurbox
    press(&host, &world, "down"); // cursor on /src/notes, unticked
    press(&host, &world, "enter");
    type_text(&host, &world, "n");
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    let issued = host.drain_commands();
    let Some(Command::Create { repo, extras, .. }) = issued.first() else {
        panic!("expected a create, got {issued:?}");
    };
    assert_eq!(repo, "/src/thurbox");
    assert!(extras.is_empty(), "the cursor row is not added: {extras:?}");
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

#[test]
fn a_branch_name_that_prefilled_to_nothing_offers_no_pill() {
    // The branch field has no suggestion behind it, and a session name of pure
    // punctuation leaves `branch_from_name` nothing to prefill it with.
    let h = host();
    let mut world = World::default();
    world.repos.set_branches_for_test(
        "",
        "/src/thurbox",
        Branches::Ready(vec!["origin/main".into()]),
    );
    open(&h, &world);
    press(&h, &world, "space");
    press(&h, &world, "alt+w");
    press(&h, &world, "enter");
    world.wants.branches = Some((String::new(), "/src/thurbox".into()));
    press(&h, &world, "enter");
    type_text(&h, &world, "!!!");
    press(&h, &world, "enter");

    let screen = drawn(&h, &world);
    assert!(screen.contains("Branch Name"), "{screen}");
    assert!(
        !screen.contains("[ Create ]") && !screen.contains("[ Next ]"),
        "nothing prefilled and no default: {screen}"
    );

    type_text(&h, &world, "b");
    let typed = drawn(&h, &world);
    assert!(typed.contains("[ Next ]"), "{typed}");
}

// ── What is missing, said before the user commits ──────────────────────────

/// Draw the sessions pane and return its screen.
///
/// The flow's own `drawn` renders the float; the first-run notice lives on the
/// list behind it, which is the one screen a machine with no sessions reaches.
fn sessions_screen(host: &LuaHost, world: &World, width: u16, height: u16) -> String {
    publish(host, world);
    let index = index_of(host, "sessions");
    let rendered = host
        .render(
            index,
            RenderContext {
                width,
                height,
                focused: false,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render");
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| {
            thurbox::kernel::paint::render_recording(
                frame,
                Rect::new(0, 0, width, height),
                &rendered.node,
                &thurbox::kernel::terminal::Terminals::new(),
                &mut Vec::new(),
            );
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A snapshot whose multiplexer is not installed.
fn without_a_multiplexer(world: &mut World) {
    world.snapshot.mux = thurbox::kernel::snapshot::MuxRow {
        binary: "tmux".into(),
        presence: Presence::Missing,
        advice: "install tmux 3.2 or newer".into(),
    };
}

#[test]
fn a_missing_agent_is_marked_on_the_row_that_would_launch_it() {
    // The whole point of publishing presence: the cost of the choice is on the
    // choice, while the cursor is still moving over the alternatives — not in a
    // pane that exits after the user has committed.
    let host = host();
    let mut world = World::default();
    world.snapshot.agents[1].presence = Presence::Missing;
    open(&host, &world);
    press(&host, &world, "space");
    press(&host, &world, "enter");
    type_text(&host, &world, "work");
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Coding Agent"), "{screen}");
    assert!(
        screen.contains("not installed"),
        "the agent that would fail is not marked: {screen}"
    );
}

#[test]
fn a_missing_agent_warns_only_once_it_is_the_question() {
    // `codex` is missing, but the repository step is not where that is decided,
    // and warning about an agent the user has not been offered yet reads as a
    // refusal of the step they are on.
    let host = host();
    let mut world = World::default();
    world.snapshot.agents[1].presence = Presence::Missing;
    open(&host, &world);
    let screen = drawn(&host, &world);
    assert!(
        !screen.contains("pane will exit at once"),
        "the repository step warned about an agent nobody has chosen: {screen}"
    );
}

#[test]
fn a_missing_multiplexer_is_stated_from_the_first_step_of_the_flow() {
    // Nothing can be created without it, so it outranks every other warning and
    // does not wait for a step the user may never reach.
    let host = host();
    let mut world = World::default();
    without_a_multiplexer(&mut world);
    open(&host, &world);
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("tmux is not installed"),
        "the flow never says the multiplexer is missing: {screen}"
    );
}

#[test]
fn a_remote_host_is_never_reported_as_missing_the_local_multiplexer() {
    // The local machine's tmux has nothing to do with a session that will run on
    // a host — and `unknown` is not `missing`.
    let host = host();
    let mut world = World::default();
    without_a_multiplexer(&mut world);
    world.snapshot.hosts = vec![HostRow {
        name: "devbox".into(),
        detail: "me@devbox".into(),
        backend: "ssh:devbox".into(),
    }];
    open(&host, &world);
    press(&host, &world, "j");
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(
        !screen.contains("is not installed"),
        "a remote flow reported the local machine's multiplexer: {screen}"
    );
}

#[test]
fn a_remote_agent_is_never_reported_as_missing_by_local_presence() {
    // Presence is published from THIS machine's PATH — a remote host's binaries
    // live on the host and were never looked at, and the agent row must say
    // nothing rather than borrow the wrong machine's answer.
    let host = host();
    let mut world = World::default();
    world.snapshot.hosts = vec![HostRow {
        name: "devbox".into(),
        detail: "me@devbox".into(),
        backend: "ssh:devbox".into(),
    }];
    world.snapshot.agents[1].presence = Presence::Missing;
    world
        .repos
        .set_bookmarks_for_test("ssh:devbox", vec![bookmark("/srv/thurbox", Some(true))]);
    open(&host, &world);
    press(&host, &world, "j"); // local → devbox
    press(&host, &world, "enter");
    world.wants.bookmarks = Some("ssh:devbox".into());
    press(&host, &world, "space");
    press(&host, &world, "enter");
    type_text(&host, &world, "remote");
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Coding Agent"), "{screen}");
    assert!(
        !screen.contains("not installed"),
        "a remote host's agent was judged by the local machine's PATH: {screen}"
    );
}

#[test]
fn the_empty_session_list_says_the_multiplexer_is_missing() {
    // The one screen a first run always reaches. Absent on a machine that has
    // it, which is the normal case.
    let host = host();
    let mut world = World::default();
    without_a_multiplexer(&mut world);
    // The width a real sidebar has: the note must still be readable there, not
    // truncated to its first clause.
    let screen = sessions_screen(&host, &world, 28, 14);
    assert!(screen.contains("No sessions yet"), "{screen}");
    assert!(
        screen.contains("tmux is not installed"),
        "the first-run screen says nothing about the missing multiplexer: {screen}"
    );
    assert!(
        screen.contains("thurbox-cli doctor"),
        "the note names nowhere to get the whole answer: {screen}"
    );

    let quiet = World::default();
    let screen = sessions_screen(&host, &quiet, 28, 14);
    assert!(
        !screen.contains("is not installed"),
        "a machine that has everything was still warned: {screen}"
    );
}

#[test]
fn a_fork_never_warns_about_the_wrong_agent() {
    // A fork's agent is the source session's, resolved server-side by the
    // kernel — flow.agent_index is never assigned for one, so indexing
    // agents() by it would read whichever agent happens to sort first in
    // agents.toml, not the one the fork will actually run.
    let host = host();
    let mut world = World::default();
    world.snapshot.agents[0].presence = Presence::Missing;
    world.snapshot.sessions = vec![source_session()];
    publish(&host, &world);
    // Rendering is what settles the cursor onto the session row.
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
    .expect("render the sessions list");
    host.on_action(index_of(&host, "sessions"), "sessions.fork")
        .expect("sessions.fork");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Session Name"), "{screen}");
    assert!(
        !screen.contains("pane will exit at once"),
        "the fork flow warned about an agent it was never asked to pick: {screen}"
    );
}
