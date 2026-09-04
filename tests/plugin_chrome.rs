//! What a pane may say, and what it may set off, outside its own rect.
//!
//! Two gaps this closes, both recorded in the panes themselves. The creation
//! flow states one of them in a comment — "a plugin cannot write the message
//! band (it is kernel chrome), so a refusal this flow makes itself is shown on
//! its own footer row instead" — and pays a row of every modal for it. The
//! other is quieter: the kernel's own modals are reachable from a `role =
//! "action:…"` node and therefore only from the *mouse*, so a pane could not
//! open help from a key handler at all.
//!
//! Both are commands rather than globals, for the reason every write is one: a
//! plugin says what it wants and the loop does it on a later frame. The
//! coordinator half — the band actually showing the text, the modal actually
//! opening — is asserted on a real terminal in `tests/tui_e2e.rs`; what is
//! pinned here is the vocabulary, and that a mistake in it is refused rather
//! than swallowed.

use thurbox::kernel::bands::Level;
use thurbox::kernel::command::Command;
use thurbox::kernel::host::{KeyPress, LuaHost, RenderContext};

/// An interface of exactly the bundled `lib/` plus the panes given, so a test
/// pane may `require` the component layer without the bundled panes' snapshot
/// needs.
fn interface(plugins: &[(&str, &str)]) -> (tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = home.path().join("ui");
    thurbox::kernel::bundled::materialize(&ui);
    for (name, source) in plugins {
        std::fs::write(ui.join("plugins").join(name), source).expect("write");
    }
    (home, ui)
}

/// A pane whose only job is to run `body` when `x` is pressed.
fn presser(body: &str) -> String {
    format!(
        r#"return {{
  name = "presser",
  slot = "sessions",
  render = function()
    return {{ type = "text", text = "" }}
  end,
  on_key = function(key)
    if key.key == "x" then
      {body}
      return true
    end
    return false
  end,
}}"#
    )
}

fn press_x(host: &LuaHost) -> Result<Vec<Command>, String> {
    let index = host.index_of("presser").expect("no presser");
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
    .expect("render");
    let key = KeyPress {
        name: "x".to_string(),
        ch: Some('x'),
        ..KeyPress::default()
    };
    match host.on_key(index, &key) {
        Ok(_) => Ok(host.drain_commands()),
        Err(e) => Err(e.message),
    }
}

fn issued(body: &str) -> Result<Vec<Command>, String> {
    let (_home, ui) = interface(&[("91_presser.lua", &presser(body))]);
    let host = LuaHost::new(&ui);
    assert!(host.error.is_none(), "{:?}", host.error);
    press_x(&host)
}

#[test]
fn a_pane_can_run_a_kernel_action_from_a_key_handler() {
    let commands = issued(r#"command("action", { text = "help.open" })"#).expect("key");
    assert_eq!(
        commands,
        vec![Command::Action {
            // Stamped from the plugin executing, never read from the options
            // table — the same rule `program` and `emit` follow. It is the
            // fallback owner a click carries, so an action nothing declares
            // still reaches the pane that asked for it.
            owner: "plugins/91_presser.lua".to_string(),
            action: "help.open".to_string(),
        }]
    );
}

#[test]
fn a_pane_can_say_something_in_the_message_band() {
    let commands = issued(r#"command("message", { text = "nothing to undo" })"#).expect("key");
    assert_eq!(
        commands,
        vec![Command::Message {
            text: "nothing to undo".to_string(),
            level: Level::Info,
        }]
    );

    let commands =
        issued(r#"command("message", { text = "gone", level = "error" })"#).expect("key");
    assert_eq!(
        commands,
        vec![Command::Message {
            text: "gone".to_string(),
            level: Level::Error,
        }]
    );
}

/// The trap the `command` option list already documents: a name no verb reads
/// is collected and ignored, so a verb whose required field is misspelt has to
/// refuse rather than enqueue a no-op.
#[test]
fn a_message_with_no_text_and_a_level_nobody_badges_are_both_refused() {
    let error = issued(r#"command("message", { message = "oops" })"#).expect_err("refused");
    assert!(error.contains("text"), "{error}");

    let error = issued(r#"command("message", { text = "oops", level = "critical" })"#)
        .expect_err("refused");
    assert!(error.contains("critical"), "{error}");
}

#[test]
fn an_action_with_no_action_is_refused() {
    let error = issued(r#"command("action", { action = "help.open" })"#).expect_err("refused");
    assert!(error.contains("text"), "{error}");
}
