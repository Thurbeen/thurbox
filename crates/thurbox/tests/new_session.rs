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
use thurbox::kernel::snapshot::{AgentRow, FleetRow, FleetState, HostRow, Snapshot};
use thurbox::kernel::theme::Themes;

const MOD: &str = "new_session";

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

fn offered() -> BookmarkRow {
    BookmarkRow {
        label: Some("Thurbox interface — edit your panes".into()),
        offered: true,
        ..bookmark(INTERFACE_DIR, Some(false))
    }
}

fn store_with(rows: Vec<BookmarkRow>) -> RepoStore {
    let mut repos = RepoStore::new();
    repos.set_bookmarks_for_test("", rows);
    repos
}

fn world_with(rows: Vec<BookmarkRow>) -> World {
    World {
        repos: store_with(rows),
        ..World::default()
    }
}

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
        starts: &Default::default(),
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

fn drawn(host: &LuaHost, world: &World) -> String {
    match painted(host, world, None) {
        Some((buffer, _)) => screen_of(&buffer),
        None => String::new(),
    }
}

fn painted(
    host: &LuaHost,
    world: &World,
    hovered: Option<&Identity>,
) -> Option<(Buffer, Vec<Hit>)> {
    painted_on(host, world, hovered, 120)
}

fn painted_on(
    host: &LuaHost,
    world: &World,
    hovered: Option<&Identity>,
    screen_cols: u16,
) -> Option<(Buffer, Vec<Hit>)> {
    painted_sized(host, world, hovered, screen_cols, 40)
}

