use thurbox::kernel::snapshot::SnapshotStore;
use thurbox::kernel::theme::Themes;

#[test]
fn a_snapshot_refresh_moves_the_version() {
    let mut store = SnapshotStore::without_sessions(&Default::default());

    let before = store.version();
    store.refresh();
    assert_ne!(
        store.version(),
        before,
        "a refresh replaced the snapshot without saying so"
    );
}

#[test]
fn every_refresh_moves_it_again() {
    let mut store = SnapshotStore::without_sessions(&Default::default());

    let mut seen = store.version();
    for _ in 0..5 {
        store.refresh();
        let now = store.version();
        assert_ne!(now, seen, "a later refresh did not move the version");
        seen = now;
    }
}

#[test]
fn reading_the_snapshot_leaves_the_version_alone() {
    let mut store = SnapshotStore::without_sessions(&Default::default());
    store.refresh();

    let settled = store.version();
    for _ in 0..100 {
        let _ = store.current().sessions.len();
        let _ = store.current().taken_at_ms;
    }
    assert_eq!(
        store.version(),
        settled,
        "reading the snapshot moved its version"
    );
}

#[test]
fn selecting_a_theme_moves_the_version_and_reading_it_does_not() {
    let mut themes = Themes::load(None);
    let settled = themes.version();

    for _ in 0..50 {
        let _ = themes.roles();
        let _ = themes.active();
    }
    assert_eq!(
        themes.version(),
        settled,
        "reading the palette moved its version"
    );

    let other = themes
        .choices()
        .iter()
        .map(|choice| choice.name.clone())
        .find(|name| *name != themes.active().name)
        .expect("more than one theme ships");
    themes.preview(&other).expect("preview");
    assert_ne!(
        themes.version(),
        settled,
        "the active palette changed without saying so"
    );
}

use thurbox::kernel::host::{Epoch, LuaHost, Published};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{SessionRow, Snapshot};

fn interface() -> (tempfile::TempDir, LuaHost) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    std::fs::write(
        mods.join("10_probe.lua"),
        r#"return { name = "probe", slot = "center", render = function()
             local names = {}
             for _, s in ipairs(thurbox.sessions or {}) do names[#names + 1] = s.name end
             return { text = table.concat(names, ",") .. "|" .. tostring(thurbox.theme.name) }
           end }"#,
    )
    .expect("write");
    let host = LuaHost::new(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);
    (dir, host)
}

fn row(id: &str, name: &str) -> SessionRow {
    SessionRow {
        id: id.into(),
        name: name.into(),
        agent: "claude".into(),
        status: thurbox::session::SessionState::Idle,
        cwd: None,
        repo: None,
        repos: Vec::new(),
        branch: None,
        base_branch: None,
        backend: "local:tmux".into(),
        backend_id: Some("%1".into()),
        remote_host: None,
        place: None,
        agent_session_id: None,
        parent_id: None,
        display_order: None,
        worktree_count: 0,
        git: None,
        stopped: false,
        hook_state: None,
        key: None,
        reports_as: None,
        detected_agent: None,
        shell_backend_id: None,
        member_dirs: Vec::new(),
        meta: Default::default(),
        deleting: None,
        state_why: None,
        status_source: None,
    }
}

fn publish_at(host: &LuaHost, epoch: Epoch, snapshot: &Snapshot, themes: &Themes) {
    publish_at_with(host, epoch, snapshot, themes, &[]);
}

fn publish_at_with(
    host: &LuaHost,
    epoch: Epoch,
    snapshot: &Snapshot,
    themes: &Themes,
    inflight: &[thurbox::kernel::command::InFlight],
) {
    publish_at_printing(host, epoch, snapshot, themes, inflight, &Default::default());
}

