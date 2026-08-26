//! Plugin events, through the real kernel: a plugin declares what it listens
//! for, the kernel derives what changed and hands each subscriber one call per
//! change, and a handler that fails costs only itself
//! (`openspec/changes/plugin-events-and-command-palette`).
//!
//! What lives in the loop — the dispatch point, the cascade bound, focus and
//! command events, the reload — is the binary's and is asserted by its unit
//! tests and the design; this file covers the kernel's half, which is the half
//! a plugin author can observe.

use std::path::PathBuf;

use thurbox::kernel::command::Command;
use thurbox::kernel::events::{Deriver, Event, Field};
use thurbox::kernel::host::{LuaHost, Phase, Published, RenderContext};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::{SessionRow, Snapshot};
use thurbox::kernel::theme::Themes;

/// An interface directory holding the given plugins, each `(file, source)`.
fn interface(plugins: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let plugins_dir = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins_dir).expect("mkdir");
    for (file, source) in plugins {
        std::fs::write(plugins_dir.join(file), source).expect("write");
    }
    dir
}

fn host_of(plugins: &[(&str, &str)]) -> (tempfile::TempDir, LuaHost) {
    let dir = interface(plugins);
    let host = LuaHost::new(dir.path());
    assert!(host.error.is_none(), "{:?}", host.error);
    (dir, host)
}

fn row(name: &str, status: &str) -> SessionRow {
    SessionRow {
        id: format!("{name}-0000-0000-0000-000000000000"),
        name: name.to_string(),
        agent: "claude".to_string(),
        status: status.to_string(),
        cwd: Some(PathBuf::from("/src/thurbox")),
        repo: Some("thurbox".to_string()),
        repos: vec!["thurbox".to_string()],
        branch: Some(format!("feat/{name}")),
        base_branch: None,
        backend: "local-tmux".to_string(),
        backend_id: Some("%1".to_string()),
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

fn publish(host: &LuaHost, snapshot: &Snapshot) {
    let themes = Themes::load(None);
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
}

fn status_event(session: &str, from: &str, to: &str) -> Event {
    Event::new("session.status")
        .with("session", Some(session))
        .with("name", Some(session))
        .with("from", Some(from))
        .with("to", Some(to))
}

/// A plugin that records every event it receives in the shared `store`, so a
/// test can read what it saw without rendering it.
const RECORDER: &str = r#"
return {
  name = "recorder", slot = "a",
  events = { "session.status" },
  on_event = function(name, payload)
    store.seen = (store.seen or 0) + 1
    store.last = name .. " " .. tostring(payload.session) .. " " .. tostring(payload.from) .. ">" .. tostring(payload.to)
  end,
  render = function() return { text = tostring(store.seen or 0) } end,
}
"#;

#[test]
fn a_declared_subscription_fires_once_with_its_payload() {
    let (_dir, host) = host_of(&[("10_recorder.lua", RECORDER)]);
    let failures = host.dispatch_event(&status_event("s1", "idle", "blocked"));
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(
        host.shared_string("last").as_deref(),
        Some("session.status s1 idle>blocked")
    );
    // Once: a second dispatch of a different event is not a second call.
    host.dispatch_event(&Event::new("session.created").with("session", Some("s2")));
    assert_eq!(
        host.shared_string("last").as_deref(),
        Some("session.status s1 idle>blocked"),
        "an undeclared event must not reach the handler"
    );
}

#[test]
fn a_handler_with_no_subscriptions_receives_nothing() {
    let (_dir, host) = host_of(&[(
        "10_mute.lua",
        r#"return {
             name = "mute", slot = "a",
             on_event = function() store.heard = true end,
             render = function() return { text = "" } end,
           }"#,
    )]);
    host.dispatch_event(&status_event("s1", "idle", "blocked"));
    assert_eq!(host.shared_bool("heard"), None);
}

#[test]
fn a_subscription_to_a_name_the_kernel_does_not_emit_refuses_to_load() {
    let dir = interface(&[(
        "10_typo.lua",
        r#"return {
             name = "typo", slot = "a",
             events = { "sesion.status" },
             on_event = function() end,
             render = function() return { text = "" } end,
           }"#,
    )]);
    let host = LuaHost::new(dir.path());
    let error = host.error.clone().expect("the plugin must not load");
    assert!(error.contains("sesion.status"), "{error}");
    assert!(
        error.contains("session.status"),
        "the message should list what was available: {error}"
    );
    // The `user.` form loads, so plugins can talk to each other by name.
    let dir = interface(&[(
        "10_user.lua",
        r#"return {
             name = "user", slot = "a",
             events = { "user.refresh" },
             on_event = function() end,
             render = function() return { text = "" } end,
           }"#,
    )]);
    assert!(LuaHost::new(dir.path()).error.is_none());
}