fn painted_sized(
    host: &LuaHost,
    world: &World,
    hovered: Option<&Identity>,
    screen_cols: u16,
    screen_rows: u16,
) -> Option<(Buffer, Vec<Hit>)> {
    publish_hovered(host, world, hovered);
    let index = index_of(host, MOD);
    let rendered = host
        .render(
            index,
            RenderContext {
                width: screen_cols,
                height: screen_rows,
                focused: true,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render");
    let float = rendered.float?;
    let rows = float.rows.expect("the flow sizes its own height");
    let width = float
        .cols
        .unwrap_or((f64::from(screen_cols) * float.width_pct / 100.0) as u16);
    let mut hits = Vec::new();
    let mut terminal = Terminal::new(TestBackend::new(width, rows)).expect("terminal");
    terminal
        .draw(|frame| {
            thurbox::kernel::paint::render_recording(
                frame,
                Rect::new(0, 0, width, rows),
                &rendered.node,
                &thurbox::kernel::terminal::Terminals::with_registry(
                    std::sync::Arc::new(thurbox::backend::wiring::local_only(&Default::default())),
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

fn identity_at(hits: &[Hit], x: u16, y: u16) -> Identity {
    let position = ratatui::layout::Position::new(x, y);
    hits.iter()
        .rev()
        .find(|hit| hit.rect.contains(position))
        .map(|hit| hit.identity.clone())
        .unwrap_or_default()
}

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
    host.on_click(index_of(host, MOD), &click)
        .expect("on_click");
    identity
}

fn press(host: &LuaHost, world: &World, chord: &str) {
    publish(host, world);
    let index = index_of(host, MOD);
    let key = key_press(chord);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    if let Some(binding) = registry.resolve(&key, Some(MOD)) {
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
        let index = index_of(host, MOD);
        let mut registry = Registry::default();
        let (bindings, settings) = host.declarations();
        registry.declare(bindings, settings);
        let consumed = match registry.resolve(&key, Some(MOD)) {
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
    if drawn(host, world).contains("Place") {
        press(host, world, "enter");
    }
}

fn devbox() -> HostRow {
    HostRow {
        name: "devbox".into(),
        detail: "me@devbox".into(),
        backend: "ssh:devbox".into(),
        ..HostRow::default()
    }
}

fn fleet(
    name: &str,
    transport: &str,
    state: FleetState,
    places: Vec<HostRow>,
    agents: &[&str],
) -> FleetRow {
    FleetRow {
        name: name.into(),
        transport: transport.into(),
        remote: transport != "local Fleet",
        state,
        why: None,
        home: None,
        agents: Some(
            agents
                .iter()
                .map(|name| AgentRow {
                    name: (*name).into(),
                })
                .collect(),
        ),
        places: Some(places),
    }
}

fn three_fleets() -> Vec<FleetRow> {
    let box_local = HostRow {
        detail: "box itself (mux)".into(),
        ready: Some(true),
        ..HostRow::local("box itself")
    };
    let wsl = HostRow {
        name: "mux/wsl-dev".into(),
        detail: "a mux place".into(),
        backend: "mux/wsl-dev".into(),
        ready: Some(true),
        why: None,
        here: false,
    };
    let mut gpu = fleet("gpu", "ssh gpu", FleetState::Unreachable, Vec::new(), &[]);
    gpu.why = Some("ssh: connect to host gpu port 22: Connection refused".into());
    gpu.places = None;
    vec![
        fleet(
            "box",
            "ssh box",
            FleetState::Down,
            vec![box_local, wsl],
            &["codex"],
        ),
        gpu,
        fleet(
            "laptop",
            "local Fleet",
            FleetState::Up,
            vec![local_host()],
            &["claude", "codex"],
        ),
    ]
}

#[test]
fn a_single_listed_fleet_skips_the_fleet_step() {
    let host = host();
    let mut world = World::default();
    world.snapshot.fleets = vec![fleet(
        "laptop",
        "local Fleet",
        FleetState::Up,
        vec![local_host()],
        &["claude"],
    )];
    press(&host, &world, "ctrl+n");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Place"), "{screen}");
    assert!(!screen.contains("local Fleet"), "no Fleet step: {screen}");
    press(&host, &world, "enter");
    press(&host, &world, "down");
    press(&host, &world, "enter");
    type_text(&host, &world, "notes");
    press(&host, &world, "enter");
    assert!(
        matches!(
            &host.drain_commands()[..],
            [Command::Create { fleet: Some(f), agent: Some(a), .. }] if f == "laptop" && a == "claude"
        ),
        "one agent spawns at once, on the one Fleet"
    );
}

#[test]
fn a_sole_incompatible_fleet_is_refused_on_the_place_step() {
    let host = host();
    let mut world = World::default();
    let mut only = fleet(
        "box",
        "ssh box",
        FleetState::Incompatible,
        vec![local_host()],
        &[],
    );
    only.why = Some("incompatible: fleet 9.0.0 speaks fleet/v2".into());
    only.agents = None;
    world.snapshot.fleets = vec![only];
    press(&host, &world, "ctrl+n");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("box is incompatible: fleet 9.0.0 speaks fleet/v2"),
        "{screen}"
    );
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("Place"),
        "still on the place step: {screen}"
    );
    assert!(!screen.contains("Asking Fleet"), "{screen}");
    assert!(host.drain_commands().is_empty());
}

#[test]
fn a_refusal_keeps_the_cursor_when_a_later_place_is_ready() {
    let host = host();
    let mut world = World::default();
    let mut places = places_of_two_lengths();
    places.reverse();
    let mut only = fleet("box", "ssh box", FleetState::Incompatible, places, &[]);
    only.why = Some("incompatible: fleet 9.0.0 speaks fleet/v2".into());
    only.agents = None;
    world.snapshot.fleets = vec![only];
    press(&host, &world, "ctrl+n");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("▸ box is incompatible: fleet 9.0.0 speaks fleet/v2"),
        "{screen}"
    );
    let buffer = colours(&host, &world, None);
    let muted = fg_at(&buffer, "navigate");
    assert_ne!(fg_at(&buffer, "▸ box is incompatible"), muted, "{screen}");
    assert_ne!(fg_at(&buffer, "Select"), muted, "{screen}");
}

#[test]
fn a_fleet_refused_later_shows_the_refusal_on_the_agent_step() {
    let host = host();
    let mut world = World::default();
    let mut only = fleet(
        "laptop",
        "local Fleet",
        FleetState::Up,
        vec![local_host()],
        &[],
    );
    only.agents = None;
    world.snapshot.fleets = vec![only];
    press(&host, &world, "ctrl+n");
    press(&host, &world, "enter");
    press(&host, &world, "down");
    press(&host, &world, "enter");
    type_text(&host, &world, "notes");
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Asking Fleet for its agents"), "{screen}");

    world.snapshot.fleets[0].state = FleetState::Incompatible;
    world.snapshot.fleets[0].why = Some("incompatible: fleet 9.0.0 speaks fleet/v2".into());
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("laptop is incompatible: fleet 9.0.0 speaks fleet/v2"),
        "{screen}"
    );
    assert!(!screen.contains("Asking Fleet"), "{screen}");
    assert!(!screen.contains("Create"), "{screen}");
    assert!(screen.contains("esc"), "{screen}");
    press(&host, &world, "enter");
    assert!(host.drain_commands().is_empty());
}

#[test]
fn the_fleet_step_lists_each_fleet_and_the_rest_follows_the_one_picked() {
    let host = host();
    let mut world = World::default();
    world.snapshot.fleets = three_fleets();
    press(&host, &world, "ctrl+n");
    let screen = drawn(&host, &world);
    for shown in [
        "Fleet",
        "ssh box",
        "Fleet not running",
        "install the systemd unit (see doctor)",
        "ssh gpu",
        "○ unreachable",
        "local Fleet",
        "● up",
    ] {
        assert!(screen.contains(shown), "{shown}: {screen}");
    }

    press(&host, &world, "down");
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(screen.contains("gpu cannot be reached"), "{screen}");
    assert!(
        screen.contains("ssh box"),
        "still on the Fleet step: {screen}"
    );

    assert_eq!(host.shared_string("want_places"), None);
    press(&host, &world, "up");
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Place on box"), "{screen}");
    assert_eq!(
        host.shared_string("want_places").as_deref(),
        Some("box"),
        "the place picker asks box for its places again"
    );
    assert!(screen.contains("box itself (mux)"), "{screen}");
    assert!(screen.contains("mux/wsl-dev"), "{screen}");
    assert!(screen.contains("● ready"), "{screen}");
    assert!(
        !screen.contains("this machine"),
        "laptop's places: {screen}"
    );

    world
        .repos
        .set_bookmarks_for_test("box/local", vec![bookmark("/srv/app", None)]);
    world.wants.bookmarks = Some("box/local".into());
    press(&host, &world, "enter");
    drawn(&host, &world);
    assert_eq!(
        host.shared_string("want_places"),
        None,
        "only while it is open"
    );
    let screen = drawn(&host, &world);
    assert!(screen.contains("Repos on box/local"), "{screen}");
    assert!(screen.contains("/srv/app"), "{screen}");
    assert!(!screen.contains("/src/thurbox"), "this machine's: {screen}");

    press(&host, &world, "tab");
    press(&host, &world, "tab");
    drawn(&host, &world);
    assert!(
        host.shared_string("want_browse")
            .is_some_and(|want| want.starts_with("box/local\0")),
        "{:?}",
        host.shared_string("want_browse")
    );
    press(&host, &world, "backtab");
    press(&host, &world, "enter");
    type_text(&host, &world, "app");
    press(&host, &world, "enter");
    assert_eq!(
        host.drain_commands(),
        vec![Command::Create {
            name: "app".into(),
            repo: "/srv/app".into(),
            agent: Some("codex".into()),
            host: None,
            fleet: Some("box".into()),
            worktree: Some(false),
            branch: None,
            base: None,
            extras: Vec::new(),
        }],
        "box's one agent, on box's local"
    );
}

#[test]
fn a_remote_fleets_path_starts_at_its_own_home() {
    let host = host();
    let mut world = World::default();
    world.snapshot.fleets = three_fleets();
    world.wants.bookmarks = Some("box/local".into());
    press(&host, &world, "ctrl+n");
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    press(&host, &world, "tab");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("type an absolute path on box, or a clone URL"),
        "{screen}"
    );
    assert!(!screen.contains("~/"), "{screen}");

    press(&host, &world, "tab");
    drawn(&host, &world);
    assert_eq!(
        host.shared_string("want_browse").as_deref(),
        Some("box/local\0~")
    );
    world.wants.browse = Some(("box/local".into(), "~".into()));
    world.repos.set_listing_for_test(
        "box/local",
        "~",
        Listing::Ready(vec![BrowseEntry {
            name: "app".into(),
            is_git: true,
        }]),
    );
    world
        .repos
        .set_resolved_dir_for_test("box/local", "~", "/home/box");
    drawn(&host, &world);
    press(&host, &world, "enter");
    assert!(
        host.drain_commands().iter().any(|c| matches!(
            c,
            Command::Bookmark { host, path, .. } if host == "box/local" && path == "/home/box/app"
        )),
        "a picked entry is absolute"
    );
    world.wants.browse = None;

    let host = self::host();
    world.snapshot.fleets[0].home = Some("/home/me".into());
    press(&host, &world, "ctrl+n");
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    press(&host, &world, "tab");
    assert!(drawn(&host, &world).contains("/home/me/"));
    for (typed, back) in [("~", 1), ("~/", 2), ("~/pro", 5)] {
        type_text(&host, &world, typed);
        drawn(&host, &world);
        assert_eq!(
            host.shared_string("want_browse").as_deref(),
            Some("box/local\0/home/me"),
            "{typed}"
        );
        for _ in 0..back {
            press(&host, &world, "backspace");
        }
    }
    type_text(&host, &world, "~/app");
    press(&host, &world, "enter");
    assert!(
        host.drain_commands().iter().any(|c| matches!(
            c,
            Command::Bookmark { host, path, .. } if host == "box/local" && path == "/home/me/app"
        )),
        "a typed ~ is box's home"
    );
}

#[test]
fn a_remote_fleets_home_prefills_only_its_own_local() {
    let mut world = World::default();
    world.snapshot.fleets = three_fleets();
    world.snapshot.fleets[0].home = Some("/home/box".into());

    let host = host();
    press(&host, &world, "ctrl+n");
    press(&host, &world, "enter");
    press(&host, &world, "down");
    press(&host, &world, "enter");
    press(&host, &world, "tab");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Repos on box/mux/wsl-dev"), "{screen}");
    assert!(
        !screen.contains("/home/box/"),
        "box's home is not wsl-dev's: {screen}"
    );
    assert!(
        screen.contains("type an absolute path on box/mux/wsl-dev, or a clone URL"),
        "{screen}"
    );

    let host = self::host();
    press(&host, &world, "ctrl+n");
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    press(&host, &world, "tab");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Repos on box/local"), "{screen}");
    assert!(screen.contains("/home/box/"), "{screen}");
}

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
fn the_host_step_offers_this_machine_at_once_and_fleets_hosts_when_they_arrive() {
    let host = host();
    let mut world = World::default();
    press(&host, &world, "ctrl+n");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Place"), "{screen}");
    assert!(screen.contains("this machine"), "{screen}");
    world.snapshot.hosts = Some(vec![local_host(), devbox()]);
    let screen = drawn(&host, &world);
    assert!(screen.contains("devbox"), "{screen}");
    press(&host, &world, "enter");
    assert!(drawn(&host, &world).contains("Select Repo"));
}

#[test]
fn a_host_list_not_yet_published_shows_the_wait() {
    let host = host();
    let mut world = World::default();
    world.snapshot.hosts = None;
    press(&host, &world, "ctrl+n");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Asking Fleet for its places"), "{screen}");
    world.snapshot.hosts = Some(vec![local_host(), devbox()]);
    let screen = drawn(&host, &world);
    assert!(screen.contains("devbox"), "{screen}");
}

fn places_of_two_lengths() -> Vec<HostRow> {
    vec![
        HostRow {
            detail: "this machine (mux)".into(),
            ready: Some(true),
            ..HostRow::local("this machine")
        },
        HostRow {
            name: "mux/gpu-box".into(),
            detail: "a mux place".into(),
            backend: "mux/gpu-box".into(),
            ready: Some(false),
            why: Some(NOT_READY_WHY.into()),
            here: false,
        },
    ]
}

const NOT_READY_WHY: &str =
    "place \"gpu-box\" cannot be reached: ssh: connect to host gpu-box port 22: Connection timed out";

fn column_of(buffer: &Buffer, needle: &str) -> u16 {
    locate(buffer, needle).0
}

#[test]
fn the_place_picker_aligns_its_columns_and_gives_the_reason_one_line() {
    let frames: [(u16, &[&str]); 2] = [
        (
            80,
            &[
                "╭ Place ───────────────────────────────────────────────────╮",
                "│  local        this machine (mux)  ● ready                │",
                "│▸ mux/gpu-box  a mux place         ○ not ready            │",
                "│ place \"gpu-box\" cannot be reached: ssh: connect to host …│",
                "│                                                          │",
                "│j/k navigate                         [ Select ] [ Cancel ]│",
                "╰──────────────────────────────────────────────────────────╯",
            ],
        ),
        (
            48,
            &[
                "╭ Place ───────────────────────────────────────╮",
                "│  local        this machine (mux)  ● ready    │",
                "│▸ mux/gpu-box  a mux place         ○ not ready│",
                "│ place \"gpu-box\" cannot be reached: ssh: conn…│",
                "│                                              │",
                "│j/k navigate             [ Select ] [ Cancel ]│",
                "╰──────────────────────────────────────────────╯",
            ],
        ),
    ];
    for (screen_cols, frame) in frames {
        let host = host();
        let mut world = World::default();
        world.snapshot.hosts = Some(places_of_two_lengths());
        press(&host, &world, "ctrl+n");
        press(&host, &world, "down");
        let (buffer, _) = painted_on(&host, &world, None, screen_cols).expect("open");
        let screen = screen_of(&buffer);
        assert!(
            buffer.area.width <= screen_cols,
            "{screen_cols} cols: the modal is {} wide:\n{screen}",
            buffer.area.width
        );
        let lines: Vec<&str> = screen.lines().collect();
        let local = lines
            .iter()
            .position(|l| l.contains("local"))
            .expect(&screen);
        let place = lines
            .iter()
            .position(|l| l.contains("mux/gpu-box"))
            .expect(&screen);
        let detail = |row: usize, text: &str| {
            let at = lines[row].find(text).unwrap_or_else(|| {
                panic!("{screen_cols} cols: {text:?} not on row {row}:\n{screen}")
            });
            lines[row][..at].chars().count()
        };
        assert_eq!(
            detail(local, "this"),
            detail(place, "a mux"),
            "{screen_cols} cols: the detail column wanders:\n{screen}"
        );
        assert!(
            detail(place, "a mux") > detail(place, "mux/gpu-box") + "mux/gpu-box".len(),
            "{screen_cols} cols: a name runs into its detail:\n{screen}"
        );
        assert_eq!(
            column_of(&buffer, "● ready"),
            column_of(&buffer, "○ not ready"),
            "{screen_cols} cols: the status column wanders:\n{screen}"
        );
        assert!(
            !lines[place].contains("not ready:"),
            "{screen_cols} cols: the row carries the reason, cut:\n{screen}"
        );
        let reason = lines[place + 1].trim_matches(|c: char| c == '│' || c.is_whitespace());
        assert!(
            reason.starts_with("place \"gpu-box\" cannot be reached") && reason.ends_with('…'),
            "{screen_cols} cols: the reason is not one ellipsized line:\n{screen}"
        );
        assert_eq!(screen, frame.join("\n"), "{screen_cols} cols:\n{screen}");
    }
}

#[test]
fn a_ready_place_under_the_cursor_leaves_one_blank_reason_line() {
    let host = host();
    let mut world = World::default();
    world.snapshot.hosts = Some(places_of_two_lengths());
    press(&host, &world, "ctrl+n");
    let screen = drawn(&host, &world);
    let lines: Vec<&str> = screen.lines().collect();
    let place = lines
        .iter()
        .position(|l| l.contains("mux/gpu-box"))
        .expect(&screen);
    let blank = |l: &str| {
        l.trim_matches(|c: char| c == '│' || c.is_whitespace())
            .is_empty()
    };
    assert!(
        blank(lines[place + 1]) && blank(lines[place + 2]),
        "{screen}"
    );
    assert!(lines[place + 3].contains("[ Select ]"), "{screen}");
}

fn fg_at(buffer: &Buffer, needle: &str) -> ratatui::style::Color {
    let (x, y) = locate(buffer, needle);
    buffer[(x, y)].fg
}

#[test]
fn a_not_ready_place_is_dimmed() {
    let host = host();
    let mut world = World::default();
    let mut places = places_of_two_lengths();
    places.insert(
        1,
        HostRow {
            name: "mux/other".into(),
            detail: "another place".into(),
            backend: "mux/other".into(),
            ready: Some(true),
            why: None,
            here: false,
        },
    );
    world.snapshot.hosts = Some(places);
    press(&host, &world, "ctrl+n");
    let buffer = colours(&host, &world, None);
    let muted = fg_at(&buffer, "navigate");
    assert_eq!(
        fg_at(&buffer, "mux/gpu-box"),
        muted,
        "{}",
        screen_of(&buffer)
    );
    assert_eq!(
        fg_at(&buffer, "a mux place"),
        muted,
        "{}",
        screen_of(&buffer)
    );
    assert_ne!(fg_at(&buffer, "mux/other"), muted, "{}", screen_of(&buffer));
}

#[test]
fn a_not_ready_place_cannot_be_selected() {
    let host = host();
    let mut world = World::default();
    world.snapshot.hosts = Some(places_of_two_lengths());
    press(&host, &world, "ctrl+n");
    let ready = colours(&host, &world, None);
    assert_ne!(
        fg_at(&ready, "Select"),
        fg_at(&ready, "navigate"),
        "Select is enabled on a ready place:\n{}",
        screen_of(&ready)
    );
    press(&host, &world, "down");
    let not_ready = colours(&host, &world, None);
    assert_eq!(
        fg_at(&not_ready, "Select"),
        fg_at(&not_ready, "navigate"),
        "Select is disabled on a place that is not ready:\n{}",
        screen_of(&not_ready)
    );
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("╭ Place"),
        "the flow left the place step:\n{screen}"
    );
    assert!(screen.contains("mux/gpu-box is not ready"), "{screen}");
}

#[test]
fn a_disabled_select_does_not_light_under_the_pointer() {
    let host = host();
    let mut world = World::default();
    world.snapshot.hosts = Some(places_of_two_lengths());
    press(&host, &world, "ctrl+n");
    press(&host, &world, "down");
    let pill = identity_of(&host, &world, "Select", 0);
    let resting = colours(&host, &world, None);
    let lit = colours(&host, &world, Some(&pill));
    let (x, y) = locate(&resting, "Select");
    assert_eq!(resting[(x, y)], lit[(x, y)], "{}", screen_of(&lit));
}

#[test]
fn the_place_picker_opens_on_the_first_ready_place() {
    let host = host();
    let mut world = World::default();
    let mut places = places_of_two_lengths();
    places.reverse();
    world.snapshot.hosts = Some(places);
    press(&host, &world, "ctrl+n");
    let screen = drawn(&host, &world);
    assert!(
        screen.lines().any(|l| l.contains("▸ local")),
        "the cursor is not on the ready place:\n{screen}"
    );
    press(&host, &world, "enter");
    assert!(
        !drawn(&host, &world).contains("╭ Place"),
        "the ready place is picked"
    );
}

#[test]
fn a_long_name_never_hides_a_pickers_status_column() {
    let long = "mux/a-place-whose-name-runs-on-and-on-and-on-x";
    assert_eq!(long.len(), 46);
    let host = host();
    let mut world = World::default();
    let mut places = places_of_two_lengths();
    places[1].name = long.into();
    places[1].backend = long.into();
    world.snapshot.hosts = Some(places);
    press(&host, &world, "ctrl+n");
    let (buffer, _) = painted_on(&host, &world, None, 60).expect("open");
    let screen = screen_of(&buffer);
    let lines: Vec<&str> = screen.lines().collect();
    assert!(
        lines[1].contains("local") && lines[1].contains("● ready"),
        "{screen}"
    );
    assert!(
        lines[2].contains("mux/a-place") && lines[2].contains("○ not ready"),
        "{screen}"
    );
    assert!(
        lines[2].contains('…'),
        "the long name is ellipsized:\n{screen}"
    );
    assert_eq!(
        column_of(&buffer, "● ready"),
        column_of(&buffer, "○ not ready"),
        "{screen}"
    );

    let host = self::host();
    let mut world = World::default();
    let mut fleets = three_fleets();
    fleets[2].name = "a-fleet-whose-name-runs-on-and-on-and-on-and-on".into();
    world.snapshot.fleets = fleets;
    press(&host, &world, "ctrl+n");
    let (buffer, _) = painted_on(&host, &world, None, 60).expect("open");
    let screen = screen_of(&buffer);
    assert!(screen.contains("Fleet"), "{screen}");
    for state in ["○ Fleet not running", "○ unreachable", "● up"] {
        assert!(screen.contains(state), "{state:?} is lost:\n{screen}");
    }
}

#[test]
fn a_long_place_list_scrolls_inside_a_short_screen() {
    let host = host();
    let mut world = World::default();
    let mut places = vec![places_of_two_lengths()[0].clone()];
    for n in 1..20 {
        places.push(HostRow {
            name: format!("mux/place-{n:02}"),
            detail: "a mux place".into(),
            backend: format!("mux/place-{n:02}"),
            ready: Some(false),
            why: Some(NOT_READY_WHY.into()),
            here: false,
        });
    }
    world.snapshot.hosts = Some(places);
    press(&host, &world, "ctrl+n");
    for _ in 0..19 {
        press(&host, &world, "down");
    }
    let (buffer, _) = painted_sized(&host, &world, None, 80, 24).expect("open");
    let screen = screen_of(&buffer);
    assert!(
        buffer.area.height <= 24,
        "{} rows:\n{screen}",
        buffer.area.height
    );
    assert!(
        screen.lines().any(|l| l.contains("▸ mux/place-19")),
        "the cursor's row is off screen:\n{screen}"
    );
    assert!(
        screen.contains("[ Select ]"),
        "the footer is lost:\n{screen}"
    );
    assert!(
        screen.contains("cannot be reached"),
        "no reason at all:\n{screen}"
    );
    let lines: Vec<&str> = screen.lines().collect();
    let listed = lines
        .iter()
        .filter(|l| l.contains("mux/place-") || l.contains("this machine"))
        .count();
    assert_eq!(listed, 19, "the list gave up rows to the reason:\n{screen}");
    let cursor = lines
        .iter()
        .position(|l| l.contains("▸ mux/place-19"))
        .expect(&screen);
    assert!(lines[cursor + 1].contains("cannot be reached"), "{screen}");
    assert_eq!(
        lines[cursor + 2].trim_matches(|c: char| c == '│' || c.is_whitespace()),
        "",
        "the reason takes one line:\n{screen}"
    );
}

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
    assert!(screen.contains("Place"), "{screen}");
    assert!(
        screen.contains("local"),
        "the local machine is a choice: {screen}"
    );
    assert!(
        screen.contains("devbox") && screen.contains("me@devbox"),
        "a host is shown with the detail that tells two apart: {screen}"
    );
}

fn with_herdr_local() -> World {
    let mut world = World::default();
    let herdr_local = HostRow {
        name: "herdr/local".into(),
        detail: "this machine (herdr)".into(),
        backend: "herdr/local".into(),
        here: true,
        ..HostRow::default()
    };
    world.snapshot.fleets = vec![fleet(
        "laptop",
        "local Fleet",
        FleetState::Up,
        vec![local_host(), herdr_local],
        &["claude"],
    )];
    world
}

#[test]
fn another_runtimes_local_place_is_this_machine() {
    let host = host();
    let world = with_herdr_local();
    press(&host, &world, "ctrl+n");
    press(&host, &world, "down");
    press(&host, &world, "enter");
    let screen = drawn(&host, &world);
    assert_eq!(host.shared_string("want_bookmarks").as_deref(), Some(""));
    assert!(screen.contains("/src/notes"), "{screen}");
    assert!(!screen.contains("Clone URL"), "{screen}");
    press(&host, &world, "down");
    press(&host, &world, "enter");
    type_text(&host, &world, "notes");
    press(&host, &world, "enter");
    let commands = host.drain_commands();
    assert!(
        matches!(
            &commands[..],
            [Command::Create { repo, host: Some(h), .. }]
                if repo == "/src/notes" && h == "herdr/local"
        ),
        "{commands:?}"
    );
}

#[test]
fn another_runtimes_local_place_is_browsed_here() {
    let host = host();
    let world = with_herdr_local();
    press(&host, &world, "ctrl+n");
    press(&host, &world, "down");
    press(&host, &world, "enter");
    press(&host, &world, "tab");
    press(&host, &world, "tab");
    drawn(&host, &world);
    let browse = host.shared_string("want_browse").expect("a listing");
    assert!(browse.starts_with('\0'), "{browse:?}");
}

#[test]
fn a_remote_fleets_runtime_local_place_is_read_through_itself() {
    let host = host();
    let mut world = World::default();
    let herdr_local = HostRow {
        name: "herdr/local".into(),
        detail: "box itself (herdr)".into(),
        backend: "herdr/local".into(),
        here: true,
        ..HostRow::default()
    };
    world.snapshot.fleets = vec![fleet(
        "box",
        "ssh box",
        FleetState::Up,
        vec![herdr_local],
        &["claude"],
    )];
    press(&host, &world, "ctrl+n");
    press(&host, &world, "enter");
    press(&host, &world, "tab");
    press(&host, &world, "tab");
    drawn(&host, &world);
    assert_eq!(
        host.shared_string("want_bookmarks").as_deref(),
        Some("box/local")
    );
    assert_eq!(
        host.shared_string("want_browse").as_deref(),
        Some("box/herdr/local\0~")
    );
}

#[test]
fn a_remote_fleets_runtime_local_place_skips_a_local_that_is_not_ready() {
    let host = host();
    let mut world = World::default();
    let down = HostRow {
        detail: "box itself (mux)".into(),
        ready: Some(false),
        why: Some("tmux is not running".into()),
        ..HostRow::local("box itself")
    };
    let herdr_local = HostRow {
        name: "herdr/local".into(),
        detail: "box itself (herdr)".into(),
        backend: "herdr/local".into(),
        ready: Some(true),
        here: true,
        ..HostRow::default()
    };
    world.snapshot.fleets = vec![fleet(
        "box",
        "ssh box",
        FleetState::Up,
        vec![down, herdr_local],
        &["claude"],
    )];
    press(&host, &world, "ctrl+n");
    press(&host, &world, "enter");
    press(&host, &world, "tab");
    press(&host, &world, "tab");
    drawn(&host, &world);
    assert_eq!(
        host.shared_string("want_bookmarks").as_deref(),
        Some("box/local")
    );
    assert_eq!(
        host.shared_string("want_browse").as_deref(),
        Some("box/herdr/local\0~")
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

#[test]
fn remembered_repositories_are_listed_with_their_kind() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    let screen = drawn(&host, &world);
    assert!(screen.contains("Repos (2)"), "{screen}");
    assert!(screen.contains("/src/thurbox"), "{screen}");
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

fn listed(screen: &str) -> Vec<String> {
    screen
        .lines()
        .filter_map(|line| line.split_once("[ ] ").or_else(|| line.split_once("[x] ")))
        .map(|(_, rest)| rest.trim_end_matches(['│', ' ', '┐', '┘']).to_string())
        .collect()
}

#[test]
fn a_match_in_the_repository_name_ranks_above_one_in_its_path() {
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
            "/home/me/work/api",
            "/home/me/work/api-gateway",
            "/home/me/work/legacy-api",
            "/home/me/capital/web",
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
    let host = host();
    let world = World::default();
    open(&host, &world);
    let screen = drawn(&host, &world);
    assert!(screen.contains("Search (2/2)"), "{screen}");
}

#[test]
fn letters_that_used_to_be_shortcuts_are_typed_into_the_search() {
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
    let host = host();
    let mut world = world_with(vec![offered()]);
    open(&host, &world);
    press(&host, &world, "tab");
    type_text(&host, &world, "/src/new");
    press(&host, &world, "enter");

    world.repos = store_with(vec![offered(), bookmark("/src/new", Some(true))]);
    let screen = drawn(&host, &world);
    let selected: Vec<&str> = screen.lines().filter(|line| line.contains("[x]")).collect();
    assert_eq!(selected.len(), 1, "one row is chosen:\n{screen}");
    assert!(
        selected[0].contains("new"),
        "the path just typed is the one chosen:\n{screen}"
    );
}

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
    assert!(drawn(&host, &world).contains("Select Repo"));
}

#[test]
fn adding_a_repository_clears_the_search_that_would_hide_it() {
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
    assert!(
        drawn(&host, &world).contains("[ Create folder ]"),
        "the folder is made with its parents, so a failed listing is no reason to refuse"
    );
}

#[test]
fn a_listing_still_on_its_way_leaves_enter_an_add() {
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

#[test]
fn a_plain_selection_names_the_session_then_the_agent() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "down");
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
    assert!(screen.contains("▸ claude"), "{screen}");

    press(&host, &world, "enter");
    assert_eq!(
        host.drain_commands(),
        vec![Command::Create {
            name: "read the code".into(),
            repo: "/src/notes".into(),
            agent: Some("claude".into()),
            host: None,
            fleet: None,
            worktree: Some(false),
            branch: None,
            base: None,
            extras: Vec::new(),
        }]
    );
    assert_eq!(drawn(&host, &world), "", "the flow closes when it commits");
}

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
            fleet: None,
            worktree: Some(true),
            branch: Some("fix-tooltips".into()),
            base: Some("main".into()),
            extras: Vec::new(),
        }]
    );
}

