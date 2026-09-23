//! Global search: what it matches, where it lands, and what it puts back.
//!
//! The strip is easy to build and easy to build wrongly, and every way it goes
//! wrong is invisible from a screenshot: a substring matcher looks like a search
//! box until you type two letters of a hyphenated name, a preview that does not
//! move the list looks like a preview until you press escape and find your
//! selection changed anyway, and `j` navigating instead of typing looks fine
//! until someone searches for a session with a `j` in its name.
//!
//! So these drive the plugin the way the loop does — declared action first, raw
//! key second — and assert on the two things that outlive the frame: the shared
//! selection, and the commands it emits.

use thurbox::kernel::command::Command;
use thurbox::kernel::host::{KeyPress, LuaHost, Published, RenderContext};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::search::{Answer, Hit, Request};
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;
use thurbox::session::SessionState;

const PLUGIN: &str = "search";
const SESSIONS: &str = "sessions";

fn host() -> LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

fn row(id: &str, name: &str, agent: &str, branch: &str) -> SessionRow {
    SessionRow {
        id: id.into(),
        name: name.into(),
        agent: agent.into(),
        status: SessionState::Idle,
        cwd: Some(std::path::PathBuf::from("/src/thurbox")),
        repo: Some("thurbox".into()),
        repos: vec!["thurbox".into()],
        branch: Some(branch.into()),
        base_branch: None,
        backend: "local-tmux".into(),
        backend_id: Some("%1".into()),
        remote_host: None,
        agent_session_id: None,
        parent_id: None,
        display_order: None,
        worktree_count: 0,
        git: None,
        stopped: false,
        hook_state: None,
        reports_as: None,
        detected_agent: None,
        shell_backend_id: None,
        member_dirs: Vec::new(),
    }
}

/// Three sessions whose names do NOT share a substring with the queries below,
/// so a substring matcher fails every fuzzy assertion here.
fn snapshot() -> Snapshot {
    Snapshot {
        sessions: vec![
            row("aaa", "fix-osc52", "claude", "fix/osc52"),
            row("bbb", "fix-branch", "codex", "feat/wsl"),
            row("ccc", "docs-remote-hooks", "claude", "docs/hooks"),
        ],
        ..Snapshot::default()
    }
}

thread_local! {
    /// The answer the kernel is currently publishing, so a keystroke's publish
    /// carries the same one the render before it did — as the loop's would.
    static PUBLISHED: std::cell::RefCell<Option<Answer>> = const { std::cell::RefCell::new(None) };
}

fn publish(host: &LuaHost) {
    let held = PUBLISHED.with(|p| p.borrow().clone());
    publish_with(host, held.as_ref());
}

/// Publish with a content-search answer, as the loop does once one lands.
fn publish_with(host: &LuaHost, search: Option<&Answer>) {
    publish_snapshot(host, &snapshot(), search);
}

fn publish_snapshot(host: &LuaHost, snap: &Snapshot, search: Option<&Answer>) {
    let themes = Themes::load(None);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
        snapshot: snap,
        attach_errors: &Default::default(),
        inflight: &[],
        themes: &themes,
        registry: &registry,
        diffs: &diffs,
        links: &Default::default(),
        search,
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
        selection: None,
        hovered: None,
        printing: &Default::default(),
    })
    .expect("publish");
}

fn render(host: &LuaHost, plugin: &str) {
    publish(host);
    let index = host
        .index_of(plugin)
        .unwrap_or_else(|| panic!("no {plugin}"));
    host.render(
        index,
        RenderContext {
            width: 60,
            height: 12,
            focused: true,
            elapsed: 0.0,
            frame: 0,
        },
    )
    .expect("render");
}