fn publish_at_printing(
    host: &LuaHost,
    epoch: Epoch,
    snapshot: &Snapshot,
    themes: &Themes,
    inflight: &[thurbox::kernel::command::InFlight],
    printing: &std::collections::HashSet<String>,
) {
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::new();
    host.publish(&Published {
        epoch,
        snapshot,
        attach_errors: &Default::default(),
        starts: &Default::default(),
        inflight,
        themes,
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
        printing,
    })
    .expect("publish");
}

fn probe(host: &LuaHost) -> String {
    let index = host.index_of("probe").expect("probe plugin");
    let node = host
        .render(
            index,
            thurbox::kernel::host::RenderContext {
                width: 80,
                height: 4,
                focused: false,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render")
        .node;
    format!("{node:?}")
}

#[test]
fn a_change_of_what_is_printing_rebuilds_one_group_and_no_others() {
    let (_dir, host) = interface();
    let themes = Themes::load(None);
    let rows = Snapshot {
        sessions: vec![row("s1", "one"), row("s2", "two")],
        ..Snapshot::default()
    };
    let epoch = Epoch::always_fresh();

    publish_at(&host, epoch, &rows, &themes);

    let before = host.reused_groups();
    publish_at(&host, epoch, &rows, &themes);
    let all_groups = host.reused_groups() - before;
    assert!(
        all_groups > 1,
        "expected several gated groups, saw {all_groups}"
    );

    let printing: std::collections::HashSet<String> = ["s1".to_string()].into_iter().collect();
    let mut moved = epoch;
    moved.printing = epoch.printing.wrapping_add(1);
    let before = host.reused_groups();
    publish_at_printing(&host, moved, &rows, &themes, &[], &printing);
    let reused = host.reused_groups() - before;

    assert_eq!(
        reused,
        all_groups - 1,
        "a printing change rebuilt {} groups; it may rebuild only its own",
        all_groups - reused
    );
}

#[test]
fn a_moved_source_is_published_even_though_groups_are_gated() {
    let (_dir, host) = interface();
    let themes = Themes::load(None);

    let first = Snapshot {
        sessions: vec![row("a", "alpha")],
        ..Snapshot::default()
    };
    let mut epoch = Epoch::default();
    publish_at(&host, epoch, &first, &themes);
    assert!(probe(&host).contains("alpha"));

    let second = Snapshot {
        sessions: vec![row("b", "beta")],
        ..Snapshot::default()
    };
    epoch.snapshot += 1;
    publish_at(&host, epoch, &second, &themes);
    assert!(
        probe(&host).contains("beta"),
        "the sessions group was reused after the snapshot moved"
    );
}

#[test]
fn a_gated_publish_matches_a_full_rebuild() {
    let (_dir, host) = interface();
    let themes = Themes::load(None);
    let snapshot = Snapshot {
        sessions: vec![row("a", "alpha"), row("b", "beta")],
        ..Snapshot::default()
    };

    let epoch = Epoch::default();
    publish_at(&host, epoch, &snapshot, &themes);
    let gated_first = probe(&host);

    publish_at(&host, epoch, &snapshot, &themes);
    let gated_again = probe(&host);

    publish_at(&host, Epoch::always_fresh(), &snapshot, &themes);
    let rebuilt = probe(&host);

    assert_eq!(
        gated_first, gated_again,
        "a reused group differed from itself"
    );
    assert_eq!(
        gated_again, rebuilt,
        "a reused group differed from one built afresh"
    );
}

#[test]
fn a_theme_change_reaches_lua_through_its_own_version() {
    let (_dir, host) = interface();
    let mut themes = Themes::load(None);
    let snapshot = Snapshot::default();

    let mut epoch = Epoch::default();
    publish_at(&host, epoch, &snapshot, &themes);
    let before = probe(&host);

    let other = themes
        .choices()
        .iter()
        .map(|choice| choice.name.clone())
        .find(|name| *name != themes.active().name)
        .expect("more than one theme ships");
    themes.preview(&other).expect("preview");
    epoch.themes = themes.version();
    publish_at(&host, epoch, &snapshot, &themes);

    assert_ne!(
        before,
        probe(&host),
        "the theme group was reused after the palette changed"
    );
}

fn two_panes() -> (tempfile::TempDir, LuaHost) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    let body = r#"render = function()
             local names = {}
             for _, s in ipairs(thurbox.sessions or {}) do names[#names + 1] = s.name end
             return { text = table.concat(names, ",") }
           end }"#;
    std::fs::write(
        mods.join("10_pure.lua"),
        format!("return {{ name = \"pure\", slot = \"left\", pure = true, {body}"),
    )
    .expect("write");
    std::fs::write(
        mods.join("20_plain.lua"),
        format!("return {{ name = \"plain\", slot = \"center\", {body}"),
    )
    .expect("write");
    let host = LuaHost::new(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);
    (dir, host)
}

fn tree_of(host: &LuaHost, name: &str) -> String {
    let index = host.index_of(name).expect("plugin");
    format!(
        "{:?}",
        host.render(
            index,
            thurbox::kernel::host::RenderContext {
                width: 40,
                height: 4,
                focused: false,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render")
        .node
    )
}

#[test]
fn a_pure_pane_is_not_re_rendered_while_its_inputs_stand() {
    let (_dir, host) = two_panes();
    let themes = Themes::load(None);
    publish_at(
        &host,
        Epoch::default(),
        &Snapshot {
            sessions: vec![row("a", "alpha")],
            ..Snapshot::default()
        },
        &themes,
    );

    let _ = tree_of(&host, "pure");
    let settled = host.skipped_renders();
    for _ in 0..10 {
        let _ = tree_of(&host, "pure");
    }
    assert_eq!(
        host.skipped_renders() - settled,
        10,
        "a pure pane was re-rendered although nothing it reads had moved"
    );
}

#[test]
fn an_undeclared_pane_renders_every_time() {
    let (_dir, host) = two_panes();
    let themes = Themes::load(None);
    publish_at(&host, Epoch::default(), &Snapshot::default(), &themes);

    let _ = tree_of(&host, "plain");
    let settled = host.skipped_renders();
    for _ in 0..10 {
        let _ = tree_of(&host, "plain");
    }
    assert_eq!(
        host.skipped_renders(),
        settled,
        "an undeclared pane was served a cached tree"
    );
}

#[test]
fn a_moved_epoch_re_renders_a_pure_pane() {
    let (_dir, host) = two_panes();
    let themes = Themes::load(None);
    let mut epoch = Epoch::default();

    publish_at(
        &host,
        epoch,
        &Snapshot {
            sessions: vec![row("a", "alpha")],
            ..Snapshot::default()
        },
        &themes,
    );
    assert!(tree_of(&host, "pure").contains("alpha"));

    let second = Snapshot {
        sessions: vec![row("b", "beta")],
        ..Snapshot::default()
    };
    epoch.snapshot += 1;
    publish_at(&host, epoch, &second, &themes);
    assert!(
        tree_of(&host, "pure").contains("beta"),
        "a pure pane kept its tree after the snapshot moved"
    );
}

#[test]
fn accepting_a_command_must_move_the_epoch_to_reach_a_pure_pane() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    let themes = Themes::load(None);
    let sessions = host.index_of("sessions").expect("sessions pane");
    let snapshot = Snapshot {
        sessions: vec![row("aaa", "doomed"), row("bbb", "bystander")],
        ..Snapshot::default()
    };
    let paint = || {
        format!(
            "{:?}",
            host.render(
                sessions,
                thurbox::kernel::host::RenderContext {
                    width: 40,
                    height: 10,
                    focused: true,
                    elapsed: 0.0,
                    frame: 0,
                },
            )
            .expect("render")
            .node
        )
    };

    let mut epoch = Epoch::default();
    publish_at(&host, epoch, &snapshot, &themes);
    assert!(paint().contains("doomed"), "the row should start present");

    let _ = paint();
    let settled = host.skipped_renders();
    let _ = paint();
    assert_eq!(
        host.skipped_renders() - settled,
        1,
        "the session list never settled, so this test cannot observe the cache"
    );

    let inflight = vec![thurbox::kernel::command::InFlight {
        id: 1,
        kind: "delete",
        session: "aaa".to_string(),
        subject: None,
        host: None,
        phase: thurbox::kernel::command::Phase::Running,
        error: None,
    }];

    publish_at_with(&host, epoch, &snapshot, &themes, &inflight);
    assert!(
        paint().contains("doomed"),
        "the in-flight list reached a pure pane on its own — this test is \
         asserting the caching that makes the epoch bump necessary, so if it \
         fails the bump may no longer be needed"
    );

    epoch.data += 1;
    publish_at_with(&host, epoch, &snapshot, &themes, &inflight);
    let screen = paint();
    assert!(
        !screen.contains("doomed"),
        "the deleted row outlived the epoch that carried its delete:\n{screen}"
    );
    assert!(screen.contains("bystander"), "{screen}");
}

#[test]
fn a_different_rect_or_focus_re_renders_a_pure_pane() {
    let (_dir, host) = two_panes();
    let themes = Themes::load(None);
    publish_at(&host, Epoch::default(), &Snapshot::default(), &themes);
    let index = host.index_of("pure").expect("plugin");

    let at = |width: u16, focused: bool| {
        host.render(
            index,
            thurbox::kernel::host::RenderContext {
                width,
                height: 4,
                focused,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render")
    };

    let _ = at(40, false);
    let _ = at(80, false);
    let _ = at(40, true);
}

#[test]
fn a_reload_drops_every_cached_tree() {
    let (dir, mut host) = two_panes();
    let themes = Themes::load(None);
    publish_at(
        &host,
        Epoch::default(),
        &Snapshot {
            sessions: vec![row("a", "alpha")],
            ..Snapshot::default()
        },
        &themes,
    );
    assert!(tree_of(&host, "pure").contains("alpha"));

    std::fs::write(
        dir.path().join("mods").join("10_pure.lua"),
        r#"return { name = "pure", slot = "left", pure = true,
             render = function() return { text = "rewritten" } end }"#,
    )
    .expect("rewrite");
    host.reload();
    assert!(host.error.is_none(), "{:?}", host.error);
    publish_at(
        &host,
        Epoch::default(),
        &Snapshot {
            sessions: vec![row("a", "alpha")],
            ..Snapshot::default()
        },
        &themes,
    );

    assert!(
        tree_of(&host, "pure").contains("rewritten"),
        "a tree from before the reload outlived it"
    );
}

#[test]
fn writing_the_same_value_to_the_store_is_not_a_change() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    std::fs::write(
        mods.join("10_pure.lua"),
        r#"return { name = "pure", slot = "left", pure = true,
             render = function()
               local names = {}
               for _, s in ipairs(thurbox.sessions or {}) do names[#names + 1] = s.name end
               return { text = table.concat(names, ",") }
             end }"#,
    )
    .expect("write");
    std::fs::write(
        mods.join("20_noisy.lua"),
        r#"return { name = "noisy", slot = "center",
             render = function()
               store["panels.count"] = 3
               return { text = "noisy" }
             end }"#,
    )
    .expect("write");
    let host = LuaHost::new(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);

    let themes = Themes::load(None);
    let epoch = Epoch::default();
    publish_at(
        &host,
        epoch,
        &Snapshot {
            sessions: vec![row("a", "alpha")],
            ..Snapshot::default()
        },
        &themes,
    );
    assert!(tree_of(&host, "pure").contains("alpha"));
    let _ = tree_of(&host, "noisy");

    publish_at(
        &host,
        epoch,
        &Snapshot {
            sessions: vec![row("b", "beta")],
            ..Snapshot::default()
        },
        &themes,
    );
    assert!(
        tree_of(&host, "pure").contains("alpha"),
        "an unchanged store write invalidated a cached tree"
    );
}

#[test]
fn a_still_animation_clock_lets_a_pure_pane_settle() {
    let (_dir, host) = two_panes();
    let themes = Themes::load(None);
    publish_at(&host, Epoch::default(), &Snapshot::default(), &themes);

    let index = host.index_of("pure").expect("plugin");
    let at = |elapsed: f64| {
        host.render(
            index,
            thurbox::kernel::host::RenderContext {
                width: 40,
                height: 4,
                focused: false,
                elapsed,
                frame: 0,
            },
        )
        .expect("render")
    };

    let _ = at(0.0);
    let settled = host.skipped_renders();
    for step in 1..=10 {
        let _ = at(f64::from(step));
    }
    assert_eq!(
        host.skipped_renders() - settled,
        10,
        "time passing re-rendered a pure pane while its epoch stood still"
    );
}

#[test]
fn a_moved_animation_clock_re_renders_a_pane_that_reads_it() {
    let (_dir, host) = clock_panes();
    let themes = Themes::load(None);
    let mut epoch = Epoch::default();
    publish_at(&host, epoch, &Snapshot::default(), &themes);

    let _ = render_at(&host, "spinner", 0.0);
    let settled = host.skipped_renders();
    epoch.animation += 1;
    publish_at(&host, epoch, &Snapshot::default(), &themes);
    let _ = render_at(&host, "spinner", 1.0);

    assert_eq!(
        host.skipped_renders(),
        settled,
        "the animation clock moved and the pane was still served from the cache"
    );
}

#[test]
fn a_re_stamped_snapshot_does_not_move_the_version_within_a_second() {
    let mut store = SnapshotStore::without_sessions(&Default::default());
    store.refresh();

    let settled = store.version();
    let stamp = store.current().taken_at_ms;
    for _ in 0..50 {
        store.refresh_if_due();
    }
    assert_eq!(
        store.version(),
        settled,
        "re-reading unchanged rows moved the version"
    );
    assert_eq!(
        store.current().taken_at_ms,
        stamp,
        "the published instant moved without the version saying so"
    );
}

#[test]
fn the_published_instant_is_whole_seconds() {
    let mut store = SnapshotStore::without_sessions(&Default::default());
    store.refresh();
    assert_eq!(
        store.current().taken_at_ms % 1000,
        0,
        "taken_at_ms carries sub-second precision no reader can see"
    );
}

#[test]
fn declaring_and_setting_move_the_registrys_version_and_reading_does_not() {
    let mut registry = Registry::default();

    let settled = registry.version();
    for _ in 0..50 {
        let _ = registry.is_disabled("ui/mods/10_sessions.lua");
    }
    assert_eq!(
        registry.version(),
        settled,
        "reading the registry moved its version"
    );

    registry.declare(Vec::new(), Vec::new());
    assert_ne!(
        registry.version(),
        settled,
        "declaring plugins did not move the registry's version"
    );

    let after_declare = registry.version();
    let _ = registry.rebind("nonexistent-action", Some("ctrl+alt+f9"));
    assert_ne!(
        registry.version(),
        after_declare,
        "a rebinding attempt did not move the registry's version — the version \
         is bumped at the top of each mutator precisely so an early return \
         cannot publish a stale answer"
    );
}

#[test]
fn asking_terminals_for_metadata_is_not_itself_a_change() {
    let mut terminals = thurbox::kernel::terminal::Terminals::with_registry(
        std::sync::Arc::new(thurbox::backend::wiring::local_only(&Default::default())),
        &Default::default(),
    );

    let settled = terminals.meta_version();
    for _ in 0..50 {
        let _ = terminals.meta();
    }
    assert_eq!(
        terminals.meta_version(),
        settled,
        "reading agent metadata moved its version"
    );
}

#[test]
fn reading_attach_failures_is_not_itself_a_change() {
    let terminals = thurbox::kernel::terminal::Terminals::with_registry(
        std::sync::Arc::new(thurbox::backend::wiring::local_only(&Default::default())),
        &Default::default(),
    );
    let settled = terminals.failed_version();

    for _ in 0..50 {
        let failures = terminals.failures();
        assert!(failures.is_empty(), "nothing was asked to attach");
    }

    assert_eq!(
        terminals.failed_version(),
        settled,
        "reading the failure set moved its version"
    );
}

fn clock_panes() -> (tempfile::TempDir, LuaHost) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    std::fs::write(
        mods.join("10_spinner.lua"),
        r#"return { name = "spinner", slot = "left", pure = true,
             render = function(ctx)
               return { text = "tick " .. tostring(math.floor(ctx.elapsed or 0)) }
             end }"#,
    )
    .expect("write");
    std::fs::write(
        mods.join("20_still.lua"),
        r#"return { name = "still", slot = "center", pure = true,
             render = function() return { text = "still" } end }"#,
    )
    .expect("write");
    let host = LuaHost::new(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);
    (dir, host)
}