#[test]
fn ticked_repositories_all_go_into_the_session() {
    let host = host();
    let world = world_with_branches(main_and_dev());
    open(&host, &world);
    press(&host, &world, "alt+w");
    press(&host, &world, "down");
    press(&host, &world, "space");
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    type_text(&host, &world, "both");
    press(&host, &world, "enter");
    press(&host, &world, "enter");
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

#[test]
fn a_directory_without_git_is_refused_a_worktree() {
    let host = host();
    let world = World::default();
    open(&host, &world);
    press(&host, &world, "down");
    press(&host, &world, "alt+w");
    let screen = drawn(&host, &world);
    assert!(screen.contains("Not a git repo"), "{screen}");
    assert!(!screen.contains("[wt]"), "{screen}");
}

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
    assert!(drawn(&host, &world).contains("→ expand worktrees"));
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
    assert_eq!(task["metadata"]["name"], "read-the-code");
    assert_eq!(
        task["spec"]["workspace"],
        serde_json::json!(["file:///src/notes"])
    );
    assert_eq!(task["spec"]["harness"], "claude");
    assert_eq!(task["spec"]["host"], "local");
}

#[cfg(unix)]
struct StubFleet {
    dir: tempfile::TempDir,
    program: std::path::PathBuf,
}

#[cfg(unix)]
impl StubFleet {
    fn new(hosts: &str) -> StubFleet {
        let dir = tempfile::tempdir().expect("dir");
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
                "#!/bin/sh\ncase \"$1 $2\" in\n  'runtime browse') echo \"$@\" >> '{0}.browse'; echo '{1}'; exit 0 ;;\n  'runtime branches') case \"$4\" in */notes) echo 'error: workspace-git is not on the PATH of mux/box' >&2; exit 1 ;; esac; echo '{2}'; exit 0 ;;\nesac\ncase \"$1\" in\n  get) exit 3 ;;\n  version) echo '{{\"apiVersions\":[\"fleet/v1alpha1\"],\"harnesses\":[\"claude\",\"claude-code\",\"codex\"]}}' ;;\n  runtime) echo '{{\"hosts\":{hosts}}}' ;;\n  repositories) echo '{{\"repositories\":{{\"https://example.invalid/thurbox\":\"/src/thurbox\"}}}}' ;;\n  *) echo \"$@\" >> '{0}'; cat >> '{0}' ;;\nesac\n",
                dir.path().join("asked").display(),
                REMOTE_HOME_LISTING,
                REMOTE_BRANCHES,
            ),
        )
        .expect("fleet");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        StubFleet { dir, program }
    }

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

