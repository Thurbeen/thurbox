use thurbox::kernel::bands::Level;
use thurbox::kernel::command::Command;
use thurbox::kernel::host::{KeyPress, LuaHost, RenderContext};

fn interface(mods: &[(&str, &str)]) -> (tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = home.path().join("ui");
    thurbox::kernel::bundled::materialize(&ui);
    for (name, source) in mods {
        std::fs::write(ui.join("mods").join(name), source).expect("write");
    }
    (home, ui)
}

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
            fallback: "mods/91_presser.lua".to_string(),
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