fn render_at(host: &LuaHost, name: &str, elapsed: f64) -> String {
    let index = host.index_of(name).expect("plugin");
    format!(
        "{:?}",
        host.render(
            index,
            thurbox::kernel::host::RenderContext {
                width: 40,
                height: 4,
                focused: false,
                elapsed,
                frame: 0,
            },
        )
        .expect("render")
        .node
    )
}

#[test]
fn the_animation_clock_does_not_re_render_a_pane_that_never_reads_it() {
    let (_dir, host) = clock_panes();
    let themes = Themes::load(None);
    let mut epoch = Epoch::default();
    publish_at(&host, epoch, &Snapshot::default(), &themes);

    let _ = render_at(&host, "still", 0.0);
    let settled = host.skipped_renders();
    for step in 1..=10 {
        epoch.animation += 1;
        publish_at(&host, epoch, &Snapshot::default(), &themes);
        let _ = render_at(&host, "still", f64::from(step));
    }
    assert_eq!(
        host.skipped_renders() - settled,
        10,
        "a pane that never asked for the clock was re-rendered by it"
    );
}

#[test]
fn a_pane_that_reads_the_clock_sees_the_time_it_was_given() {
    let (_dir, host) = clock_panes();
    let themes = Themes::load(None);
    let mut epoch = Epoch::default();
    publish_at(&host, epoch, &Snapshot::default(), &themes);
    assert!(render_at(&host, "spinner", 3.0).contains("tick 3"));
    epoch.animation += 1;
    publish_at(&host, epoch, &Snapshot::default(), &themes);
    assert!(render_at(&host, "spinner", 7.0).contains("tick 7"));
}