#[cfg(unix)]
const REMOTE_HOME_LISTING: &str =
    r#"{"dir":"/home/box","entries":[{"name":"app","repo":true},{"name":"notes","repo":false}]}"#;

#[cfg(unix)]
const REMOTE_BRANCHES: &str = r#"{"default":"main","names":["dev","main"]}"#;

#[cfg(unix)]
fn serve(host: &LuaHost, world: &mut World) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        drawn(host, world);
        world.wants = Wants::new(
            host.shared_string("want_bookmarks"),
            host.shared_string("want_browse"),
        )
        .with_branches(host.shared_string("want_branches"));
        world.repos.serve(&world.wants);
        world.repos.poll();
        if !world.repos.in_flight() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "a read never answered"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(unix)]
#[test]
fn a_place_on_another_machine_is_browsed_and_given_a_worktree_there() {
    let stub = StubFleet::new("[\"local\",\"mux/box\"]");
    let link = thurbox::fleet_link::FleetLink::local(stub.program.to_string_lossy());
    let host = host();
    let mut world = World {
        repos: RepoStore::with_fleets(&[link]),
        ..World::default()
    };
    let mux_box = HostRow {
        name: "mux/box".into(),
        detail: "a mux place".into(),
        backend: "mux/box".into(),
        ready: Some(true),
        why: None,
        here: false,
    };
    world.snapshot.fleets = vec![fleet(
        "local",
        "local Fleet",
        FleetState::Up,
        vec![local_host(), mux_box],
        &["claude"],
    )];

    press(&host, &world, "ctrl+n");
    press(&host, &world, "down");
    press(&host, &world, "enter");
    serve(&host, &mut world);
    press(&host, &world, "tab");
    serve(&host, &mut world);
    press(&host, &world, "tab");
    serve(&host, &mut world);
    let screen = drawn(&host, &world);
    assert!(screen.contains("Browse /home/box"), "{screen}");
    assert!(
        screen.contains("app") && screen.contains("notes"),
        "{screen}"
    );
    let browsed = std::fs::read_to_string(stub.dir.path().join("asked.browse"))
        .expect("the place was browsed through Fleet");
    assert!(
        browsed
            .lines()
            .any(|line| line == "runtime browse mux/box --json"),
        "{browsed}"
    );

    press(&host, &world, "enter");
    let commands = host.drain_commands();
    assert!(
        matches!(
            &commands[..],
            [Command::Bookmark { host, path, .. }]
                if host == "local/mux/box" && path == "/home/box/app"
        ),
        "{commands:?}"
    );
    stub.run(commands).expect("the bookmark");
    world.repos.invalidate_bookmarks();
    serve(&host, &mut world);
    let screen = drawn(&host, &world);
    assert!(screen.contains("/home/box/app"), "{screen}");
    assert!(!screen.contains("expand worktrees"), "{screen}");
    press(&host, &world, "right");
    drawn(&host, &world);
    assert_eq!(host.shared_string("want_branches"), None);

    press(&host, &world, "alt+w");
    let screen = drawn(&host, &world);
    assert!(screen.contains("[wt]"), "{screen}");
    press(&host, &world, "enter");
    serve(&host, &mut world);
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("dev") && screen.contains("main"),
        "{screen}"
    );
    press(&host, &world, "enter");
    type_text(&host, &world, "feat");
    press(&host, &world, "enter");
    press(&host, &world, "enter");
    let commands = host.drain_commands();
    assert!(
        matches!(
            &commands[..],
            [Command::Create { repo, worktree: Some(true), base: Some(base), host: Some(h), .. }]
                if repo == "/home/box/app" && base == "main" && h == "mux/box"
        ),
        "{commands:?}"
    );
    stub.run(commands).expect("the create");
    let task = stub.applied();
    assert_eq!(
        task["spec"]["workspace"],
        serde_json::json!(["git+file:///home/box/app?branch=feat#main"])
    );
    assert_eq!(task["spec"]["host"], "mux/box");
}

