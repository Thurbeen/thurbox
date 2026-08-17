//! Where v2 stands against the v1 recordings.
//!
//! `tests/v1_recordings.rs` captured what v1 draws. This measures the v2
//! plugins against it — and records the divergences **as assertions**, so the
//! gap is a number that can only shrink, not a claim in a document.
//!
//! Cell-exact parity is not asserted, because it is not yet true: the bundled
//! plugins are a reimplementation, not a port, and they differ in visible ways
//! (rule-style repo headers, the worktree mark, status dots in the border
//! title). What *is* asserted is every property that must hold for a pane to be
//! a replacement rather than a lookalike — and each remaining difference is
//! named here rather than left to be discovered after v1 is gone.
//!
//! `v2-retire-v1` is gated on this file asserting cell-exact equality. Until it
//! does, that change cannot honestly run.

use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use thurbox::kernel::host::{LuaHost, Published, RenderContext};
use thurbox::kernel::layout::resolve;
use thurbox::kernel::modals::ModalKind;
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;
use thurbox::ui::layout::compute_layout;

fn host() -> LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

fn row(name: &str, repo: &str, status: &str, branch: &str) -> SessionRow {
    SessionRow {
        id: format!("{name}-0000-0000-0000-000000000000"),
        name: name.into(),
        agent: "claude".into(),
        status: status.into(),
        cwd: Some(std::path::PathBuf::from(format!("/src/{repo}"))),
        repo: Some(repo.into()),
        repos: vec![repo.into()],
        branch: Some(branch.into()),
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
        sessions: vec![
            row("fix-osc52", "thurbox", "working", "fix/osc52"),
            row("add-wsl", "thurbox", "blocked", "feat/wsl"),
            row("perf-cache", "thurbox", "done", "perf/cache"),
            row("update-deps", "website", "idle", "chore/deps"),
        ],
        ..Snapshot::default()
    }
}

fn render(host: &LuaHost, plugin: &str, width: u16, height: u16) -> String {
    render_with(host, plugin, width, height, &sample(), &Default::default())
}