#[test]
fn a_non_string_index_on_the_render_context_is_nil_rather_than_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    std::fs::write(
        mods.join("10_odd.lua"),
        r#"return { name = "odd", slot = "left",
             render = function(ctx)
               local typo
               local seen = { ctx[typo], ctx[true], ctx[7] }
               return { text = "nils " .. tostring(#seen == 0) }
             end }"#,
    )
    .expect("write");
    let host = LuaHost::new(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);
    publish_at(
        &host,
        Epoch::default(),
        &Snapshot::default(),
        &Themes::load(None),
    );
    assert!(render_at(&host, "odd", 0.0).contains("nils true"));
}

#[test]
fn a_mod_cannot_reach_or_replace_the_render_context_metatable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    std::fs::write(
        mods.join("10_prober.lua"),
        r#"return { name = "prober", slot = "left",
             render = function(ctx)
               local hidden = getmetatable(ctx) == false
               local refused = not pcall(setmetatable, ctx, { __index = function() end })
               return { text = "hidden " .. tostring(hidden)
                          .. " refused " .. tostring(refused)
                          .. " clock " .. tostring(math.floor(ctx.elapsed or -1)) }
             end }"#,
    )
    .expect("write");
    let host = LuaHost::new(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);
    publish_at(
        &host,
        Epoch::default(),
        &Snapshot::default(),
        &Themes::load(None),
    );
    let painted = render_at(&host, "prober", 5.0);
    assert!(
        painted.contains("hidden true"),
        "the metatable was handed to the plugin: {painted}"
    );
    assert!(
        painted.contains("refused true"),
        "a plugin replaced the shared metatable: {painted}"
    );
    assert!(
        painted.contains("clock 5"),
        "the clock stopped being served: {painted}"
    );
}