#[test]
fn one_subscriber_throwing_costs_neither_the_others_nor_its_own_pane() {
    let (_dir, host) = host_of(&[
        (
            "10_thrower.lua",
            r#"return {
                 name = "thrower", slot = "a",
                 events = { "session.status" },
                 on_event = function() error("no thanks") end,
                 render = function() return { text = "still drawing" } end,
               }"#,
        ),
        ("20_recorder.lua", RECORDER),
    ]);
    let failures = host.dispatch_event(&status_event("s1", "idle", "blocked"));
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].plugin, "thrower");
    assert_eq!(failures[0].phase, Phase::Event);
    assert!(
        failures[0].message.contains("session.status") && failures[0].message.contains("no thanks"),
        "the failure names the event and the plugin's own message: {}",
        failures[0].message
    );
    // The second subscriber still ran.
    assert_eq!(
        host.shared_string("last").as_deref(),
        Some("session.status s1 idle>blocked")
    );
    // And the thrower's pane still renders on the next frame.
    let rendered = host
        .render(
            host.index_of("thrower").expect("loaded"),
            RenderContext {
                width: 20,
                height: 3,
                focused: false,
                elapsed: 0.0,
                frame: 1,
            },
        )
        .expect("render");
    assert!(format!("{:?}", rendered.node).contains("still drawing"));
}

#[test]
fn a_handler_reads_the_published_tables_and_can_only_enqueue() {
    let (_dir, host) = host_of(&[(
        "10_reader.lua",
        r#"return {
             name = "reader", slot = "a",
             events = { "session.status" },
             on_event = function(name, payload)
               local first = thurbox.sessions[1]
               store.name = first and first.name or "nobody"
               command("send", { session = payload.session, text = "you are " .. payload.to })
               return "ignored"
             end,
             render = function() return { text = "" } end,
           }"#,
    )]);
    let snapshot = Snapshot {
        sessions: vec![row("fix-osc52", "idle")],
        ..Snapshot::default()
    };
    publish(&host, &snapshot);
    let failures = host.dispatch_event(&status_event("s1", "idle", "blocked"));
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(host.shared_string("name").as_deref(), Some("fix-osc52"));
    let issued = host.drain_commands();
    assert_eq!(
        issued,
        vec![Command::Send {
            session: "s1".into(),
            text: "you are blocked".into()
        }]
    );
}

#[test]
fn a_plugin_emits_a_user_event_and_another_receives_it_with_the_source() {
    let (_dir, host) = host_of(&[
        (
            "10_sender.lua",
            r#"return {
                 name = "sender", slot = "a",
                 keys = { { key = "e", action = "sender.emit", desc = "emit" } },
                 on_action = function()
                   command("emit", { text = "refresh", scope = "x", count = 2, urgent = true })
                   return true
                 end,
                 render = function() return { text = "" } end,
               }"#,
        ),
        (
            "20_listener.lua",
            r#"return {
                 name = "listener", slot = "b",
                 events = { "user.refresh" },
                 on_event = function(name, payload)
                   store.got = name .. " from " .. tostring(payload.source) .. " scope=" .. tostring(payload.scope)
                     .. " count=" .. tostring(payload.count) .. " urgent=" .. tostring(payload.urgent)
                 end,
                 render = function() return { text = "" } end,
               }"#,
        ),
    ]);
    let sender = host.index_of("sender").expect("loaded");
    assert!(host.on_action(sender, "sender.emit").expect("on_action"));
    let issued = host.drain_commands();
    let Some(Command::Emit {
        owner,
        name,
        payload,
    }) = issued.first().cloned()
    else {
        panic!("expected an emit, got {issued:?}");
    };
    // Owner is stamped from the executing plugin, never read from Lua.
    assert_eq!(owner, "plugins/10_sender.lua");
    assert_eq!(name, "user.refresh");
    assert!(payload.contains(&("scope".to_string(), Field::Text("x".into()))));
    assert!(payload.contains(&("count".to_string(), Field::Number(2.0))));
    assert!(payload.contains(&("urgent".to_string(), Field::Bool(true))));

    // As the loop delivers it: the source is the emitting plugin's NAME.
    let source = host.name_of_path(&owner).expect("the owner is loaded");
    let mut event = Event::new(name);
    event.payload = payload;
    event.payload.push(("source".into(), source.into()));
    let failures = host.dispatch_event(&event);
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(
        host.shared_string("got").as_deref(),
        Some("user.refresh from sender scope=x count=2 urgent=true")
    );
}

