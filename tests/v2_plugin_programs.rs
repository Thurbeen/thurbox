//! A plugin running an interactive program in a pane it owns.
//!
//! **No multiplexer is started here.** Spawning a real program needs tmux, which
//! the unit suite deliberately does not have (that is what the gated
//! `tui-smoke` job is for), so what is asserted is everything up to and including
//! the decision to spawn: who may ask, what the ask resolves to, what is drawn
//! when nothing is behind the surface, and where keys would go. The parts that
//! genuinely need a process are exercised by hand — see the change's tasks.
//!
//! The invisible failures are the gate and the attribution, exactly as they are
//! for `run`: a capability that is *present and refusing* looks identical to one
//! that is absent until you read the error, and a pane resolved to the wrong owner
//! looks like nothing at all until two plugins both want `watch`.

use thurbox::kernel::host::{Capability, LuaHost, RenderContext};
use thurbox::kernel::terminal::{ProgramKey, Terminals};

/// Build an interface out of `plugins`, each `(file name, source)`.
fn interface(plugins: &[(&str, &str)]) -> (tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = home.path().join("ui");
    thurbox::kernel::bundled::materialize(&ui);
    for (name, source) in plugins {
        std::fs::write(ui.join("plugins").join(name), source).expect("write");
    }
    (home, ui)
}

fn render(host: &LuaHost, name: &str) -> thurbox::kernel::host::Rendered {
    let index = host.index_of(name).unwrap_or_else(|| panic!("no {name}"));
    host.render(
        index,
        RenderContext {
            width: 40,
            height: 10,
            focused: true,
            elapsed: 0.0,
            frame: 0,
        },
    )
    .expect("render")
}

/// A pane that asks for a program and draws its surface.
fn watch_pane(name: &str, pane: &str, declares: bool) -> String {
    format!(
        r#"return {{
  name = "{name}",
  slot = "center",
  input = "session",
  {}
  render = function()
    command("program", {{ text = "{pane}", repo = "watch", args = {{ "-warp", "1" }} }})
    return {{ type = "surface", program = "{pane}", fill = 1 }}
  end,
}}"#,
        if declares {
            r#"capabilities = { "program" },"#
        } else {
            ""
        }
    )
}

// ── who may ask ────────────────────────────────────────────────────────────

#[test]
fn an_undeclared_plugin_asking_for_a_program_is_queued_but_may_not_run_it() {
    // The command is *queued* — `command` is always available — and refused when
    // it is honoured, because trust can be revoked between the ask and the doing.
    // What must never happen is the program starting.
    let (_home, ui) = interface(&[("91_watch.lua", &watch_pane("watch", "watch", false))]);
    let host = LuaHost::new(&ui);
    assert!(host.error.is_none(), "{:?}", host.error);
    render(&host, "watch");

    let plugin = &host.plugins[host.index_of("watch").expect("loaded")];
    assert!(
        !host.may(plugin, Capability::Program),
        "declared nothing, so it may not run a program"
    );
}

#[test]
fn a_declared_but_untrusted_plugin_still_may_not_run_a_program() {
    let (_home, ui) = interface(&[("91_watch.lua", &watch_pane("watch", "watch", true))]);
    let host = LuaHost::new(&ui);
    let plugin = &host.plugins[host.index_of("watch").expect("loaded")];
    assert_eq!(plugin.capabilities, vec![Capability::Program]);
    assert!(
        !host.may(plugin, Capability::Program),
        "declaring is asking, not being granted"
    );

    host.set_trusted(vec!["plugins/91_watch.lua".to_string()]);
    let plugin = &host.plugins[host.index_of("watch").expect("loaded")];
    assert!(host.may(plugin, Capability::Program));
}

// ── whose pane is it ───────────────────────────────────────────────────────

/// The surface a plugin draws resolves to *its own* pane, always.
///
/// The plugin writes a bare name and the kernel supplies the owner, so naming
/// another plugin's pane is impossible by construction rather than refused by a
/// check that could be forgotten — the lesson `run`'s implementation-in-globals
/// bug taught.
#[test]
fn two_plugins_asking_for_the_same_name_get_different_panes() {
    let (_home, ui) = interface(&[
        ("91_one.lua", &watch_pane("one", "watch", true)),
        ("92_two.lua", &watch_pane("two", "watch", true)),
    ]);
    let host = LuaHost::new(&ui);
    assert!(host.error.is_none(), "{:?}", host.error);

    let surface_of = |name: &str| -> String {
        let rendered = render(&host, name);
        rendered
            .node
            .first_live_surface()
            .unwrap_or_else(|| panic!("{name} drew no live surface"))
            .to_string()
    };
    let one = surface_of("one");
    let two = surface_of("two");

    assert_ne!(one, two, "the same pane name, two different panes");
    assert!(one.contains("91_one.lua"), "{one}");
    assert!(two.contains("92_two.lua"), "{two}");
    // And neither is mistakable for a session's terminal.
    assert_eq!(
        render(&host, "one").node.first_session_surface(),
        None,
        "a program is not a session"
    );
}