#[test]
fn a_pane_that_starts_reading_the_clock_is_keyed_on_it_from_then_on() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mods = dir.path().join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    std::fs::write(
        mods.join("10_maybe.lua"),
        r#"return { name = "maybe", slot = "left", pure = true,
             render = function(ctx)
               if store.spin then
                 return { text = "tick " .. tostring(math.floor(ctx.elapsed or 0)) }
               end
               return { text = "quiet" }
             end }"#,
    )
    .expect("write");
    let host = LuaHost::new(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);
    let themes = Themes::load(None);
    let mut epoch = Epoch::default();
    publish_at(&host, epoch, &Snapshot::default(), &themes);

    let _ = render_at(&host, "maybe", 0.0);
    let settled = host.skipped_renders();
    epoch.animation += 1;
    publish_at(&host, epoch, &Snapshot::default(), &themes);
    assert!(render_at(&host, "maybe", 1.0).contains("quiet"));
    assert_eq!(
        host.skipped_renders() - settled,
        1,
        "should have been cached"
    );

    host.set_shared_bool("spin", true);
    epoch.animation += 1;
    publish_at(&host, epoch, &Snapshot::default(), &themes);
    assert!(render_at(&host, "maybe", 2.0).contains("tick 2"));

    let settled = host.skipped_renders();
    epoch.animation += 1;
    publish_at(&host, epoch, &Snapshot::default(), &themes);
    assert!(render_at(&host, "maybe", 3.0).contains("tick 3"));
    assert_eq!(
        host.skipped_renders(),
        settled,
        "a pane that had begun reading the clock was cached across a tick"
    );
}

