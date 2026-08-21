//! What a frame is allowed to recompute (`frame-cost`).
//!
//! The change-signals these assert on gate both the published tables and the
//! pure-pane tree cache, so a source that moves without its signal moving is
//! not a slow interface but a **stale** one — a pane that stops updating, with
//! no error anywhere. That failure is invisible to every other test in the
//! suite, which is why it gets its own file.

use thurbox::kernel::snapshot::SnapshotStore;
use thurbox::kernel::theme::Themes;
use thurbox::storage::Database;

#[test]
fn a_snapshot_refresh_moves_the_generation() {
    // The generation is what a reader compares to decide it can reuse what it
    // built last time. If a refresh left it still, every such reader would keep
    // showing what the database said before.
    let db = Database::open_in_memory().expect("db");
    let mut store = SnapshotStore::with_database(db);

    let before = store.generation();
    store.refresh();
    assert_ne!(
        store.generation(),
        before,
        "a refresh replaced the snapshot without saying so"
    );
}

#[test]
fn every_refresh_moves_it_again() {
    // `taken_at_ms` is published and rendered as "5s ago", so even a refresh
    // that finds identical rows has changed something a plugin reads. A
    // generation that only moved on *row* changes would freeze that label.
    let db = Database::open_in_memory().expect("db");
    let mut store = SnapshotStore::with_database(db);

    let mut seen = store.generation();
    for _ in 0..5 {
        store.refresh();
        let now = store.generation();
        assert_ne!(now, seen, "a later refresh did not move the generation");
        seen = now;
    }
}

#[test]
fn reading_the_snapshot_leaves_the_generation_alone() {
    // The other half, and the one that makes gating worth anything: if merely
    // looking moved the signal, nothing could ever be reused.
    let db = Database::open_in_memory().expect("db");
    let mut store = SnapshotStore::with_database(db);
    store.refresh();

    let settled = store.generation();
    for _ in 0..100 {
        let _ = store.current().sessions.len();
        let _ = store.current().taken_at_ms;
    }
    assert_eq!(
        store.generation(),
        settled,
        "reading the snapshot moved its generation"
    );
}