#[cfg(unix)]
#[test]
fn a_typed_home_on_a_place_elsewhere_is_that_places_home() {
    let stub = StubFleet::new("[\"local\",\"mux/box\"]");
    let link = thurbox::fleet_link::FleetLink::local(stub.program.to_string_lossy());
    let host = host();
    let mut world = World {
        repos: RepoStore::with_fleets(&[link]),
        ..World::default()
    };
    let mux_box = HostRow {
        name: "mux/box".into(),
        detail: "a mux place".into(),
        backend: "mux/box".into(),
        ready: Some(true),
        why: None,
        here: false,
    };
    world.snapshot.fleets = vec![fleet(
        "local",
        "local Fleet",
        FleetState::Up,
        vec![local_host(), mux_box],
        &["claude"],
    )];
    press(&host, &world, "ctrl+n");
    press(&host, &world, "down");
    press(&host, &world, "enter");
    serve(&host, &mut world);
    press(&host, &world, "tab");
    type_text(&host, &world, "~/app/");
    drawn(&host, &world);
    assert_eq!(
        host.shared_string("want_browse").as_deref(),
        Some("local/mux/box\0~"),
        "the home is asked for first"
    );
    press(&host, &world, "backspace");
    press(&host, &world, "enter");
    assert!(host.drain_commands().is_empty());
    let screen = drawn(&host, &world);
    assert!(screen.contains("asking mux/box for its home"), "{screen}");
    type_text(&host, &world, "/");
    serve(&host, &mut world);
    drawn(&host, &world);
    assert_eq!(
        host.shared_string("want_browse").as_deref(),
        Some("local/mux/box\0/home/box/app"),
        "then `~/app` is listed under it"
    );
    press(&host, &world, "backspace");
    press(&host, &world, "enter");
    let commands = host.drain_commands();
    assert!(
        matches!(
            &commands[..],
            [Command::Bookmark { host, path, .. }]
                if host == "local/mux/box" && path == "/home/box/app"
        ),
        "{commands:?}"
    );
}