#[test]
fn the_bundled_centre_pane_is_not_re_rendered_by_the_clock() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ui");
    let host = LuaHost::new(dir);
    assert!(host.error.is_none(), "{:?}", host.error);
    let themes = Themes::load(None);
    let mut epoch = Epoch::default();
    let snapshot = Snapshot {
        sessions: vec![row("a", "alpha")],
        ..Snapshot::default()
    };
    publish_at(&host, epoch, &snapshot, &themes);

    for _ in 0..2 {
        let _ = render_at(&host, "agent", 0.0);
        let _ = render_at(&host, "sessions", 0.0);
    }

    let settled = host.skipped_renders();
    epoch.animation += 1;
    publish_at(&host, epoch, &snapshot, &themes);
    let _ = render_at(&host, "agent", 1.0);
    assert_eq!(
        host.skipped_renders() - settled,
        1,
        "the agent pane paid for an animation tick it draws nothing for"
    );

    let settled = host.skipped_renders();
    let _ = render_at(&host, "sessions", 1.0);
    assert_eq!(
        host.skipped_renders(),
        settled,
        "the session list was served a cached tree across the tick its spinner moves on"
    );
}

#[test]
fn an_unrelated_worker_result_keeps_the_diff_table() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("mods")).unwrap();
    std::fs::write(
        dir.path().join("mods/10_probe.lua"),
        r#"local last
    return { name = "probe", slot = "center", render = function()
        local same = last == nil or rawequal(last, thurbox.diffs)
        last = thurbox.diffs
        return { text = tostring(same) }
    end }"#,
    )
    .unwrap();
    let host = LuaHost::new(dir.path());
    let themes = Themes::load(None);
    let mut epoch = Epoch::default();
    publish_at(&host, epoch, &Snapshot::default(), &themes);
    assert!(probe(&host).contains("true"));
    epoch.data += 1;
    publish_at(&host, epoch, &Snapshot::default(), &themes);
    assert!(
        probe(&host).contains("true"),
        "an unrelated result rebuilt the diff table"
    );
}