#[test]
fn a_pane_name_that_could_not_be_a_window_is_a_load_error_not_a_missing_pane() {
    // Caught while the tree is read, so the author gets a message naming the path
    // rather than a pane that silently never appears.
    let (_home, ui) = interface(&[("91_bad.lua", &watch_pane("bad", "watch#2", true))]);
    let host = LuaHost::new(&ui);
    let index = host.index_of("bad").expect("the plugin itself loads");
    let error = host
        .render(
            index,
            RenderContext {
                width: 40,
                height: 10,
                focused: true,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect_err("the tree must not convert");
    let text = format!("{error:?}");
    assert!(text.contains("program"), "{text}");
}

// ── what is drawn when nothing is behind it ────────────────────────────────

/// A surface with nothing started says so, rather than drawing an empty box.
#[test]
fn an_unstarted_program_surface_is_reported_as_such() {
    use thurbox::kernel::paint::{ProgramPaint, SurfaceProvider};
    let terminals = Terminals::new();
    let key = ProgramKey::new("plugins/91_watch.lua", "watch");

    let mut term =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(30, 6)).expect("terminal");
    let mut outcome = ProgramPaint::Painted;
    term.draw(|frame| {
        outcome = terminals.render_program(
            frame,
            ratatui::layout::Rect::new(0, 0, 30, 6),
            &key.surface_id(),
        );
    })
    .expect("draw");

    assert_eq!(
        outcome,
        ProgramPaint::NotStarted,
        "nothing was started, and that is a state to report"
    );
}

/// The whole tree paints, with the surface's placeholder inside it.
#[test]
fn a_program_pane_paints_its_placeholder_rather_than_nothing() {
    let (_home, ui) = interface(&[("91_watch.lua", &watch_pane("watch", "watch", true))]);
    let host = LuaHost::new(&ui);
    let rendered = render(&host, "watch");

    let mut term =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 8)).expect("terminal");
    term.draw(|frame| {
        thurbox::kernel::paint::render(
            frame,
            ratatui::layout::Rect::new(0, 0, 40, 8),
            &rendered.node,
            &thurbox::kernel::paint::PlaceholderSurfaces,
        );
    })
    .expect("draw");

    let painted: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(
        painted.contains("program surface") || painted.contains("nothing started"),
        "an empty box explains nothing: {painted:?}"
    );
}

// ── keys ───────────────────────────────────────────────────────────────────

/// A key aimed at a pane with nothing behind it must not be swallowed.
///
/// Silently eating it is the failure that reads as "the keyboard stopped working":
/// there is no error, no output, and no way to tell it from a program that ignored
/// the key.
#[test]
fn a_key_for_an_absent_program_is_not_reported_as_delivered() {
    let terminals = Terminals::new();
    let key = ProgramKey::new("plugins/91_watch.lua", "watch");
    assert!(
        !terminals.send_to_program(&key, b"q".to_vec()),
        "nothing is running, so nothing accepted it"
    );
}

#[test]
fn a_program_surface_is_never_resolved_to_a_session() {
    let terminals = Terminals::new();
    // A well-formed program id resolves to nothing while nothing is running, and a
    // session id never resolves to a program at all.
    let key = ProgramKey::new("plugins/91_watch.lua", "watch");
    assert!(terminals.program_key(&key.surface_id()).is_none());
    assert!(terminals
        .program_key("5f9c1f6e-1b2a-4c3d-8e9f-0a1b2c3d4e5f")
        .is_none());
    // And a program surface has no output stamp among the sessions'.
    assert!(terminals.output_stamp(&key.surface_id()).is_none());
}

// ── not a session ──────────────────────────────────────────────────────────

/// Nothing that enumerates sessions can see a plugin's pane.
///
/// It holds because a pane is never a database row — but it is asserted, because
/// the machinery underneath is shared with sessions and a leak would be a session
/// the user cannot delete, restart or explain.
#[test]
fn a_program_pane_is_absent_from_every_session_enumeration() {
    let terminals = Terminals::new();
    let key = ProgramKey::new("plugins/91_watch.lua", "watch");

    // Not attached, not failed, has no shell, and contributes no output generation.
    assert!(!terminals.is_attached(&key.surface_id()));
    assert!(terminals.failure(&key.surface_id()).is_none());
    assert!(!terminals.has_shell(&key.surface_id()));
    assert_eq!(terminals.output_generation(), 0);
    assert!(terminals.visible_text(&key.surface_id()).is_none());
    assert!(terminals.last_rect(&key.surface_id()).is_none());
}
