//! The right button reaches `on_context`, and nothing else.
//!
//! The property worth guarding is the one that makes this safe to add at all:
//! a right press is SILENT in a pane that has not been taught it. Every
//! `on_click` ever written reads "act on this row" — open the file, run the
//! action — so a right press delivered there would do exactly that, everywhere,
//! the moment the kernel began forwarding it. The two hooks must therefore stay
//! strictly separate, which is what these assert.

use thurbox::kernel::host::{Click, LuaHost};

/// A pane that answers both presses and says which it got.
const TWO_HANDED: &str = r#"
return {
  name = "twohanded",
  slot = "sessions",
  order = 10,
  render = function()
    return { type = "text", text = state.said or "" }
  end,
  on_click = function(hit)
    state.said = "left:" .. tostring(hit.id)
    return true
  end,
  on_context = function(hit)
    state.said = "right:" .. tostring(hit.id)
    return true
  end,
}
"#;

/// A pane that was written before the right button existed.
const LEFT_ONLY: &str = r#"
return {
  name = "leftonly",
  slot = "center",
  order = 20,
  render = function()
    return { type = "text", text = state.said or "" }
  end,
  on_click = function(hit)
    state.said = "left:" .. tostring(hit.id)
    return true
  end,
}
"#;

fn host_with(plugins: &[(&str, &str)]) -> (tempfile::TempDir, LuaHost) {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = home.path().join("ui");
    std::fs::create_dir_all(ui.join("plugins")).expect("mkdir");
    for (file, body) in plugins {
        std::fs::write(ui.join("plugins").join(file), body).expect("write plugin");
    }
    let host = LuaHost::new(ui);
    assert!(host.error.is_none(), "{:?}", host.error);
    (home, host)
}

fn index_of(host: &LuaHost, name: &str) -> usize {
    host.plugins
        .iter()
        .position(|p| p.name == name)
        .unwrap_or_else(|| panic!("{name} should have loaded"))
}

fn on(id: &str) -> Click {
    Click {
        id: Some(id.to_string()),
        role: Some("row".into()),
        w: 20,
        h: 1,
        ..Click::default()
    }
}

/// What `state` the pane ended up with, read back the way the kernel would see
/// it — through a render, not by reaching into Lua.
fn said(host: &LuaHost, index: usize) -> String {
    let ctx = thurbox::kernel::host::RenderContext {
        width: 20,
        height: 4,
        focused: true,
        elapsed: 0.0,
        frame: 0,
    };
    let node = host.render(index, ctx).expect("render").node;
    format!("{node:?}")
}

#[test]
fn each_button_reaches_its_own_hook() {
    let (_home, host) = host_with(&[("10_twohanded.lua", TWO_HANDED)]);
    let index = index_of(&host, "twohanded");

    assert!(
        host.on_context(index, &on("a")).expect("context"),
        "handled"
    );
    assert!(
        said(&host, index).contains("right:a"),
        "a right press must reach on_context"
    );

    assert!(host.on_click(index, &on("b")).expect("click"), "handled");
    assert!(
        said(&host, index).contains("left:b"),
        "a left press must still reach on_click"
    );
}

/// The safety property: a pane written before this existed cannot be made to
/// act by a button it never asked for.
#[test]
fn a_pane_with_only_on_click_never_hears_a_right_press() {
    let (_home, host) = host_with(&[("20_leftonly.lua", LEFT_ONLY)]);
    let index = index_of(&host, "leftonly");

    assert!(
        !host
            .on_context(index, &on("a"))
            .expect("no handler is not an error"),
        "declining is the answer, not an error panel"
    );
    assert!(
        !said(&host, index).contains("left:"),
        "and on_click must not have run: {}",
        said(&host, index)
    );
}