/// One keystroke, routed exactly as the loop routes it: the registry resolves a
/// declared chord to an action, and anything it does not claim is raw input.
fn press(host: &LuaHost, chord: &str) {
    publish(host);
    let index = host.index_of(PLUGIN).expect("no search plugin");
    let mut key = KeyPress {
        name: chord.to_string(),
        ..KeyPress::default()
    };
    if chord.chars().count() == 1 {
        key.ch = chord.chars().next();
    }
    if let Some(rest) = chord.strip_prefix("ctrl+") {
        key.ctrl = true;
        key.name = rest.to_string();
        key.ch = rest.chars().next().filter(|_| rest.chars().count() == 1);
    }
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

fn open(host: &LuaHost) {
    render(host, SESSIONS);
    press(host, "ctrl+/");
    let _ = host.drain_commands();
}

fn type_query(host: &LuaHost, text: &str) {
    for ch in text.chars() {
        press(host, &ch.to_string());
    }
}

fn selected(host: &LuaHost) -> Option<String> {
    host.shared_string("selected")
}

#[test]
fn the_strip_opens_closed_and_takes_focus() {
    let host = host();
    render(&host, SESSIONS);
    assert_ne!(
        host.shared_bool("panels.search"),
        Some(true),
        "search must start closed"
    );

    press(&host, "ctrl+/");
    assert_eq!(host.shared_bool("panels.search"), Some(true));
    assert_eq!(
        host.drain_commands(),
        vec![Command::Focus {
            plugin: "search".into(),
            toggle: false,
        }],
        "an open strip that does not take focus cannot be typed into"
    );
}

#[test]
fn it_matches_a_subsequence_not_a_substring() {
    // `fb` is not a substring of `fix-branch`, which is the whole difference
    // between a search you type three letters into and one you have to spell.
    let host = host();
    open(&host);
    type_query(&host, "fb");
    render(&host, PLUGIN);

    // The single match is previewed, which is how the result set is observable
    // without reading the painted strip.
    assert_eq!(selected(&host).as_deref(), Some("bbb"));
}

#[test]
fn a_letter_is_typed_rather_than_navigating() {
    // `j` and `k` move every other list in this interface. Here they are
    // characters, or a session with a `j` in its name could not be searched for.
    let host = host();
    open(&host);
    type_query(&host, "j");
    render(&host, PLUGIN);
    // Nothing matches a bare `j`, so the preview stayed where it was rather than
    // stepping down the list the way `j` would elsewhere.
    assert_eq!(selected(&host).as_deref(), Some("aaa"));
}

#[test]
fn moving_the_selection_previews_it_in_the_list() {
    // The preview IS the feature: the session list's cursor moves to the result
    // while focus stays in the strip, so a result can be judged before it is
    // chosen.
    let host = host();
    open(&host);
    render(&host, PLUGIN);
    assert_eq!(selected(&host).as_deref(), Some("aaa"));

    press(&host, "down");
    assert_eq!(selected(&host).as_deref(), Some("bbb"));
    press(&host, "down");
    assert_eq!(selected(&host).as_deref(), Some("ccc"));
    press(&host, "up");
    assert_eq!(selected(&host).as_deref(), Some("bbb"));

    // And the list really followed, rather than the value merely sticking.
    render(&host, SESSIONS);
    assert_eq!(selected(&host).as_deref(), Some("bbb"));
}

#[test]
fn cancelling_puts_back_what_was_selected_before() {
    // Previewing has already moved a cursor by the time you press escape, so
    // closing without restoring would make cancelling a way to change the
    // selection by accident.
    let host = host();
    render(&host, SESSIONS);
    host.set_shared_string("selected", "ccc");
    render(&host, SESSIONS);

    press(&host, "ctrl+/");
    let _ = host.drain_commands();
    assert_eq!(
        selected(&host).as_deref(),
        Some("aaa"),
        "opening previews the row under the cursor"
    );
    press(&host, "down");
    assert_eq!(selected(&host).as_deref(), Some("bbb"), "previewed");

    press(&host, "esc");
    assert_eq!(selected(&host).as_deref(), Some("ccc"), "put back");
    assert_eq!(host.shared_bool("panels.search"), Some(false));
    assert_eq!(host.shared_string("search.query"), None, "query cleared");
}

#[test]
fn enter_keeps_the_jump_and_lands_in_the_session() {
    // v1's Enter puts you IN the result: a session result focuses that session's
    // terminal, not the row you picked it from.
    let host = host();
    render(&host, SESSIONS);
    host.set_shared_string("selected", "aaa");
    render(&host, SESSIONS);

    press(&host, "ctrl+/");
    let _ = host.drain_commands();
    type_query(&host, "dr");
    press(&host, "enter");

    assert_eq!(selected(&host).as_deref(), Some("ccc"), "the jump survived");
    assert_eq!(host.shared_bool("panels.search"), Some(false));
    assert_eq!(
        host.drain_commands(),
        vec![Command::Focus {
            plugin: "agent".into(),
            toggle: false,
        }]
    );
}

#[test]
fn a_session_found_by_something_other_than_its_name_still_matches() {
    // v1 searches the agent and the branch too, so a session can be found by
    // what it is working on rather than by what it was called.
    let host = host();
    open(&host);
    type_query(&host, "codex");
    render(&host, PLUGIN);
    assert_eq!(selected(&host).as_deref(), Some("bbb"));
}

#[test]
fn the_query_is_published_so_the_panes_can_highlight_themselves() {
    // Search decides WHAT matches; the pane holding the row decides how a match
    // looks in it. That contract is the shared query — without it the strip
    // would have to reprint rows it does not own.
    let host = host();
    open(&host);
    type_query(&host, "fix");
    assert_eq!(host.shared_string("search.query").as_deref(), Some("fix"));
}

#[test]
fn backspace_widens_the_search_again() {
    // The caret editing v1's strip has. Without it a mistyped query can only be
    // escaped out of and started over.
    let host = host();
    open(&host);
    type_query(&host, "docs");
    // Rendered before asserting because rendering is what previews: typing
    // records the query, and the frame it causes clamps the cursor to the new
    // results and points the list at whatever that lands on. The strip used to
    // do both — the scan of every session, and once a query settled every
    // session's *screen*, ran twice per keystroke for the same answer.
    render(&host, PLUGIN);
    assert_eq!(selected(&host).as_deref(), Some("ccc"));

    for _ in 0..4 {
        press(&host, "backspace");
    }
    assert_eq!(host.shared_string("search.query").as_deref(), Some(""));
    render(&host, PLUGIN);
    // Everything matches an empty query, so the cursor is back on the first row.
    assert_eq!(selected(&host).as_deref(), Some("aaa"));
}

#[test]
fn the_opening_chord_is_declared_so_help_lists_it_and_it_can_be_rebound() {
    let host = host();
    let index = host.index_of(PLUGIN).expect("no search plugin");
    let bindings = &host.plugins[index].bindings;
    assert!(
        bindings.iter().any(|binding| binding.chord == "ctrl+/"),
        "a key that only exists inside on_key is invisible to help and unrebindable"
    );
}

/// The kernel's answer to `query`: one hit per `(session, line, scroll)`.
fn answer(query: &str, hits: &[(&str, &str, usize)]) -> Answer {
    Answer {
        request: Request {
            query: query.into(),
            sessions: None,
        },
        hits: hits
            .iter()
            .map(|(session, text, scroll)| Hit {
                session: (*session).into(),
                shell: false,
                text: (*text).into(),
                ranges: vec![],
                back: scroll + 5,
                scroll: *scroll,
                row: 7,
                exact: true,
                score: 100,
            })
            .collect(),
        total: hits.len(),
        sessions: 3,
        lines: 3000,
        ..Answer::default()
    }
}

/// Render with `search` as the kernel's published answer.
fn render_answered(host: &LuaHost, search: Option<&Answer>) {
    PUBLISHED.with(|p| *p.borrow_mut() = search.cloned());
    let index = host.index_of(PLUGIN).expect("no search plugin");
    publish_with(host, search);
    host.render(
        index,
        RenderContext {
            width: 60,
            height: 12,
            focused: true,
            elapsed: 0.0,
            frame: 0,
        },
    )
    .expect("render");
}

#[test]
fn an_open_strip_warms_the_terminals_and_a_keystroke_asks_at_once() {
    // Open with nothing typed, the strip asks with an empty query: the kernel
    // reads every history into its cache and matches nothing, so the first
    // keystroke is matched against text already read. And a keystroke asks on
    // the very next frame — the worker gives up a run the moment a newer one
    // supersedes it, so a debounce would only add its wait to every keystroke.
    let host = host();
    open(&host);
    render(&host, PLUGIN);
    assert_eq!(host.shared_string("want_content").as_deref(), Some(""));

    type_query(&host, "err");
    render(&host, PLUGIN);
    assert_eq!(host.shared_string("want_content").as_deref(), Some("err"));
}

#[test]
fn a_session_is_found_by_a_line_in_its_terminal() {
    // The half that finds a session by the error in it, rather than by anything
    // anyone thought to name it.
    let host = host();
    open(&host);
    type_query(&host, "ENOSPC");
    render_answered(
        &host,
        Some(&answer(
            "ENOSPC",
            &[("ccc", "error: ENOSPC no space left", 0)],
        )),
    );
    // Nothing matches `ENOSPC` by name, agent, branch or repo — only the text.
    assert_eq!(selected(&host).as_deref(), Some("ccc"));
    // And the session list is told, so it lights a row it could not have
    // matched against its own fields.
    assert_eq!(host.shared_string("search.matches").as_deref(), Some("ccc"));
}

#[test]
fn an_answer_to_an_older_query_is_not_shown() {
    // The kernel answers a frame or two after the query settles, so while the
    // next query runs the last answer is still published. Showing it under the
    // new query would light the wrong characters and then vanish.
    let host = host();
    open(&host);
    type_query(&host, "ENOSPC");
    render_answered(
        &host,
        Some(&answer(
            "ENOSP",
            &[("ccc", "error: ENOSPC no space left", 0)],
        )),
    );
    assert_eq!(host.shared_string("search.matches").as_deref(), Some(""));
}

#[test]
fn stepping_onto_a_text_hit_scrolls_its_terminal_to_the_line() {
    // Preview is the feature: the agent pane scrolls back to the line while
    // focus stays in the strip. The request is left in `store` and the agent
    // pane is asked to read it — an action carries no argument.
    let host = host();
    open(&host);
    type_query(&host, "ENOSPC");
    let found = answer(
        "ENOSPC",
        &[("ccc", "first ENOSPC", 120), ("aaa", "second ENOSPC", 40)],
    );
    render_answered(&host, Some(&found));
    let _ = host.drain_commands();

    press(&host, "down");
    assert_eq!(selected(&host).as_deref(), Some("aaa"));
    assert_eq!(
        host.shared_string("terminal.reveal").as_deref(),
        Some("aaa 40 7")
    );
    assert!(host.drain_commands().contains(&Command::Action {
        owner: "plugins/65_search.lua".into(),
        action: "terminal.reveal".into(),
    }));

    // Stepping back puts the terminal it left at the bottom in the same
    // request, so previews do not leave a trail of scrolled terminals.
    press(&host, "up");
    assert_eq!(
        host.shared_string("terminal.reveal").as_deref(),
        Some("-aaa;ccc 120 7")
    );

    // Cancelling scrolls the last one back too.
    press(&host, "esc");
    assert_eq!(
        host.shared_string("terminal.reveal").as_deref(),
        Some("-ccc")
    );
}

#[test]
fn opening_a_text_hit_lands_the_agent_pane_on_the_line() {
    // The operator's failure, at the plugin level: opening a hit must show the
    // session SCROLLED TO the match, not merely focus it. The agent pane is
    // driven the way the kernel routes the action, and its surface node is
    // what the kernel paints with.
    let host = host();
    open(&host);
    type_query(&host, "ENOSPC");
    render_answered(
        &host,
        Some(&answer("ENOSPC", &[("ccc", "error: ENOSPC", 120)])),
    );
    let _ = host.drain_commands();
    press(&host, "enter");

    let commands = host.drain_commands();
    assert!(
        commands.contains(&Command::Focus {
            plugin: "agent".into(),
            toggle: false,
        }),
        "{commands:?}"
    );
    let agent = host.index_of("agent").expect("agent pane");
    host.on_action(agent, "terminal.reveal").expect("reveal");
    publish(&host);
    let node = host
        .render(
            agent,
            RenderContext {
                width: 80,
                height: 24,
                focused: true,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render")
        .node;
    let tree = format!("{node:?}");
    assert!(tree.contains("scroll: 120"), "{tree}");
    // And the line it landed on is marked, at the row the kernel said.
    assert!(tree.contains("mark: Some(7)"), "{tree}");
}

#[test]
fn every_word_must_match_in_any_order_across_fields() {
    // `claude docs`: the agent is claude, the name has docs. aaa is claude but
    // not docs; only ccc is both.
    let host = host();
    open(&host);
    type_query(&host, "docs claude");
    render(&host, PLUGIN);
    assert_eq!(host.shared_string("search.matches").as_deref(), Some("ccc"));
}

#[test]
fn a_capital_makes_the_query_case_sensitive() {
    let host = host();
    open(&host);
    type_query(&host, "osc");
    render(&host, PLUGIN);
    assert_eq!(host.shared_string("search.matches").as_deref(), Some("aaa"));

    for _ in 0..3 {
        press(&host, "backspace");
    }
    type_query(&host, "OSC");
    render(&host, PLUGIN);
    assert_eq!(host.shared_string("search.matches").as_deref(), Some(""));
}

#[test]
fn an_exact_match_ranks_above_a_subsequence() {
    // `ch` is a subsequence of `docs-hub` and a substring of `fix-branch`; the
    // list puts docs-hub first, the ranking must not.
    let host = host();
    let snap = Snapshot {
        sessions: vec![
            row("ccc", "docs-hub", "codex", "x"),
            row("bbb", "fix-branch", "codex", "y"),
        ],
        ..Snapshot::default()
    };
    publish_snapshot(&host, &snap, None);
    let index = host.index_of(PLUGIN).expect("search");
    host.on_action(index, "search.open").expect("open");
    for ch in "ch".chars() {
        publish_snapshot(&host, &snap, None);
        let key = KeyPress {
            name: ch.to_string(),
            ch: Some(ch),
            ..KeyPress::default()
        };
        host.on_key(index, &key).expect("key");
    }
    publish_snapshot(&host, &snap, None);
    host.render(
        index,
        RenderContext {
            width: 60,
            height: 12,
            focused: true,
            elapsed: 0.0,
            frame: 0,
        },
    )
    .expect("render");
    assert_eq!(
        host.shared_string("search.matches").as_deref(),
        Some("bbb ccc")
    );
}

#[test]
fn a_filter_narrows_the_terminals_searched() {
    // `in:` and `repo:` never reach the kernel as text; they become the list of
    // sessions its search is limited to.
    let host = host();
    open(&host);
    type_query(&host, "in:docs err");
    render_answered(&host, None);
    assert_eq!(host.shared_string("want_content").as_deref(), Some("err"));
    assert_eq!(
        host.shared_string("want_content.sessions").as_deref(),
        Some("ccc")
    );
}

#[test]
fn tab_cycles_what_is_searched() {
    let host = host();
    open(&host);
    type_query(&host, "err");
    render_answered(&host, None);
    assert!(host.shared_string("want_content").is_some());

    // text only: the terminals are still asked, and no name matches listed.
    press(&host, "tab");
    render_answered(&host, None);
    assert!(host.shared_string("want_content").is_some());

    // names only: nothing is asked of the terminals.
    press(&host, "tab");
    render_answered(&host, None);
    assert_eq!(host.shared_string("want_content"), None);
}

#[test]
fn closing_the_strip_stops_the_terminals_being_read() {
    let host = host();
    open(&host);
    type_query(&host, "err");
    render_answered(&host, None);
    assert!(host.shared_string("want_content").is_some());

    press(&host, "esc");
    assert_eq!(
        host.shared_string("want_content"),
        None,
        "a closed strip must not leave the kernel searching terminals"
    );
    assert_eq!(host.shared_string("search.matches"), None);
}

/// Enough matches to overflow the strip, painted through the real kernel.
///
/// The strip's own arithmetic decides how many rows it asks the list for, and
/// asking for one row more than the rect holds is not a clipped list: an
/// over-subscribed box hands its whole rect to the FIRST child and nothing to the
/// others, so one line paints and every result — selection marker included —
/// disappears. It went wrong by subtracting the frame and the scope line but not
/// the query row, which put the cliff at eight matches in a 12-row strip: a
/// count reachable by having eight sessions and typing nothing.
#[test]
fn every_match_still_paints_when_the_results_fill_the_strip() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use thurbox::kernel::paint::{render as paint_render, PlaceholderSurfaces};

    const WIDTH: u16 = 60;
    const HEIGHT: u16 = 12;

    for count in 1..=14usize {
        let sessions: Vec<SessionRow> = (0..count)
            .map(|i| row(&format!("s{i}"), &format!("session-{i}"), "claude", "main"))
            .collect();
        let host = host();
        open(&host);

        let themes = Themes::load(None);
        let mut registry = Registry::default();
        let (bindings, settings) = host.declarations();
        registry.declare(bindings, settings);
        let diffs = thurbox::kernel::diff::DiffStore::new();
        let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
        let snap = Snapshot {
            sessions,
            ..Snapshot::default()
        };
        host.publish(&Published {
            epoch: thurbox::kernel::host::Epoch::always_fresh(),
            snapshot: &snap,
            attach_errors: &Default::default(),
            inflight: &[],
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
            repos: &repos,
            wants: &Default::default(),
            focus: None,
            selection: None,
            hovered: None,
            printing: &Default::default(),
        })
        .expect("publish");

        let index = host.index_of(PLUGIN).expect("no search plugin");
        let node = host
            .render(
                index,
                RenderContext {
                    width: WIDTH,
                    height: HEIGHT,
                    focused: true,
                    elapsed: 0.0,
                    frame: 0,
                },
            )
            .expect("render")
            .node;

        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("terminal");
        terminal
            .draw(|frame| paint_render(frame, frame.area(), &node, &PlaceholderSurfaces))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let painted: Vec<String> = (0..HEIGHT)
            .map(|y| {
                (0..WIDTH)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect();
        let screen = painted.join("\n");

        // The cursor starts on the first result, so its marker is the one thing
        // that must be on screen at every count.
        assert!(
            screen.contains('▸'),
            "{count} matches: the selected result vanished\n{screen}"
        );
        assert!(
            screen.contains("session-0"),
            "{count} matches: no result rows painted\n{screen}"
        );
    }
}

/// The pane draws the match, and the selection bar must not paint over it.
///
/// Previewing moves this list's cursor ONTO the result, so the row carrying the
/// highlight is always the selected one — which made this the one row where the
/// match was invisible: the bar patched `fg` over every span, leaving the matched
/// characters in the selection's own colour with only their underline to show
/// for it. v1 layered the two the other way round (`highlight_style` is built on
/// top of the row's base style), and this pins that order.
#[test]
fn a_match_keeps_its_colour_under_the_selection_bar() {
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;
    use thurbox::kernel::node::parse_color;
    use thurbox::kernel::paint::{render as paint_render, PlaceholderSurfaces};

    const WIDTH: u16 = 40;
    const HEIGHT: u16 = 10;

    let themes = Themes::load(None);
    let roles = themes.roles();
    let colour =
        |role: &str| -> Color { parse_color(roles.get(role).expect("role")).expect("colour") };
    let accent = colour("accent");
    let selection_bg = colour("selection_bg");

    // Paint the session list, optionally with a query in force.
    let cells = |query: Option<&str>| -> Vec<(String, Color, Color)> {
        let host = host();
        if let Some(text) = query {
            open(&host);
            type_query(&host, text);
            // The strip previews as it renders, which is what selects the row.
            render(&host, PLUGIN);
            assert_eq!(selected(&host).as_deref(), Some("bbb"));
        } else {
            render(&host, SESSIONS);
        }
        publish(&host);
        let index = host.index_of(SESSIONS).expect("no sessions plugin");
        let node = host
            .render(
                index,
                RenderContext {
                    width: WIDTH,
                    height: HEIGHT,
                    focused: true,
                    elapsed: 0.0,
                    frame: 0,
                },
            )
            .expect("render")
            .node;
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("terminal");
        terminal
            .draw(|frame| paint_render(frame, frame.area(), &node, &PlaceholderSurfaces))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        (0..HEIGHT)
            .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
            .map(|(x, y)| {
                let cell = &buffer[(x, y)];
                (cell.symbol().to_string(), cell.fg, cell.bg)
            })
            .collect()
    };

    // `fb` matches `fix-branch` and nothing else, so the two lit characters are
    // its `f` and its `b`.
    let searched = cells(Some("fb"));
    let bar: Vec<&(String, Color, Color)> = searched
        .iter()
        .filter(|(_, _, bg)| *bg == selection_bg)
        .collect();
    assert!(
        !bar.is_empty(),
        "the previewed row should carry the selection bar"
    );
    let lit: Vec<&str> = bar
        .iter()
        .filter(|(_, fg, _)| *fg == accent)
        .map(|(symbol, _, _)| symbol.as_str())
        .collect();
    assert_eq!(
        lit,
        vec!["f", "b"],
        "the matched characters must keep the accent through the selection bar"
    );

    // The control: with nothing searched, nothing in the bar is accent — so the
    // assertion above is reading the highlight and not some other affordance
    // that happens to be lit.
    let resting = cells(None);
    assert!(
        !resting
            .iter()
            .any(|(_, fg, bg)| *bg == selection_bg && *fg == accent),
        "an unsearched row has no matched characters to light"
    );
}

/// The strip as the kernel paints it, at `width`×`height`, after the query has
/// settled with `search` published.
fn painted_strip(host: &LuaHost, width: u16, height: u16, search: Option<&Answer>) -> String {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use thurbox::kernel::paint::{render as paint_render, PlaceholderSurfaces};

    render_answered(host, search);
    publish_with(host, search);
    let index = host.index_of(PLUGIN).expect("no search plugin");
    let node = host
        .render(
            index,
            RenderContext {
                width,
                height,
                focused: true,
                elapsed: 2.0,
                frame: 0,
            },
        )
        .expect("render")
        .node;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| paint_render(frame, frame.area(), &node, &PlaceholderSurfaces))
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

fn hit_row(session: &str, text: &str, back: usize, positions: std::ops::Range<usize>) -> Hit {
    Hit {
        session: session.into(),
        shell: false,
        text: text.into(),
        ranges: vec![(positions.start, positions.end)],
        back,
        scroll: back,
        row: 5,
        exact: true,
        score: 100,
    }
}

#[test]
fn a_text_result_says_where_it_is_and_keeps_its_line_when_narrow() {
    let found = Answer {
        request: Request {
            query: "flake".into(),
            sessions: None,
        },
        hits: vec![hit_row(
            "ccc",
            "why does the login test flake?",
            112,
            24..29,
        )],
        total: 1,
        sessions: 3,
        lines: 3000,
        ..Answer::default()
    };
    let host = host();
    open(&host);
    type_query(&host, "flake");
    let wide = painted_strip(&host, 100, 10, Some(&found));
    let row = wide
        .lines()
        .find(|line| line.contains("112↑"))
        .unwrap_or_else(|| panic!("no located result\n{wide}"));
    assert!(row.contains("docs-remote-hooks"), "{wide}");
    assert!(row.contains("why does the login test flake?"), "{wide}");
    assert!(
        wide.contains("text 1 in 3,000 lines of 3 sessions"),
        "{wide}"
    );

    // Narrow: the session column goes before the line does.
    let narrow = painted_strip(&host, 36, 10, Some(&found));
    let row = narrow
        .lines()
        .find(|line| line.contains("112↑"))
        .unwrap_or_else(|| panic!("no located result\n{narrow}"));
    assert!(!row.contains("docs-remote"), "{narrow}");
    assert!(row.contains("why does"), "{narrow}");
}

#[test]
fn nothing_found_says_what_was_searched() {
    let empty = Answer {
        request: Request {
            query: "zzqx".into(),
            sessions: None,
        },
        sessions: 3,
        lines: 3000,
        ..Answer::default()
    };
    let host = host();
    open(&host);
    type_query(&host, "zzqx");
    let strip = painted_strip(&host, 100, 10, Some(&empty));
    assert!(
        strip.contains(
            "no match for zzqx in the names or terminal text of 3 sessions (3,000 lines)"
        ),
        "{strip}"
    );
    assert!(strip.contains("no matches"), "{strip}");
}

#[test]
fn while_the_terminals_are_read_the_strip_says_so() {
    // The answer lands a frame or two after the query settles; until it does
    // the strip must not look finished.
    let host = host();
    open(&host);
    type_query(&host, "zzqx");
    let strip = painted_strip(&host, 100, 10, None);
    assert!(strip.contains("searching"), "{strip}");
}

#[test]
fn an_invalid_regex_says_why() {
    let broken = Answer {
        request: Request {
            query: "/(oops/".into(),
            sessions: None,
        },
        error: Some("not a valid regex: unclosed group".into()),
        ..Answer::default()
    };
    let host = host();
    open(&host);
    type_query(&host, "/(oops/");
    let strip = painted_strip(&host, 100, 10, Some(&broken));
    assert!(strip.contains("not a valid regex"), "{strip}");
}