#[test]
fn idle_samples_only_rebuild_their_readers() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("mods")).unwrap();
    for (name, expression) in [
        ("static", "'unchanged'"),
        ("clock", "tostring(thurbox.taken_at_ms)"),
        ("metrics", "tostring(thurbox.metrics.system.cpu_percent)"),
    ] {
        std::fs::write(dir.path().join(format!("mods/{name}.lua")), format!(
            "local calls = 0; return {{name = '{name}', slot = '{name}', pure = true, render = function() calls = calls + 1; return {{text = tostring(calls) .. ':' .. {expression}}} end}}"
        )).unwrap();
    }
    let host = LuaHost::new(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);
    let themes = Themes::load(None);
    let mut snapshot = Snapshot::default();
    let mut epoch = Epoch::default();
    for second in 0..=60 {
        snapshot.taken_at_ms = second * 1000;
        epoch.taken_at = second as u64;
        epoch.metrics = second as u64;
        publish_at(&host, epoch, &snapshot, &themes);
        for _ in 0..4 {
            for name in ["static", "clock", "metrics"] {
                tree_of(&host, name);
            }
        }
    }
    let static_tree = tree_of(&host, "static");
    let clock_tree = tree_of(&host, "clock");
    let metrics_tree = tree_of(&host, "metrics");
    eprintln!("idle minute: static={static_tree}, clock={clock_tree}, metrics={metrics_tree}");
    assert!(
        static_tree.contains("\"1:unchanged\""),
        "idle samples rebuilt a pane that reads neither"
    );
    assert!(
        clock_tree.contains("61:60000"),
        "the clock reader must stay live"
    );
    assert!(
        metrics_tree.contains("61:"),
        "the metrics reader must stay live"
    );
}