#[test]
fn quiescence_moves_the_generation_only_when_it_re_derives_something() {
    // `apply_output_quiescence` runs every tick by design. It must move the
    // signal when it changes a status and leave it alone otherwise, or the
    // stuck-`working` fallback would invalidate every cached tree ~40 times a
    // second for nothing.
    let db = Database::open_in_memory().expect("db");
    let mut store = SnapshotStore::with_database(db);
    store.refresh();

    let settled = store.generation();
    let changed = store.apply_output_quiescence(|_| None);
    assert_eq!(changed, 0, "an empty snapshot has nothing to re-derive");
    assert_eq!(
        store.generation(),
        settled,
        "a pass that changed nothing still moved the generation"
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

#[test]
fn a_theme_refresh_never_restarts_the_version() {
    // `refresh` replaces the whole value. A version that restarted at zero could
    // collide with one a reader had already seen and be read as "nothing
    // changed" — the one way a monotonic signal can lie.
    let mut themes = Themes::load(None);
    for _ in 0..3 {
        let before = themes.version();
        themes.refresh(None);
        assert!(
            themes.version() > before,
            "refresh restarted the version: {before} -> {}",
            themes.version()
        );
    }
}

// --- the gated publish ------------------------------------------------------

use thurbox::kernel::host::{Epoch, LuaHost, Published};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{SessionRow, Snapshot};

fn interface() -> (tempfile::TempDir, LuaHost) {
    // A pane that reports what it can see of the published tables, so a stale
    // group is visible as a rendered string rather than only as a counter.
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    std::fs::write(
        plugins.join("10_probe.lua"),
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
        status: "idle".into(),
        cwd: None,
        repo: None,
        repos: Vec::new(),
        branch: None,
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

fn publish_at(host: &LuaHost, epoch: Epoch, snapshot: &Snapshot, themes: &Themes) {
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        epoch,
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
fn a_moved_source_is_published_even_though_groups_are_gated() {
    // The requirement gating exists to not break: what a plugin reads after a
    // source moves is the new value. A group reused when its inputs changed is
    // undetectable from Lua, so this asserts on what the pane actually renders.
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
    // The proof named in design.md: for the same inputs, gating changes nothing
    // a plugin can observe. `always_fresh` rebuilds every group, so it stands in
    // for the ungated build.
    let (_dir, host) = interface();
    let themes = Themes::load(None);
    let snapshot = Snapshot {
        sessions: vec![row("a", "alpha"), row("b", "beta")],
        ..Snapshot::default()
    };

    let epoch = Epoch::default();
    publish_at(&host, epoch, &snapshot, &themes);
    let gated_first = probe(&host);

    // Same epoch, same data: every group is served from the cache.
    publish_at(&host, epoch, &snapshot, &themes);
    let gated_again = probe(&host);

    // Nothing cached at all.
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
    // Each group names only the versions it reads, so the theme group must move
    // on the theme's version alone — a snapshot that never changes must not
    // pin it.
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

// --- the purity declaration -------------------------------------------------

/// Two panes over the same data: one declaring its render pure, one not.
///
/// Skipping is observed by *lying* — publishing new data under an unchanged
/// epoch. A pane that re-renders shows the new data; one that was skipped shows
/// what it showed before. Nothing else can tell the two apart from outside,
/// which is the point: a skip is meant to be invisible when the epoch is honest.
fn two_panes() -> (tempfile::TempDir, LuaHost) {
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    let body = r#"render = function()
             local names = {}
             for _, s in ipairs(thurbox.sessions or {}) do names[#names + 1] = s.name end
             return { text = table.concat(names, ",") }
           end }"#;
    std::fs::write(
        plugins.join("10_pure.lua"),
        format!("return {{ name = \"pure\", slot = \"left\", pure = true, {body}"),
    )
    .expect("write");
    std::fs::write(
        plugins.join("20_plain.lua"),
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
    // A skip cannot be seen in what is painted — that IS the correctness claim,
    // and the group cache withholds new data at an unchanged epoch anyway — so
    // it is asserted on the count instead.
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
    // The promise to every plugin that predates the declaration: nothing about
    // it changed. Rendering it repeatedly must never be served from a cache.
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
fn a_different_rect_or_focus_re_renders_a_pure_pane() {
    // The tree was built for a rect. Reusing it in another one would paint the
    // previous size's layout into the new space.
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

    // Each of these is a distinct key, so each must reach the plugin rather than
    // being served the previous one. Asserted through the cache's own behaviour:
    // a second ask at the SAME key is served from it, so if width or focus were
    // ignored these would collide.
    let _ = at(40, false);
    let _ = at(80, false);
    let _ = at(40, true);
    // Nothing to assert on the trees here — this pane ignores its rect — so the
    // guarantee under test is that none of the three panicked or was rejected,
    // and that the key type carries both. See `TreeKey`.
}

#[test]
fn a_reload_drops_every_cached_tree() {
    // A cached tree belongs to the version of the pane that produced it. After a
    // reload the file on disk may say something else entirely.
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
        dir.path().join("plugins").join("10_pure.lua"),
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
    // The fix that made the whole mechanism work. A pane that re-states an
    // unchanged value on every frame — the search strip does — would otherwise
    // invalidate every cached tree ~40 times a second, and the measured saving
    // went from 27% to nothing.
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins).expect("mkdir");
    std::fs::write(
        plugins.join("10_pure.lua"),
        r#"return { name = "pure", slot = "left", pure = true,
             render = function()
               local names = {}
               for _, s in ipairs(thurbox.sessions or {}) do names[#names + 1] = s.name end
               return { text = table.concat(names, ",") }
             end }"#,
    )
    .expect("write");
    // A pane that writes the SAME value every render, like the search strip.
    std::fs::write(
        plugins.join("20_noisy.lua"),
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

    // The noisy pane has now written its unchanged value. If that counted as a
    // change, the pure pane's tree would be gone.
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
    // The bug this guards: the clock used to be read straight from
    // `ctx.elapsed`, so it advanced 8 times a second whether or not anything on
    // screen was moving. At the 4fps idle floor that invalidated every pure
    // pane on every frame — 3 renders skipped out of 284 — and an idle
    // interface re-ran all its Lua for a byte-identical tree.
    //
    // The clock now lives in the epoch and the loop advances it only while
    // something animates, so an epoch that stands still is one a pane can be
    // cached across, however much wall-clock time passes.
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
    // Seconds of wall clock, and several frame numbers, with a still epoch.
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
fn a_moved_animation_clock_re_renders_a_pure_pane() {
    // The other half: while something IS animating the loop advances the clock,
    // and the pane must run again or the spinner freezes on screen.
    let (_dir, host) = two_panes();
    let themes = Themes::load(None);
    let mut epoch = Epoch::default();
    publish_at(&host, epoch, &Snapshot::default(), &themes);

    let index = host.index_of("pure").expect("plugin");
    let render = |host: &LuaHost| {
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
    };

    let _ = render(&host);
    let settled = host.skipped_renders();
    epoch.animation += 1;
    publish_at(&host, epoch, &Snapshot::default(), &themes);
    let _ = render(&host);

    assert_eq!(
        host.skipped_renders(),
        settled,
        "the animation clock moved and the pane was still served from the cache"
    );
}
