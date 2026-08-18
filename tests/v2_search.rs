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
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;

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
        status: "idle".into(),
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
        hook_state: None,
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

fn publish(host: &LuaHost) {
    publish_with(host, &Default::default());
}

/// Publish with terminal text, as the loop does once a plugin asks for it.
fn publish_with(host: &LuaHost, content: &std::collections::HashMap<String, String>) {
    let themes = Themes::load(None);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        snapshot: &snapshot(),
        attach_errors: &Default::default(),
        inflight: &[],
        themes: &themes,
        registry: &registry,
        diffs: &diffs,
        links: &Default::default(),
        content,
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
            plugin: "search".into()
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
            plugin: "agent".into()
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

/// A screen for one session, as the kernel would serve it.
fn screens(session: &str, text: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    map.insert(session.to_string(), text.to_string());
    map
}

/// Render after the debounce has elapsed, which is what makes the pane ask.
fn render_after_debounce(host: &LuaHost, content: &std::collections::HashMap<String, String>) {
    let index = host.index_of(PLUGIN).expect("no search plugin");
    // Twice: the first render notices the query changed and starts the clock,
    // the second finds it unmoved and asks.
    for elapsed in [0.0_f64, 1.0] {
        publish_with(host, content);
        host.render(
            index,
            RenderContext {
                width: 60,
                height: 12,
                focused: true,
                elapsed,
                frame: 0,
            },
        )
        .expect("render");
    }
}

#[test]
fn nothing_is_asked_of_the_terminals_until_a_query_settles() {
    // Every agent's screen on every frame is not a thing to publish
    // speculatively, so the read is served only while the pane asks — and it
    // asks only once the query has stood still.
    let host = host();
    open(&host);
    assert_eq!(
        host.shared_string("want_content"),
        None,
        "an empty query asks nothing"
    );

    type_query(&host, "err");
    render(&host, PLUGIN);
    assert_eq!(
        host.shared_string("want_content"),
        None,
        "the first frame after a keystroke starts the clock, it does not ask"
    );

    render_after_debounce(&host, &Default::default());
    assert_eq!(host.shared_string("want_content").as_deref(), Some("err"));
}

#[test]
fn a_session_is_found_by_what_its_terminal_is_showing() {
    // The half that finds a session by the error in it, rather than by anything
    // anyone thought to name it.
    let host = host();
    open(&host);
    type_query(&host, "ENOSPC");
    render_after_debounce(
        &host,
        &screens("ccc", "writing...\nerror: ENOSPC no space left\n$ "),
    );

    // Nothing matches `ENOSPC` by name, agent, branch or repo — only the screen.
    assert_eq!(selected(&host).as_deref(), Some("ccc"));
}

#[test]
fn a_session_that_already_matched_is_not_listed_twice() {
    // v1 skips the content scan for a session whose metadata matched: one result
    // pretending to be two is worse than one.
    let host = host();
    open(&host);
    type_query(&host, "docs");
    render_after_debounce(&host, &screens("ccc", "docs docs docs"));

    // Still one result, so stepping past it wraps rather than landing on a
    // duplicate of the same session.
    press(&host, "down");
    assert_eq!(selected(&host).as_deref(), Some("ccc"));
}

#[test]
fn a_screen_hit_matches_as_a_substring_not_a_subsequence() {
    // Subsequence matching over a whole screen would match nearly anything, so
    // the content pass is a plain `contains` — v1 draws the same line.
    let host = host();
    open(&host);
    type_query(&host, "xyz");
    render_after_debounce(&host, &screens("ccc", "x marks y then z"));
    // The letters appear in order but not together, so this is not a match.
    assert_eq!(
        selected(&host).as_deref(),
        Some("aaa"),
        "no result to preview"
    );
}

#[test]
fn closing_the_strip_stops_the_terminals_being_read() {
    let host = host();
    open(&host);
    type_query(&host, "err");
    render_after_debounce(&host, &Default::default());
    assert!(host.shared_string("want_content").is_some());

    press(&host, "esc");
    assert_eq!(
        host.shared_string("want_content"),
        None,
        "a closed strip must not leave the kernel scanning screens"
    );
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
            snapshot: &snap,
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