#[test]
fn another_logins_home_on_a_place_elsewhere_is_refused() {
    let host = host();
    let mut world = World::default();
    let mux_box = HostRow {
        name: "mux/box".into(),
        detail: "a mux place".into(),
        backend: "mux/box".into(),
        ready: Some(true),
        why: None,
        here: false,
    };
    world.snapshot.fleets = vec![fleet(
        "local",
        "local Fleet",
        FleetState::Up,
        vec![local_host(), mux_box],
        &["claude"],
    )];
    press(&host, &world, "ctrl+n");
    press(&host, &world, "down");
    press(&host, &world, "enter");
    press(&host, &world, "tab");
    type_text(&host, &world, "~/");
    drawn(&host, &world);
    assert_eq!(
        host.shared_string("want_browse").as_deref(),
        Some("local/mux/box\0~")
    );
    world
        .repos
        .set_listing_for_test("local/mux/box", "~", Listing::Ready(Vec::new()));
    world
        .repos
        .set_resolved_dir_for_test("local/mux/box", "~", "/home/box");
    world.wants.browse = Some(("local/mux/box".into(), "~".into()));
    drawn(&host, &world);
    assert_eq!(
        host.shared_string("want_browse").as_deref(),
        Some("local/mux/box\0/home/box"),
        "the home is known"
    );
    press(&host, &world, "backspace");
    press(&host, &world, "backspace");
    type_text(&host, &world, "~bob/app");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("only ~ and ~/… are supported on mux/box"),
        "{screen}"
    );
    assert_eq!(host.shared_string("want_browse"), None, "no listing");
    press(&host, &world, "enter");
    assert!(host.drain_commands().is_empty(), "nothing remembered");
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("only ~ and ~/… are supported on mux/box"),
        "{screen}"
    );
}

