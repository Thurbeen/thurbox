//! Hovering an affordance lights it.
//!
//! v1 highlights whatever the pointer is over (`App::mouse_hover`); v2 enabled
//! motion reporting and then did nothing with it — paying the whole cost of the
//! firehose for no visible effect.
//!
//! The subject is the **agent pane's tab strip**, which is where the surviving
//! hoverable affordances live: the footer pills these tests used to read went
//! with the footer plugin.
//!
//! The rule these pin down is the one that keeps it honest: the kernel
//! publishes the identity it resolved through the SAME hitboxes it routes
//! clicks through, so what lights up and what a click activates cannot drift
//! apart. A test that only checked "some style changed" would miss that, so
//! these assert the pointer lands on the pill it is actually over.

use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::Terminal;

use thurbox::kernel::host::{LuaHost, Published, RenderContext};
use thurbox::kernel::node::Identity;
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::Snapshot;
use thurbox::kernel::theme::Themes;

fn host() -> LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

/// Paint the agent pane's top border with `hovered` in force, returning each
/// cell's background. Its chips are the affordances a pointer can land on.
fn chip_backgrounds(host: &LuaHost, hovered: Option<&Identity>) -> Vec<(String, Color)> {
    let themes = Themes::load(None);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let snapshot = one_session();

    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        snapshot: &snapshot,
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
        focus: Some("agent"),
        hovered,
    })
    .expect("publish");

    // The session list publishes `store.selected`, which is how the agent pane
    // knows what to show — so it renders first, exactly as the loop paints it.
    let sessions = host
        .plugins
        .iter()
        .position(|p| p.name == "sessions")
        .expect("sessions plugin");
    host.render(
        sessions,
        RenderContext {
            width: 30,
            height: 10,
            focused: true,
            elapsed: 0.0,
            frame: 0,
        },
    )
    .expect("render the list");

    let index = host
        .plugins
        .iter()
        .position(|p| p.name == "agent")
        .expect("agent plugin");
    let node = host
        .render(
            index,
            RenderContext {
                width: WIDTH,
                height: 10,
                focused: false,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render")
        .node;

    let mut terminal = Terminal::new(TestBackend::new(WIDTH, 10)).expect("terminal");
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
    // Row 0 is the border the strip is packed into.
    (0..WIDTH)
        .map(|x| {
            let cell = &buffer[(x, 0)];
            (cell.symbol().to_string(), cell.bg)
        })
        .collect()
}

const WIDTH: u16 = 120;

/// One session, so the pane draws its border and title rather than an empty
/// state.
fn one_session() -> Snapshot {
    Snapshot {
        sessions: vec![thurbox::kernel::snapshot::SessionRow {
            id: "s1-0000-0000-0000-000000000000".into(),
            name: "fix-osc52".into(),
            agent: "claude".into(),
            status: "idle".into(),
            cwd: None,
            repo: Some("thurbox".into()),
            repos: vec!["thurbox".into()],
            branch: Some("fix/osc52".into()),
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
        }],
        ..Snapshot::default()
    }
}

/// The background of the first cell of the chip whose label starts at `label`.
fn background_of(cells: &[(String, Color)], label: &str) -> Color {
    let text: String = cells.iter().map(|(symbol, _)| symbol.as_str()).collect();
    let at = text
        .find(label)
        .unwrap_or_else(|| panic!("no chip labelled {label} in {text:?}"));
    cells[at].1
}

fn hover_on(role: &str) -> Identity {
    Identity {
        id: None,
        classes: vec![],
        role: Some(role.into()),
    }
}

#[test]
fn a_hovered_pill_changes_and_its_neighbours_do_not() {
    let host = host();
    let resting = chip_backgrounds(&host, None);
    let lit = chip_backgrounds(&host, Some(&hover_on("action:terminal.shell")));

    assert_ne!(
        background_of(&resting, "Shell"),
        background_of(&lit, "Shell"),
        "the hovered chip should light up"
    );

    // The half that matters: hovering one chip must not restyle the others, or
    // the highlight is telling the user the wrong thing about where they are.
    assert_eq!(
        background_of(&resting, "Agent"),
        background_of(&lit, "Agent"),
        "a neighbouring chip must be untouched"
    );
}

#[test]
fn hovering_nothing_leaves_every_chip_at_rest() {
    // Guards the nil path: `thurbox.hover` is always published, empty when the
    // pointer is over nothing, and an empty table must not match a chip.
    let host = host();
    let resting = chip_backgrounds(&host, None);
    let empty = chip_backgrounds(&host, Some(&Identity::default()));
    assert_eq!(
        resting.iter().map(|(_, bg)| *bg).collect::<Vec<_>>(),
        empty.iter().map(|(_, bg)| *bg).collect::<Vec<_>>(),
        "an empty hover identity must light nothing"
    );
}

#[test]
fn the_collapse_toggle_lights_its_hint_as_well_as_its_chevron() {
    // One affordance in two colours. They are separate runs because a node has
    // one style, but both carry the same role — so hovering anywhere in ` ◀ F9 `
    // lights the whole label. Half a lit button reads as a smaller hitbox than
    // the one that is actually there.
    let host = host();
    let resting = chip_backgrounds(&host, None);
    let lit = chip_backgrounds(&host, Some(&hover_on("action:sessions.toggle_panel")));

    // By CELL, not by byte: `◀` is three bytes, so `str::find` would land inside
    // the next chip and compare the wrong things.
    let chevron = |cells: &[(String, Color)]| {
        let at = cells
            .iter()
            .position(|(symbol, _)| symbol == "◀")
            .unwrap_or_else(|| panic!("no chevron on the border"));
        // The chevron cell, and the "F" of its hint two cells later.
        assert_eq!(cells[at + 2].0, "F", "the hint follows the chevron");
        (cells[at].1, cells[at + 2].1)
    };

    let (chevron_resting, hint_resting) = chevron(&resting);
    let (chevron_lit, hint_lit) = chevron(&lit);
    assert_ne!(chevron_resting, chevron_lit, "the chevron should light");
    assert_ne!(hint_resting, hint_lit, "its hint should light with it");
    assert_eq!(chevron_lit, hint_lit, "both halves take the same band");
}

#[test]
fn a_hovered_row_is_banded_and_keeps_its_own_colours() {
    // v1's split: a button takes the accent fill and an inverted fg; a list row
    // takes a background band only, so the status dot and the branch keep their
    // colours. Tinting the fg too would flatten the row into a block of one
    // colour.
    let host = host();
    // Two rows: the cursor sits on the first, so the second is the one whose
    // band is visible. Hovering the selected row is a no-op in v1 too — it is
    // already filled with the same colour.
    let mut snapshot = one_session();
    snapshot
        .sessions
        .push(thurbox::kernel::snapshot::SessionRow {
            id: "s2-0000-0000-0000-000000000000".into(),
            name: "add-wsl".into(),
            ..snapshot.sessions[0].clone()
        });
    let id = snapshot.sessions[1].id.clone();

    let cells = |hovered: Option<&Identity>| -> Vec<(String, Color, Color)> {
        let themes = Themes::load(None);
        let mut registry = Registry::default();
        let (bindings, settings) = host.declarations();
        registry.declare(bindings, settings);
        let diffs = thurbox::kernel::diff::DiffStore::new();
        let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
        host.publish(&Published {
            snapshot: &snapshot,
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
            focus: Some("sessions"),
            hovered,
        })
        .expect("publish");
        let index = host
            .plugins
            .iter()
            .position(|p| p.name == "sessions")
            .expect("sessions plugin");
        let node = host
            .render(
                index,
                RenderContext {
                    width: 40,
                    height: 8,
                    // Unfocused, so the row carries no selection bar to hide the
                    // band under.
                    focused: false,
                    elapsed: 0.0,
                    frame: 0,
                },
            )
            .expect("render")
            .node;
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).expect("terminal");
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
        // The row is somewhere below the border and the group header.
        (0..8)
            .flat_map(|y| (0..40).map(move |x| (x, y)))
            .map(|(x, y)| {
                let cell = &buffer[(x, y)];
                (cell.symbol().to_string(), cell.fg, cell.bg)
            })
            .collect()
    };

    let resting = cells(None);
    let lit = cells(Some(&Identity {
        id: Some(id),
        ..Identity::default()
    }));

    // Some cell changed background, and no cell changed foreground.
    let bg_changed = resting
        .iter()
        .zip(&lit)
        .filter(|((_, _, a), (_, _, b))| a != b)
        .count();
    assert!(bg_changed > 0, "hovering a row should band it");
    let fg_changed = resting
        .iter()
        .zip(&lit)
        .filter(|((_, a, _), (_, b, _))| a != b)
        .count();
    assert_eq!(
        fg_changed, 0,
        "a row band must not repaint any foreground — that is the button rule"
    );
}

#[test]
fn a_role_that_matches_no_affordance_lights_nothing() {
    let host = host();
    let resting = chip_backgrounds(&host, None);
    let bogus = chip_backgrounds(&host, Some(&hover_on("action:nothing.here")));
    assert_eq!(
        background_of(&resting, "Shell"),
        background_of(&bogus, "Shell"),
        "an unmatched role must not light a chip"
    );
}