fn render_with(
    host: &LuaHost,
    plugin: &str,
    width: u16,
    height: u16,
    snapshot: &Snapshot,
    meta: &std::collections::HashMap<String, thurbox::kernel::terminal::AgentMeta>,
) -> String {
    let themes = Themes::load(None);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        snapshot,
        attach_errors: &Default::default(),
        inflight: &[],
        themes: &themes,
        registry: &registry,
        diffs: &diffs,
        links: &Default::default(),
        content: &Default::default(),
        meta,
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

    let index = host
        .plugins
        .iter()
        .position(|p| p.name == plugin)
        .unwrap_or_else(|| panic!("no plugin {plugin}"));
    let node = host
        .render(
            index,
            RenderContext {
                width,
                height,
                focused: true,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render")
        .node;

    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| {
            thurbox::kernel::paint::render(
                frame,
                frame.area(),
                &node,
                &thurbox::kernel::paint::PlaceholderSurfaces,
            )
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_session_list_carries_everything_v1_showed() {
    // Not cell-exact — the properties. Each of these is something a user would
    // notice missing.
    let screen = render(&host(), "sessions", 44, 16);

    for name in ["fix-osc52", "add-wsl", "perf-cache", "update-deps"] {
        assert!(screen.contains(name), "{name} missing:\n{screen}");
    }
    for repo in ["thurbox", "website"] {
        assert!(
            screen.contains(repo),
            "repo header {repo} missing:\n{screen}"
        );
    }
    // v1 does NOT print the branch here — a worktree session is flagged with
    // the `⑂` mark (painted in the branch colour) and the branch text is left
    // to the info panel. Asserting the name would diverge from the golden.
    assert!(screen.contains('⑂'), "worktree mark missing:\n{screen}");
    assert!(
        !screen.contains("fix/osc52"),
        "branch text belongs to the info panel, not the list:\n{screen}"
    );
    // One glyph per status, as v1 draws.
    for glyph in ['◆', '●', '○'] {
        assert!(
            screen.contains(glyph),
            "status glyph {glyph} missing:\n{screen}"
        );
    }
}

#[test]
fn a_row_carries_the_agents_activity_and_a_blocked_sessions_message() {
    // v1 puts the agent's OSC window title after the name, muted, and swaps in
    // the OSC 9/777 message for a blocked row in the status colour. Both come off
    // the live pane rather than the database, so they are published separately.
    let host = host();
    let mut meta = std::collections::HashMap::new();
    meta.insert(
        "fix-osc52-0000-0000-0000-000000000000".to_string(),
        thurbox::kernel::terminal::AgentMeta {
            activity: Some("Compacting conversation".to_string()),
            notification: None,
        },
    );
    meta.insert(
        "add-wsl-0000-0000-0000-000000000000".to_string(),
        thurbox::kernel::terminal::AgentMeta {
            activity: Some("waiting".to_string()),
            notification: Some("Needs your approval".to_string()),
        },
    );
    let screen = render_with(&host, "sessions", 60, 12, &sample(), &meta);

    assert!(
        screen.contains("Compacting conversation"),
        "the activity title is missing:\n{screen}"
    );
    // A blocked row prefers the notification over both the activity and the
    // bare word "Blocked".
    assert!(
        screen.contains("Needs your approval"),
        "the blocked message is missing:\n{screen}"
    );
    assert!(
        !screen.contains("waiting"),
        "a blocked row must not show its activity title:\n{screen}"
    );
}

#[test]
fn a_multi_repo_session_gets_its_own_group_naming_every_repo() {
    // v1 groups by the repo *set* and labels the header with every repo joined
    // by " + ", so a session spanning two repos is its own group rather than
    // being filed under its primary.
    let host = host();
    let mut snapshot = sample();
    snapshot.sessions[0].repos = vec!["thurbox".to_string(), "website".to_string()];
    let screen = render_with(&host, "sessions", 60, 14, &snapshot, &Default::default());

    assert!(
        screen.contains("thurbox + website"),
        "the multi-repo group label is missing:\n{screen}"
    );
    // The single-repo groups still exist beside it.
    assert!(screen.contains("── thurbox "), "{screen}");
    assert!(screen.contains("── website "), "{screen}");
}

#[test]
fn the_layout_thresholds_match_v1_exactly() {
    // This one IS exact, because it is arithmetic rather than styling: at each
    // width, the same columns exist or do not.
    let host = host();
    for width in [60u16, 100, 160] {
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height: 30,
        };
        let v1 = compute_layout(area, true, true, true, true, false, true, 2, true);
        let v2 = resolve(&host.arrangement(width, 30).expect("layout"), area);
        let v2_has = |slot: &str| v2.iter().any(|s| s.slot == slot);

        assert_eq!(
            v1.left_panel.is_some(),
            v2_has("sessions"),
            "session column disagrees at width {width}"
        );
        // v1 always keeps a centre pane; so must v2, at every width.
        assert!(v2_has("center"), "no centre pane at width {width}");
    }
}

/// The remaining gap to cell-exact parity, as a list rather than a claim.
///
/// `v2-retire-v1` cannot honestly run while this is non-empty. Shrinking it is
/// the work; asserting its exact contents is how it stays honest — adding a
/// divergence without noticing is a test failure.
///
/// This list covers what v2 *draws* differently. It is deliberately NOT the
/// whole parity gap: a surface-by-surface audit found behavioural gaps this
/// framing cannot express (a created session that never attaches a terminal,
/// automations that never fire), and those are tracked in
/// `openspec/changes/v2-parity-gaps/`. A rendering divergence belongs here; a
/// missing behaviour belongs there.
const KNOWN_DIVERGENCES: &[&str] = &[
    "focus levels: v1 picks one of three (`ui::FocusLevel`) per pane; v2 publishes a focused \
     boolean and each pane maps its own unfocused state, so both surviving panes reach v1's \
     `Active` but nothing currently reaches `Inactive` — the level v1 uses for the session list \
     while the automations pane owns the centre",
    "tab strip: v1 draws the strip on the centre pane whichever tab is active; v2 draws it on the \
     agent and shell tabs, which are one plugin, but not on review, which is still a centre-slot \
     plugin of its own and brings its own chrome",
    "quit chord: v1 lets the help editor rebind `ctrl+q` like any other action; v2 refuses it, \
     keeping a fixed escape hatch so a capture cannot lock you out of the application",
    "search selection keys: v1 accepts `ctrl+p`/`ctrl+n` in the search strip as well as the \
     arrows, because its search focus captures input before the keybinding table; v2 routes \
     every chord through one registry where a plugin-scoped claim does not outrank a global \
     one, so declaring them would take `ctrl+n` from new-session everywhere — the arrows are \
     declared instead, and either can be rebound",
    "remembered repositories: v1 loads them once when the picker opens and appends a newly added \
     path at the end of the list; v2 re-reads them and an added path moves to the top, because \
     recency ordering is what identifies the row that was just added (its expanded path is only \
     known on the worker)",
    "in-flow refusals: v1 reports \"Not a git repo\" and friends in the status bar; v2 shows them \
     on the flow's own footer row, because the message band is kernel chrome a plugin cannot write",
    "creating with nothing selected: v1 spawns in the local home directory for any target; v2 does \
     the same locally but asks a remote target for a repository, since a local home is not a path \
     on that machine",
];

#[test]
fn the_gap_to_parity_is_named_and_does_not_grow() {
    // Seven known differences. Three are the standing ones (the focus levels, the
    // tab strip and the quit chord); three arrived with the new-session flow,
    // each because v2's shape forces it — a read it cannot identify by name,
    // chrome it cannot write, and a path that means nothing on another machine;
    // one arrived with search, for the same kind of reason (one key registry
    // rather than a focus that bypasses it). Recorded rather than discovered
    // later, which is what this assertion is for.
    assert_eq!(
        KNOWN_DIVERGENCES.len(),
        7,
        "the gap changed; update the list and the count together:\n{}",
        KNOWN_DIVERGENCES.join("\n")
    );
    for divergence in KNOWN_DIVERGENCES {
        assert!(
            divergence.contains("v1") && divergence.contains("v2"),
            "a divergence must say what each side does: {divergence}"
        );
    }
}

#[test]
fn v2_covers_every_pane_v1_had() {
    // The parity question that actually gates deletion: is there a plugin for
    // each v1 surface? The answer is currently NO, deliberately — the interface
    // was cut back to two panes. So this asserts the two halves separately: what
    // is here, and what is knowingly not.
    let host = host();
    let names: Vec<&str> = host.plugins.iter().map(|p| p.name.as_str()).collect();
    for (v1_surface, v2_plugin) in [
        ("session list", "sessions"),
        ("terminal pane", "agent"),
        // A tab of the terminal pane rather than a pane of its own, which is
        // v1's own shape — one centre pane, several views.
        ("shell pane", "agent"),
        // A float rather than a slot occupant, which is v1's shape too: its
        // pickers are modals over whatever you were looking at.
        ("new-session flow", "new_session"),
        // A full-width strip above the bands, as in v1 — not a float, because it
        // highlights matches inside the panes it is searching.
        ("global search", "search"),
    ] {
        assert!(
            names.contains(&v2_plugin),
            "v1's {v1_surface} has no v2 plugin ({v2_plugin} missing from {names:?})"
        );
    }

    // Removed with their plugins, each to be re-added as its own change. Listed
    // so the shortfall is counted rather than implied by an absence, and so a
    // returning pane has one place to be re-registered.
    // Tracked in `openspec/changes/v2-parity-gaps/`.
    const AWAITING_THEIR_PLUGIN: [(&str, &str); 8] = [
        ("info panel", "info"),
        ("file viewer", "files"),
        ("tasks panel", "tasks"),
        ("task detail", "task_detail"),
        ("automations pane", "automations"),
        ("automation detail", "automation_detail"),
        ("code review", "review"),
        ("restore modal", "restore"),
    ];
    for (v1_surface, v2_plugin) in AWAITING_THEIR_PLUGIN {
        assert!(
            !names.contains(&v2_plugin),
            "{v1_surface} is back as {v2_plugin}; move it into the covered list \
             above so parity is asserted rather than merely restored"
        );
    }

    // v1's header and footer are back as kernel-rendered BANDS, not plugins:
    // system chrome is kernel-owned and plugins contribute data to it
    // (`openspec/changes/v2-chrome-bands/`). So they belong in neither list
    // above — present, but never as a plugin, and never a focus stop.
    for (v1_surface, band) in [
        ("header band", thurbox::kernel::bands::Band::Identity),
        ("status row", thurbox::kernel::bands::Band::Message),
        ("status bar", thurbox::kernel::bands::Band::Action),
    ] {
        assert!(
            !names.contains(&band.slot()),
            "v1's {v1_surface} is a band; it must not also be a plugin"
        );
        assert_eq!(
            thurbox::kernel::bands::Band::from_slot(band.slot()),
            Some(band),
            "v1's {v1_surface} has no band"
        );
    }

    // v1's three system panels have no plugin *by design*: they are kernel
    // modals, so they overlay rather than occupy a slot and never join the
    // focus ring. Covered as such in `tests/v2_modals.rs`.
    for (v1_surface, action) in [
        ("theme picker", "themes.open"),
        ("settings panel", "settings.open"),
        ("help overlay", "help.open"),
    ] {
        assert!(
            ModalKind::from_action(action).is_some(),
            "v1's {v1_surface} has neither a plugin nor a modal"
        );
        assert!(
            !names.contains(&action.split('.').next().unwrap_or(action)),
            "v1's {v1_surface} is a modal; it must not also be a plugin"
        );
    }
}
