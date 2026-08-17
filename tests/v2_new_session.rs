//! The new-session flow, against the real bundled plugin.
//!
//! What is worth testing here is not the pixels — `tests/v2_parity.rs` owns that
//! — but the two things v1 got wrong often enough to have grown machinery for:
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

use thurbox::kernel::command::{BookmarkEdit, Command};
use thurbox::kernel::host::{KeyPress, LuaHost, Published, RenderContext};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::repos::{BookmarkRow, Branches, BrowseEntry, Listing, RepoStore, Wants};
use thurbox::kernel::snapshot::{AgentRow, HostRow, Snapshot};
use thurbox::kernel::theme::Themes;

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
            },
            AgentRow {
                name: "codex".into(),
                command: "codex".into(),
            },
        ],
        agent_default: "claude".into(),
        ..Snapshot::default()
    }
}

fn bookmark(path: &str, is_git: Option<bool>) -> BookmarkRow {
    BookmarkRow {
        name: path.rsplit('/').next().unwrap_or(path).to_string(),
        path: path.to_string(),
        parent: None,
        is_parent: false,
        is_git,
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
        },
        BookmarkRow {
            path: "/src/thurbox".into(),
            name: "thurbox".into(),
            parent: Some("/src".into()),
            is_parent: false,
            is_git: Some(true),
        },
    ]
}

/// Everything the loop publishes, with the wants the flow is currently asking
/// for — which is what makes the three reads visible to it.
struct World {
    snapshot: Snapshot,
    repos: RepoStore,
    wants: Wants,
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
        snapshot: &world.snapshot,
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
        repos: &world.repos,
        wants: &world.wants,
        focus: None,
        hovered: None,
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
    // The flow asks in cells for its height and a share of the screen for its
    // width, exactly as v1's modals are sized.
    let rows = float.rows.expect("the flow sizes its own height");
    let width = (120.0 * float.width_pct / 100.0) as u16;
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
    while let Some(rest) = name.strip_prefix("ctrl+") {
        key.ctrl = true;
        name = rest;
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
fn space_selects_and_w_gives_it_a_worktree() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "space");
    assert!(drawn(&host, &world).contains("[x] /src/thurbox"));
    press(&host, &world, "w");
    let screen = drawn(&host, &world);
    assert!(screen.contains("[wt]"), "{screen}");
}

#[test]
fn worktree_mode_is_refused_for_a_directory_that_is_not_a_repository() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "j");
    press(&host, &world, "w");
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
    press(&host, &world, "/");
    type_text(&host, &world, "note");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Search (1/2)"), "{screen}");
    assert!(screen.contains("/src/notes"), "{screen}");
    assert!(!screen.contains("thurbox"), "{screen}");
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
fn ctrl_p_imports_the_typed_path_as_a_folder() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "/src");
    press(&host, &world, "ctrl+p");
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
fn d_forgets_a_remembered_repository() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "d");
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
fn a_member_of_a_folder_cannot_be_forgotten_on_its_own() {
    let host = host();
    let world = world_with(folder_rows());
    open(&host, &world);
    press(&host, &world, "j");
    press(&host, &world, "d");
    assert!(
        host.drain_commands().is_empty(),
        "a child has no memory of its own to forget"
    );
    assert!(drawn(&host, &world).contains("delete the parent header instead"));
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
            agent: Some("claude".into()),
            host: None,
            extras: Vec::new(),
        }]
    );
    assert_eq!(drawn(&host, &world), "", "the flow closes when it commits");
}

#[test]
fn an_empty_name_is_refused_rather_than_creating_something_unnamed() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "space");
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
    press(&host, &world, "w");
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
    press(&host, &world, "w");
    press(&host, &world, "j");
    press(&host, &world, "w");
    press(&host, &world, "j");
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
    assert_eq!(target.as_deref(), Some("ssh:devbox"));
}

#[test]
fn nothing_selected_locally_still_creates_a_session() {
    // v1 spawns in the home directory when no repository is chosen.
    let host = host();
    let world = World::default();
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
    for chord in ["ctrl+n", "j", "k", "space", "w", "d", "/", "ctrl+p"] {
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
    press(&host, &world, "/");

    press(&host, &world, "down");
    press(&host, &world, "enter");
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