#[cfg(unix)]
#[test]
fn a_places_refusal_to_list_branches_is_shown() {
    let stub = StubFleet::new("[\"local\",\"mux/box\"]");
    let link = thurbox::fleet_link::FleetLink::local(stub.program.to_string_lossy());
    let host = host();
    let mut repos = RepoStore::with_fleets(&[link]);
    repos.set_bookmarks_for_test("local/mux/box", vec![bookmark("/home/box/notes", None)]);
    let mut world = World {
        repos,
        ..World::default()
    };
    let mux_box = HostRow {
        name: "mux/box".into(),
        detail: "a mux place".into(),
        backend: "mux/box".into(),
        ready: Some(true),
        why: None,
        here: false,
    };
    world.snapshot.fleets = vec![fleet(
        "local",
        "local Fleet",
        FleetState::Up,
        vec![local_host(), mux_box],
        &["claude"],
    )];
    press(&host, &world, "ctrl+n");
    press(&host, &world, "down");
    press(&host, &world, "enter");
    serve(&host, &mut world);
    press(&host, &world, "alt+w");
    press(&host, &world, "enter");
    serve(&host, &mut world);
    let screen = drawn(&host, &world);
    assert!(
        screen.contains("workspace-git is not on the PATH of mux/box"),
        "{screen}"
    );
}

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
            fleet: None,
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
            fleet: None,
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
            fleet: None,
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
            name: "thurbox".into(),
            repo: "/src/thurbox".into(),
            agent: Some("claude".into()),
            host: None,
            fleet: None,
            worktree: Some(false),
            branch: None,
            base: None,
            extras: Vec::new(),
        }]
    );
}

