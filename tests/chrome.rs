//! The chrome around the panes: the banner row, the left column's split, and
//! the central pane's border strip.
//!
//! These are the parts of v1 a user reads without looking at any one pane —
//! and the parts that make v2 recognisable as the same program. Each test
//! names the v1 function it is holding v2 to.

use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use ratatui::style::Color;
use thurbox::kernel::bands::BandState;
use thurbox::kernel::host::{LuaHost, Published, RenderContext};
use thurbox::kernel::layout::{resolve, SlotRect};
use thurbox::kernel::node::ClickVerb;
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{AutomationRow, SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;

fn host() -> LuaHost {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    host
}

fn session(name: &str, branch: &str) -> SessionRow {
    SessionRow {
        id: format!("{name}-0000-0000-0000-000000000000"),
        name: name.into(),
        agent: "claude".into(),
        status: "idle".into(),
        cwd: Some(std::path::PathBuf::from("/src/repo")),
        repo: Some("repo".into()),
        repos: vec!["repo".into()],
        branch: Some(branch.into()),
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
        shell_backend_id: None,
        member_dirs: Vec::new(),
    }
}

fn automation(id: i64, name: &str) -> AutomationRow {
    AutomationRow {
        id,
        name: name.into(),
        schedule: "0 3 * * *".into(),
        action: "send".into(),
        enabled: true,
        last_outcome: None,
        last_detail: None,
        runs: Vec::new(),
    }
}

/// The two-session world the v1/v2 captures were taken against.
fn world(automations: usize) -> Snapshot {
    Snapshot {
        sessions: vec![
            session("fix-osc52", "fix/osc52"),
            session("add-wsl", "feat/wsl"),
        ],
        automations: (0..automations)
            .map(|n| automation(n as i64 + 1, &format!("nightly-{n}")))
            .collect(),
        ..Snapshot::default()
    }
}

fn publish(host: &LuaHost, snapshot: &Snapshot, themes: &Themes) {
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
        snapshot,
        attach_errors: &Default::default(),
        inflight: &[],
        themes,
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
}

fn index_of(host: &LuaHost, plugin: &str) -> usize {
    host.plugins
        .iter()
        .position(|p| p.name == plugin)
        .unwrap_or_else(|| panic!("no plugin {plugin}"))
}

/// A pane rendered but not painted, purely so it publishes into `store` — the
/// bus a neighbour reads to learn which session or automation is selected.
/// Given a column of its own, because a pane too small to draw returns early
/// before it publishes anything.
fn prime(host: &LuaHost, plugin: &str) {
    host.render(
        index_of(host, plugin),
        RenderContext {
            width: 40,
            height: 20,
            focused: false,
            elapsed: 0.0,
            frame: 0,
        },
    )
    .expect("render");
}

/// Paint `plugin` into a `width` x `height` buffer, after priming `first`.
fn screen(
    host: &LuaHost,
    snapshot: &Snapshot,
    first: &[&str],
    plugin: &str,
    width: u16,
    height: u16,
) -> String {
    publish(host, snapshot, &Themes::load(None));
    for name in first {
        prime(host, name);
    }

    let node = host
        .render(
            index_of(host, plugin),
            RenderContext {
                width,
                height,
                focused: false,
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

fn slots(host: &LuaHost, snapshot: &Snapshot, width: u16, height: u16) -> Vec<SlotRect> {
    publish(host, snapshot, &Themes::load(None));
    let area = Rect {
        x: 0,
        y: 0,
        width,
        height,
    };
    resolve(&host.arrangement(width, height).expect("arrangement"), area)
}

fn rect_of(placed: &[SlotRect], slot: &str) -> Option<Rect> {
    placed.iter().find(|s| s.slot == slot).map(|s| s.rect)
}

// ── Chrome bands (kernel-rendered, arrangement-placed) ─────────────────────

/// Place a band and paint it, returning the row it drew.
fn band_row(band: thurbox::kernel::bands::Band, state: &BandState<'_>, width: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, 1)).expect("terminal");
    terminal
        .draw(|frame| {
            thurbox::kernel::bands::render(frame, frame.area(), band, state);
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    (0..width)
        .map(|x| buffer[(x, 0)].symbol().to_string())
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn band_state<'a>(
    registry: &'a Registry,
    themes: &'a Themes,
    message: Option<(&'a str, thurbox::kernel::bands::Level)>,
) -> BandState<'a> {
    BandState {
        version: "9.9.9",
        theme_label: "Default",
        update_available: None,
        session: Some("fix-osc52"),
        session_count: 3,
        automation_count: 0,
        focus_label: "agent",
        message,
        progress: None,
        hovered: None,
        registry,
        themes,
    }
}

#[test]
fn the_identity_band_names_the_product_its_version_and_the_theme() {
    let themes = Themes::load(None);
    let registry = Registry::default();
    let state = band_state(&registry, &themes, None);
    let row = band_row(thurbox::kernel::bands::Band::Identity, &state, 110);

    assert!(row.contains("thurbox"), "{row}");
    assert!(row.contains("v9.9.9"), "the running version: {row}");
    assert!(row.contains("Default"), "the active theme: {row}");
    assert!(row.contains("fix-osc52"), "the selected session: {row}");
    assert!(
        !row.contains("available"),
        "no update notice when none is known: {row}"
    );
}

#[test]
fn the_identity_band_announces_a_newer_version_when_one_is_known() {
    let themes = Themes::load(None);
    let registry = Registry::default();
    let mut state = band_state(&registry, &themes, None);
    state.update_available = Some("10.0.0");
    let row = band_row(thurbox::kernel::bands::Band::Identity, &state, 110);

    assert!(row.contains("v9.9.9"), "still names what is running: {row}");
    assert!(row.contains("v10.0.0 available"), "{row}");
}

#[test]
fn the_message_band_badges_each_severity_differently() {
    use thurbox::kernel::bands::Level;
    let themes = Themes::load(None);
    let registry = Registry::default();

    for (level, badge) in [
        (Level::Info, "INFO"),
        (Level::Success, "SYNC"),
        (Level::Error, "ERROR"),
    ] {
        let state = band_state(&registry, &themes, Some(("something happened", level)));
        let row = band_row(thurbox::kernel::bands::Band::Message, &state, 60);
        assert!(row.contains(badge), "{level:?} should badge {badge}: {row}");
        assert!(row.contains("something happened"), "{row}");
    }
}

#[test]
fn the_message_band_draws_nothing_when_there_is_nothing_to_say() {
    let themes = Themes::load(None);
    let registry = Registry::default();
    let state = band_state(&registry, &themes, None);
    assert_eq!(
        band_row(thurbox::kernel::bands::Band::Message, &state, 60),
        "",
        "an empty message band must not paint"
    );
}

#[test]
fn progress_outranks_a_message_because_it_outlives_one() {
    use thurbox::kernel::bands::Level;
    let themes = Themes::load(None);
    let registry = Registry::default();
    let mut state = band_state(&registry, &themes, Some(("done", Level::Info)));
    state.progress = Some("creating thurbox");
    let row = band_row(thurbox::kernel::bands::Band::Message, &state, 60);

    assert!(row.contains("creating thurbox"), "{row}");
    assert!(
        !row.contains("done"),
        "the toast yields to live work: {row}"
    );
}

#[test]
fn the_action_band_carries_the_focus_the_counts_and_the_entries() {
    let themes = Themes::load(None);
    let host = host();
    // Declared exactly as the loop declares them: every plugin's contributions
    // plus the kernel's own, so the band sees what it sees at runtime.
    let mut registry = Registry::default();
    let (mut bindings, settings, mut pills) = host.all_declarations();
    bindings.extend(thurbox::kernel::modals::bindings());
    pills.extend(thurbox::kernel::modals::pills());
    registry.declare_all(bindings, settings, pills);

    let state = band_state(&registry, &themes, None);
    let row = band_row(thurbox::kernel::bands::Band::Action, &state, 110);

    // v1's left cluster, in its four parts: the focused surface as a badge, the
    // counts in v1's own `session(s)` spelling, then the informational chords.
    assert!(row.contains(" Agent "), "the focus badge: {row}");
    assert!(row.contains("3 session(s)"), "v1's count spelling: {row}");
    assert!(row.contains("^H/^L Focus"), "the focus hint: {row}");
    assert!(row.contains("^O Open"), "the open hint: {row}");
    // The kernel's own modals contribute these three; no bundled PANE declares an
    // entry, and that is deliberate — see `ui/plugins/20_agent.lua`. The
    // plugin-contributed path is covered in `tests/kernel_mvp.rs` with a
    // throwaway plugin, so this asserting nothing about a pane's entry is not a
    // gap in coverage.
    assert!(row.contains("Help · F1"), "{row}");
    assert!(row.contains("Theme · F4"), "{row}");
    assert!(row.contains("Settings · F6"), "{row}");
    // v1's rightmost pill, and the only entry the registry does not resolve:
    // quit is reserved rather than declared, so the band carries it itself.
    assert!(row.contains("Quit · ^Q"), "{row}");
    assert!(
        !row.contains("Shell"),
        "the shell is offered by the tab strip, not twice: {row}"
    );
}

/// Paint the action band and return both the row and the hitboxes it recorded.
fn action_band(state: &BandState<'_>, width: u16) -> (String, Vec<thurbox::kernel::bands::Hit>) {
    let mut hits = Vec::new();
    let mut terminal = Terminal::new(TestBackend::new(width, 1)).expect("terminal");
    terminal
        .draw(|frame| {
            hits = thurbox::kernel::bands::render(
                frame,
                frame.area(),
                thurbox::kernel::bands::Band::Action,
                state,
            );
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let row = (0..width)
        .map(|x| buffer[(x, 0)].symbol().to_string())
        .collect::<String>();
    (row, hits)
}

fn action_registry(host: &LuaHost) -> Registry {
    let mut registry = Registry::default();
    let (mut bindings, settings, mut pills) = host.all_declarations();
    bindings.extend(thurbox::kernel::modals::bindings());
    pills.extend(thurbox::kernel::modals::pills());
    registry.declare_all(bindings, settings, pills);
    registry
}

#[test]
fn every_entry_is_a_button_with_a_hitbox_over_its_own_label() {
    // Without this an entry is a picture of a button: drawn, and unpressable.
    // Bands are painted by the kernel rather than walked as a tree, so they must
    // hand their hitboxes back themselves.
    let themes = Themes::load(None);
    let host = host();
    let registry = action_registry(&host);
    let state = band_state(&registry, &themes, None);
    let (row, hits) = action_band(&state, 110);

    assert!(!hits.is_empty(), "the entries recorded no hitbox");
    for hit in &hits {
        // The verb a press performs, and it names a real action.
        let verb = hit.identity.click_verb().expect("an entry carries a verb");
        let action = match verb {
            ClickVerb::Action(action) => action,
            other => panic!("an entry must be an action, got {other:?}"),
        };
        // Quit is the one exception, and by design: it is reserved rather than
        // declared, so there is no binding for it to be found under.
        assert!(
            action == thurbox::kernel::bands::QUIT_ACTION
                || registry.bindings().iter().any(|b| b.action == action),
            "{action} is not declared anywhere"
        );
        // The hitbox covers the label the user is aiming at.
        let painted: String = row
            .chars()
            .skip(usize::from(hit.rect.x))
            .take(usize::from(hit.rect.width))
            .collect();
        assert!(
            painted.trim().contains('·') || !painted.trim().is_empty(),
            "hitbox {:?} covers {painted:?}",
            hit.rect
        );
    }

    // Hitboxes do not overlap, so a press resolves to exactly one entry.
    let mut sorted: Vec<_> = hits.iter().map(|h| h.rect).collect();
    sorted.sort_by_key(|rect| rect.x);
    for pair in sorted.windows(2) {
        assert!(
            pair[0].x + pair[0].width <= pair[1].x,
            "entries overlap: {:?} then {:?}",
            pair[0],
            pair[1]
        );
    }
}

#[test]
fn the_left_cluster_gives_up_its_parts_tail_first() {
    // v1 `fit_spans_to_budget`: trailing spans are dropped whole so the focus
    // badge is the last thing to lose its space, and only a badge that still
    // overflows is truncated. At 100 columns v1's own recording keeps just
    // ` Sessions ` beside the pills, for exactly this reason.
    let themes = Themes::load(None);
    let host = host();
    let registry = action_registry(&host);
    let state = band_state(&registry, &themes, None);

    let wide = band_row(thurbox::kernel::bands::Band::Action, &state, 160);
    assert!(wide.contains("^O Open"), "everything fits at 160: {wide}");

    // Narrow enough that the hints cannot survive beside the entries.
    let narrow = band_row(thurbox::kernel::bands::Band::Action, &state, 60);
    assert!(
        narrow.contains("Agent"),
        "the badge outlives the rest: {narrow}"
    );
    assert!(!narrow.contains("Focus"), "the hints go first: {narrow}");

    // An automation count only appears when there is one, and it is not a
    // pluralised word — v1 writes `automation(s)`.
    assert!(!wide.contains("automation"), "none configured: {wide}");
    let mut busy = band_state(&registry, &themes, None);
    busy.automation_count = 2;
    let with_autos = band_row(thurbox::kernel::bands::Band::Action, &busy, 160);
    assert!(with_autos.contains("2 automation(s)"), "{with_autos}");
}

#[test]
fn quit_outlives_every_other_entry_when_the_band_narrows() {
    // v1 sheds Theme → Settings → Help and keeps `Quit` at any width: the button
    // that gets you out must not be the one that disappears.
    let themes = Themes::load(None);
    let host = host();
    let registry = action_registry(&host);
    let state = band_state(&registry, &themes, None);

    let wide = band_row(thurbox::kernel::bands::Band::Action, &state, 160);
    assert!(wide.contains("Help · F1"), "{wide}");
    assert!(wide.contains("Quit · ^Q"), "{wide}");

    let narrow = band_row(thurbox::kernel::bands::Band::Action, &state, 20);
    assert!(narrow.contains("Quit · ^Q"), "quit survives: {narrow}");
    assert!(
        !narrow.contains("Help"),
        "the declared entries shed around it: {narrow}"
    );
}

#[test]
fn an_entry_is_formatted_the_way_v1_formats_a_footer_pill() {
    // v1 `ui::render_button_bar` + `button_style(primary = false)`: the chip is
    // ` label · key `, filled with the selection pair, bold, two cells of padding,
    // one cell between chips, and the last one flush with the right edge.
    use ratatui::style::Modifier;
    let themes = Themes::load(None);
    let host = host();
    let registry = action_registry(&host);
    let state = band_state(&registry, &themes, None);

    let width = 110u16;
    let mut hits = Vec::new();
    let mut terminal = Terminal::new(TestBackend::new(width, 1)).expect("terminal");
    terminal
        .draw(|frame| {
            hits = thurbox::kernel::bands::render(
                frame,
                frame.area(),
                thurbox::kernel::bands::Band::Action,
                &state,
            );
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();

    let last = hits.last().expect("an entry");
    assert_eq!(
        last.rect.x + last.rect.width,
        width,
        "the block is flush with the right edge"
    );

    for hit in &hits {
        let cell = &buffer[(hit.rect.x, 0)];
        assert!(
            cell.modifier.contains(Modifier::BOLD),
            "a chip is bold, as v1's is"
        );
        // Two cells of padding: the chip starts and ends on a space.
        let text: String = (hit.rect.x..hit.rect.x + hit.rect.width)
            .map(|x| buffer[(x, 0)].symbol().to_string())
            .collect();
        assert!(
            text.starts_with(' ') && text.ends_with(' '),
            "a chip is padded on both sides: {text:?}"
        );
        assert!(
            text.contains(" · "),
            "label and chord are joined by ' · ': {text:?}"
        );
    }

    // One cell between neighbours, and that cell is not part of either chip.
    let mut rects: Vec<_> = hits.iter().map(|h| h.rect).collect();
    rects.sort_by_key(|rect| rect.x);
    for pair in rects.windows(2) {
        assert_eq!(
            pair[1].x,
            pair[0].x + pair[0].width + 1,
            "exactly one cell between chips"
        );
    }
}

#[test]
fn a_hovered_entry_lights_and_its_neighbours_do_not() {
    // v1's split: a BUTTON brightens its fill to the accent and forces its
    // foreground, where a list row takes only a background band.
    let themes = Themes::load(None);
    let host = host();
    let registry = action_registry(&host);

    let resting = band_state(&registry, &themes, None);
    let (_, hits) = action_band(&resting, 110);
    let first = hits.first().expect("an entry").identity.clone();

    let mut lit_state = band_state(&registry, &themes, None);
    lit_state.hovered = Some(&first);

    let cells = |state: &BandState<'_>| -> Vec<(Color, Color)> {
        let mut terminal = Terminal::new(TestBackend::new(110, 1)).expect("terminal");
        terminal
            .draw(|frame| {
                thurbox::kernel::bands::render(
                    frame,
                    frame.area(),
                    thurbox::kernel::bands::Band::Action,
                    state,
                );
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        (0..110)
            .map(|x| (buffer[(x, 0)].fg, buffer[(x, 0)].bg))
            .collect()
    };

    let before = cells(&resting);
    let after = cells(&lit_state);
    let target = hits[0].rect;
    let inside = usize::from(target.x);
    assert_ne!(
        before[inside], after[inside],
        "the hovered entry should light"
    );

    // A neighbour is untouched, or the highlight is lying about where you are.
    if let Some(neighbour) = hits.get(1) {
        let outside = usize::from(neighbour.rect.x);
        assert_eq!(
            before[outside], after[outside],
            "a neighbouring entry must be untouched"
        );
    }
}

#[test]
fn a_band_is_placed_by_the_arrangement_and_omitting_it_is_not_an_error() {
    // The whole point of bands being slots: `ui/layout.lua` decides.
    let host = host();
    let placed = slots(&host, &world(0), 160, 40);
    let names: Vec<&str> = placed.iter().map(|s| s.slot.as_str()).collect();
    assert!(names.contains(&"header"), "{names:?}");
    assert!(names.contains(&"footer"), "{names:?}");
    // Nothing to say, so the message band takes no row at all.
    assert!(!names.contains(&"status"), "{names:?}");

    // Under 20 rows the identity band is surrendered first, as v1 does.
    let short = slots(&host, &world(0), 160, 12);
    let short_names: Vec<&str> = short.iter().map(|s| s.slot.as_str()).collect();
    assert!(!short_names.contains(&"header"), "{short_names:?}");
    assert!(
        short_names.contains(&"center"),
        "the panes survive: {short_names:?}"
    );
}

// ── The arrangement (ui/layout.lua) ────────────────────────────────────────

/// The placed slots that are panes, not chrome bands.
fn pane_slots(placed: &[SlotRect]) -> Vec<&str> {
    placed
        .iter()
        .map(|s| s.slot.as_str())
        .filter(|slot| thurbox::kernel::bands::Band::from_slot(slot).is_none())
        .collect()
}

#[test]
fn the_two_panes_split_the_width_between_the_bands() {
    // The interface is the session column plus the centre. v1's info/tasks/files
    // columns went with their plugins, and so did their slots — a slot no plugin
    // can fill would reserve a rect for nothing.
    let host = host();
    let placed = slots(&host, &world(0), 160, 40);
    assert_eq!(pane_slots(&placed), ["sessions", "center"]);

    let sessions = rect_of(&placed, "sessions").expect("a session column");
    let centre = rect_of(&placed, "center").expect("a centre pane");
    // A row for the identity band above and the action band below; the message
    // band is quiet, so it takes none.
    assert_eq!(sessions.y, 1, "below the identity band");
    assert_eq!(sessions.height, 38, "between the two placed bands");
    assert_eq!(centre.height, sessions.height);
    // v1's two-panel split is 25/75, and the centre takes the remainder.
    assert_eq!(sessions.width, 40, "25% of 160");
    assert_eq!(centre.x, sessions.width);
    assert_eq!(sessions.width + centre.width, 160);
}

#[test]
fn below_the_two_panel_threshold_only_the_centre_is_placed() {
    // v1 `compute_layout`: under 80 columns there is room for the agent alone.
    let host = host();
    let placed = slots(&host, &world(0), 79, 24);
    assert_eq!(pane_slots(&placed), ["center"]);
    assert_eq!(rect_of(&placed, "center").expect("centre").width, 79);
}

#[test]
fn hiding_the_session_list_gives_the_centre_the_whole_width() {
    // v1's F9. The centre is never dropped, so it absorbs the freed columns.
    let host = host();
    let placed = slots(&host, &world(0), 160, 40);
    assert_eq!(rect_of(&placed, "center").expect("centre").width, 120);

    host.on_action(
        host.index_of("sessions").expect("sessions"),
        "sessions.toggle_panel",
    )
    .expect("toggle");

    let placed = slots(&host, &world(0), 160, 40);
    assert_eq!(pane_slots(&placed), ["center"]);
    assert_eq!(rect_of(&placed, "center").expect("centre").width, 160);
}

// ── Focus levels (v1 `ui::FocusLevel`) ─────────────────────────────────────

#[test]
fn an_unfocused_pane_showing_the_current_session_stays_accented() {
    // v1 has three levels and both of these panes fall through to `Active` when
    // they do not hold focus: they still show the current session, so they keep
    // the plain accent border. Dropping them to the gray `Inactive` border made
    // the whole interface read as switched off the moment focus moved.
    let host = host();
    for pane in ["sessions", "agent"] {
        let focused = border_colour(&host, pane, true);
        let unfocused = border_colour(&host, pane, false);
        assert_ne!(
            focused, unfocused,
            "{pane}: focus must still be visible (bright accent vs accent)"
        );
    }

    // The property that was broken: the list dropped to the gray `Inactive`
    // border while the agent pane stayed `Active`, so the two disagreed about
    // what "not focused" looks like. Comparing them to each other needs no
    // colour plumbing and says the invariant directly.
    assert_eq!(
        border_colour(&host, "sessions", false),
        border_colour(&host, "agent", false),
        "both panes show the current session, so both are `Active` unfocused"
    );
    // And the accent they share is not the theme's unfocused gray.
    let accent = thurbox::kernel::theme::color_to_string(&border_colour(&host, "agent", false));
    let gray = Themes::load(None)
        .roles()
        .get("border_unfocused")
        .cloned()
        .expect("the theme defines border_unfocused");
    assert_ne!(
        accent.to_lowercase(),
        gray.to_lowercase(),
        "an unfocused pane showing the current session keeps the ACCENT border, \
         not the gray one"
    );
}

/// The colour of a pane's top-left border corner.
fn border_colour(host: &LuaHost, pane: &str, focused: bool) -> ratatui::style::Color {
    let themes = Themes::load(None);
    publish(host, &world(0), &themes);
    // The session list publishes the selection the agent pane reads.
    let sessions = host.index_of("sessions").expect("sessions");
    host.render(
        sessions,
        RenderContext {
            width: 40,
            height: 10,
            focused: pane == "sessions" && focused,
            elapsed: 0.0,
            frame: 0,
        },
    )
    .expect("render the list");

    let index = host.index_of(pane).expect("pane");
    let node = host
        .render(
            index,
            RenderContext {
                width: 60,
                height: 10,
                focused,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render")
        .node;
    let mut terminal = Terminal::new(TestBackend::new(60, 10)).expect("terminal");
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
    terminal.backend().buffer()[(0, 0)].fg
}

// ── The central pane's border strip (v1 `App::render_central_pane`) ─────────

/// The agent pane's top border row.
fn agent_border(host: &LuaHost, width: u16) -> String {
    screen(host, &world(0), &["sessions"], "agent", width, 10)
        .lines()
        .next()
        .expect("a border row")
        .to_string()
}

#[test]
fn the_tab_strip_packs_v1s_chevron_and_tabs_into_the_top_border() {
    let host = host();
    let border = agent_border(&host, 113);
    assert!(
        border.starts_with("╭ ◀ F9 ─ Agent ─ Shell · F8 ─"),
        "the chevron packs first, then the tabs, one border cell between chips:\n{border}"
    );
    // The session title keeps the right of the same row.
    assert!(
        border.ends_with("fix-osc52 (claude) [fix/osc52] [Idle] ╮"),
        "the right-aligned title survives beside the strip:\n{border}"
    );
}

#[test]
fn the_strip_never_overruns_the_title_or_the_corners() {
    let host = host();
    for width in [20u16, 40, 60, 113, 200] {
        let border = agent_border(&host, width);
        assert_eq!(
            border.chars().count(),
            width as usize,
            "the border row is exactly the pane wide at {width}"
        );
        assert!(border.starts_with('╭') && border.ends_with('╮'), "{border}");
    }
}

#[test]
fn a_narrow_pane_sheds_the_shortcuts_then_the_lowest_priority_tabs() {
    // v1 `trim_central_tabs`: strip the `· key` suffixes first, then drop the
    // lowest-priority tab — never Agent.
    let host = host();
    let wide = agent_border(&host, 113);
    assert!(wide.contains("Shell · F8"), "{wide}");

    // Narrow enough that the suffix cannot fit but both names can.
    let medium = agent_border(&host, 24);
    assert!(!medium.contains("· F8"), "shortcuts shed first:\n{medium}");
    for name in ["Agent", "Shell"] {
        assert!(medium.contains(name), "{name} still fits:\n{medium}");
    }

    let narrow = agent_border(&host, 18);
    assert!(!narrow.contains("Shell"), "Shell sheds next:\n{narrow}");
    assert!(narrow.contains("Agent"), "Agent always survives:\n{narrow}");
}

#[test]
fn a_pane_too_narrow_for_the_hint_keeps_a_bare_chevron() {
    // v1 `COLLAPSE_HINT_MIN_WIDTH`: under 40 columns the ` F9 ` hint goes and
    // the chevron alone stays, because it is how the list comes back.
    let host = host();
    let narrow = agent_border(&host, 30);
    assert!(narrow.starts_with("╭ ◀ "), "{narrow}");
    assert!(!narrow.contains("F9"), "the hint is dropped:\n{narrow}");
}

#[test]
fn the_welcome_screen_draws_no_strip() {
    // v1 returns no chevron and no tabs with no session — there is no pane
    // boundary to collapse and no view to switch to.
    let host = host();
    let empty = Snapshot::default();
    let border = screen(&host, &empty, &["sessions"], "agent", 80, 10)
        .lines()
        .next()
        .expect("a border row")
        .to_string();
    assert!(border.starts_with("┌ No Session "), "{border}");
    assert!(!border.contains("Agent"), "{border}");
}

/// The full cells a band paints, not just its symbols: the loop's settle check
/// compares cells, so a band that repainted the same text in a different style
/// would still keep it awake.
fn band_cells(
    band: thurbox::kernel::bands::Band,
    state: &BandState<'_>,
    width: u16,
) -> Vec<ratatui::buffer::Cell> {
    let mut terminal = Terminal::new(TestBackend::new(width, 1)).expect("terminal");
    terminal
        .draw(|frame| {
            thurbox::kernel::bands::render(frame, frame.area(), band, state);
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    (0..width).map(|x| buffer[(x, 0)].clone()).collect()
}

#[test]
fn a_band_repaints_identically_from_identical_state() {
    // The premise the demand-driven redraw rests on. `render_band` decides a
    // frame changed by comparing the cells a band just painted against the ones
    // it painted last frame, so a band carrying anything time-varying — a
    // clock, an uptime, an animation — would report a change on every single
    // frame and the loop would never settle to the 250ms floor.
    //
    // It did exactly that once, for a much blunter reason: the band marked the
    // frame changed for having been *drawn* at all, so an idle screen with no
    // sessions repainted at the frame cap forever. This is the guard for the
    // property that made fixing it possible.
    let themes = Themes::load(None);
    let registry = Registry::default();
    let state = band_state(&registry, &themes, None);

    for band in [
        thurbox::kernel::bands::Band::Identity,
        thurbox::kernel::bands::Band::Action,
    ] {
        let first = band_cells(band, &state, 110);
        let again = band_cells(band, &state, 110);
        assert!(
            first == again,
            "{band:?} painted differently from identical state — the loop cannot settle"
        );
    }
}

#[test]
fn a_band_repaints_differently_when_what_it_says_changes() {
    // The other half: if the comparison could not see a real change, a message
    // appearing would never be painted. Cells rather than symbols, so a change
    // of level (colour) counts too.
    let themes = Themes::load(None);
    let registry = Registry::default();

    let quiet = band_state(&registry, &themes, None);
    let noisy = band_state(
        &registry,
        &themes,
        Some(("worktree created", thurbox::kernel::bands::Level::Info)),
    );

    let before = band_cells(thurbox::kernel::bands::Band::Message, &quiet, 110);
    let after = band_cells(thurbox::kernel::bands::Band::Message, &noisy, 110);
    assert!(
        before != after,
        "the message band painted the same cells with and without a message"
    );
}