#[test]
fn a_plugin_cannot_emit_a_kernel_event() {
    let (_dir, host) = host_of(&[(
        "10_forger.lua",
        r#"return {
             name = "forger", slot = "a",
             keys = { { key = "e", action = "forger.emit", desc = "forge" } },
             on_action = function()
               command("emit", { text = "session.created", session = "fake" })
               return true
             end,
             render = function() return { text = "" } end,
           }"#,
    )]);
    let error = host
        .on_action(host.index_of("forger").expect("loaded"), "forger.emit")
        .expect_err("a kernel name must be refused");
    assert!(error.message.contains("kernel event"), "{}", error.message);
    assert!(host.drain_commands().is_empty(), "nothing was queued");
}

#[test]
fn emit_is_reachable_only_through_command() {
    // No new global: the event bus rides the one write channel plugins have.
    let (_dir, host) = host_of(&[(
        "10_probe.lua",
        r#"return {
             name = "probe", slot = "a",
             render = function()
               return { text = tostring(rawget(_G, "emit")) .. " " .. tostring(rawget(_G, "on_event")) }
             end,
           }"#,
    )]);
    let rendered = host
        .render(
            0,
            RenderContext {
                width: 20,
                height: 1,
                focused: false,
                elapsed: 0.0,
                frame: 1,
            },
        )
        .expect("render");
    assert!(format!("{:?}", rendered.node).contains("nil nil"));
}

#[test]
fn what_the_deriver_says_changed_is_what_a_subscriber_hears() {
    // The kernel's half of "a session created by another process": the row
    // appears in a snapshot, and every subscriber hears `session.created` once —
    // whoever wrote it.
    let (_dir, host) = host_of(&[(
        "10_watcher.lua",
        r#"return {
             name = "watcher", slot = "a",
             events = { "session.created", "session.deleted", "session.status" },
             on_event = function(name, payload)
               store.log = (store.log or "") .. name .. ":" .. tostring(payload.name) .. ";"
             end,
             render = function() return { text = "" } end,
           }"#,
    )]);
    let mut deriver = Deriver::new();
    let first = Snapshot {
        sessions: vec![row("a", "idle")],
        ..Snapshot::default()
    };
    // Seeds: the rows that existed before the plugin did are not news.
    for event in deriver.observe(&first, 1) {
        host.dispatch_event(&event);
    }
    assert_eq!(host.shared_string("log"), None);

    let second = Snapshot {
        sessions: vec![row("a", "blocked"), row("b", "idle")],
        ..Snapshot::default()
    };
    for event in deriver.observe(&second, 2) {
        let failures = host.dispatch_event(&event);
        assert!(failures.is_empty(), "{failures:?}");
    }
    assert_eq!(
        host.shared_string("log").as_deref(),
        Some("session.created:b;session.status:a;")
    );
    // Same version again: nothing is re-delivered.
    for event in deriver.observe(&second, 2) {
        host.dispatch_event(&event);
    }
    assert_eq!(
        host.shared_string("log").as_deref(),
        Some("session.created:b;session.status:a;")
    );
}

#[test]
fn a_subscription_is_listed_as_data() {
    let (_dir, host) = host_of(&[("10_recorder.lua", RECORDER)]);
    let plugin = &host.plugins[host.index_of("recorder").expect("loaded")];
    assert_eq!(plugin.events, vec!["session.status".to_string()]);
    assert_eq!(host.subscribers("session.status"), vec![0]);
    assert!(host.subscribers("session.created").is_empty());
}