#[test]
fn a_name_is_still_refused_when_there_is_nothing_to_name_it_after() {
    let host = host();
    let world = world_with(Vec::new());
    open(&host, &world);
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
            ..HostRow::default()
        },
    ]);
    world
        .repos
        .set_bookmarks_for_test("ssh:devbox", vec![bookmark("/srv/thurbox", Some(true))]);
    press(&host, &world, "ctrl+n");
    press(&host, &world, "j");
    press(&host, &world, "enter");

    assert_eq!(
        host.shared_string("want_bookmarks").as_deref(),
        Some("ssh:devbox"),
        "repository memory is scoped to the machine the repositories live on"
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
    assert_eq!(
        target.as_deref(),
        Some("ssh:devbox"),
        "the create names the backend, the key repository memory is scoped by"
    );
}

#[test]
fn nothing_to_pick_locally_still_creates_a_session() {
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

#[test]
fn the_empty_session_list_names_the_chord_that_creates_one() {
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
                    std::sync::Arc::new(thurbox::backend::wiring::local_only(&Default::default())),
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
    let host = host();
    let mut world = World::default();
    world.snapshot.hosts = Some(vec![
        local_host(),
        HostRow {
            name: "devbox".into(),
            detail: "me@devbox".into(),
            backend: "ssh:devbox".into(),
            ..HostRow::default()
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
    let index = index_of(&host, MOD);
    let loaded = &host.mods[index];
    assert!(loaded.floats, "the flow overlays rather than taking a slot");
    assert!(
        !loaded.focusable,
        "a closed modal must not be a tab stop, as v1's pickers are not"
    );
}

#[test]
fn every_key_the_flow_uses_is_declared_rather_than_only_handled() {
    let host = host();
    let index = index_of(&host, MOD);
    let declared: Vec<&str> = host.mods[index]
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
    let host = host();
    let mut world = World::default();
    world.snapshot.hosts = Some(vec![
        local_host(),
        HostRow {
            name: "devbox".into(),
            detail: "me@devbox".into(),
            backend: "ssh:devbox".into(),
            ..HostRow::default()
        },
        HostRow {
            name: "builder".into(),
            detail: "me@builder".into(),
            backend: "ssh:builder".into(),
            ..HostRow::default()
        },
    ]);
    press(&host, &world, "ctrl+n");

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
    let host = host();
    let world = world_with(vec![
        bookmark("/src/alpha", Some(true)),
        bookmark("/src/beta", Some(true)),
    ]);
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

#[test]
fn the_repository_list_offers_the_next_step_rather_than_done() {
    let h = host();
    let world = World::default();
    open(&h, &world);
    let screen = drawn(&h, &world);
    assert!(screen.contains("[ Next ]"), "{screen}");
}

#[test]
fn the_repo_step_footer_names_every_key_it_offers_in_full() {
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
    assert!(screen.contains("alt+⏎ next"), "{screen}");
}

#[test]
fn the_search_pills_name_the_filter_they_act_on() {
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
    let h = host();
    let mut world = World::default();
    world.snapshot.hosts = Some(vec![
        local_host(),
        HostRow {
            name: "devbox".into(),
            detail: "me@devbox".into(),
            backend: "ssh:devbox".into(),
            ..HostRow::default()
        },
    ]);
    world.repos.set_bookmarks_for_test("ssh:devbox", Vec::new());
    press(&h, &world, "ctrl+n");
    press(&h, &world, "j");
    press(&h, &world, "enter");
    world.wants.bookmarks = Some("ssh:devbox".into());

    let empty = drawn(&h, &world);
    assert!(empty.contains("No bookmarks"), "the list is empty: {empty}");
    assert!(
        !empty.contains("[ Next ]"),
        "nothing to advance to on a host: {empty}"
    );

    world
        .repos
        .set_bookmarks_for_test("ssh:devbox", vec![bookmark("/srv/thurbox", Some(true))]);
    let listed = drawn(&h, &world);
    assert!(listed.contains("[ Next ]"), "{listed}");
}

#[test]
fn ctrl_enter_confirms_from_the_path_field() {
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
    let h = host();
    let world = World::default();
    open(&h, &world);
    let screen = drawn(&h, &world);
    assert!(screen.contains("[ Next ]"), "{screen}");
}

#[test]
fn a_name_with_no_default_to_fall_back_on_offers_no_pill() {
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

    type_text(&h, &world, "x");
    let typed = drawn(&h, &world);
    assert!(typed.contains("[ Next ]"), "{typed}");
}

#[test]
fn a_name_the_repository_can_answer_for_keeps_its_pill() {
    let h = host();
    let world = World::default();
    open(&h, &world);
    press(&h, &world, "space");
    press(&h, &world, "enter");
    let screen = drawn(&h, &world);
    assert!(screen.contains("thurbox"), "the suggestion shows: {screen}");
    assert!(screen.contains("[ Next ]"), "{screen}");
}

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

fn colours(host: &LuaHost, world: &World, hovered: Option<&Identity>) -> Buffer {
    painted(host, world, hovered).expect("the flow is open").0
}

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
            ..HostRow::default()
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
    let corner = (x - 2, y);
    assert_ne!(
        resting[corner].fg, lit[corner].fg,
        "hovering the unfocused field lights its border"
    );

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