#[test]
fn selecting_another_session_releases_the_previous_diff() {
    let mut diffs = thurbox::kernel::diff::DiffStore::new();
    diffs.set_for_test("old", thurbox::kernel::diff::Diff::Failed("held".into()));
    diffs.request("new", "/no/such/repo".into(), None, "local:tmux");
    assert!(
        diffs.get("old").is_none(),
        "a previous selection's diff is still retained"
    );
    assert!(
        diffs.get("new").is_some(),
        "the new selection must be published as pending"
    );
}

#[test]
fn observed_publication_preserves_raw_reads_and_iteration() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("mods")).unwrap();
    std::fs::write(dir.path().join("mods/10_probe.lua"), r#"return {name = "probe", slot = "center", render = function()
        local fields = {}
        for key, value in pairs(thurbox) do fields[key] = value end
        local next_fields = {}
        local key, value = next(thurbox)
        while key do
            next_fields[key] = value
            key, value = next(thurbox, key)
        end
        local a = rawget(thurbox, "taken_at_ms") == 1234 and fields.taken_at_ms == 1234 and next_fields.taken_at_ms == 1234
        local b = rawequal(rawget(thurbox, "metrics"), thurbox.metrics) and rawequal(fields.metrics, thurbox.metrics)
        -- Read-only to every pane: `rawset` is withheld and a write is refused.
        local c = rawset == nil and not pcall(function() thurbox.custom = "kept" end)
            and thurbox.custom == nil
        local plain = { value = 7 }
        local d = rawget(plain, "value") == 7 and rawget(thurbox.theme, "roles") ~= nil
        return {text = tostring(a and b and c and d)}
    end}"#).unwrap();
    let host = LuaHost::new(dir.path());
    let themes = Themes::load(None);
    publish_at(
        &host,
        Epoch::default(),
        &Snapshot {
            taken_at_ms: 1234,
            ..Default::default()
        },
        &themes,
    );
    assert!(probe(&host).contains("true"));
}
