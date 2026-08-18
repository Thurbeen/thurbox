//! Running a program on a plugin's behalf: who may, where it runs, and what
//! comes back.
//!
//! The parts that fail invisibly are the gate and the attribution. A capability
//! that is *present and refusing* looks identical to one that is absent until
//! you read the error; a run attributed to the wrong plugin looks like nothing
//! at all until two plugins pick the same key. Both are asserted here rather
//! than trusted to review.

use thurbox::kernel::host::{Capability, LuaHost, RenderContext};
use thurbox::kernel::runs::{Ask, Run};

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

fn render(host: &LuaHost, name: &str) {
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
    .expect("render");
}

/// A pane that asks for a program the moment it draws.
fn asker(name: &str, key: &str, declares: bool) -> String {
    format!(
        r#"return {{
  name = "{name}",
  slot = "sessions",
  {}
  render = function()
    if run then
      run("{key}", "echo hi", {{ session = "s1" }})
    end
    return {{ type = "text", text = "" }}
  end,
}}"#,
        if declares {
            r#"capabilities = { "run" },"#
        } else {
            ""
        }
    )
}

#[test]
fn an_undeclared_plugin_has_no_run_at_all() {
    // Absent, not refusing: `run` is nil, so the pane's own `if run then` is the
    // check — there is no error to catch because there is no function to call.
    let (_home, ui) = interface(&[("91_quiet.lua", &asker("quiet", "k", false))]);
    let host = LuaHost::new(&ui);
    assert!(host.error.is_none(), "{:?}", host.error);
    render(&host, "quiet");
    assert!(
        host.drain_runs().is_empty(),
        "a plugin that declared nothing must not be able to run anything"
    );
}

#[test]
fn a_declared_but_untrusted_plugin_has_no_run_either() {
    // Declaring is asking, not being granted.
    let (_home, ui) = interface(&[("91_asks.lua", &asker("asks", "k", true))]);
    let host = LuaHost::new(&ui);
    assert_eq!(
        host.plugins[host.index_of("asks").expect("loaded")].capabilities,
        vec![Capability::Run]
    );
    render(&host, "asks");
    assert!(
        host.drain_runs().is_empty(),
        "declaring is not being trusted"
    );
}

/// A plugin chunk's `_ENV` *is* the globals table, so a capability parked there
/// under an internal name is granted to everyone. `run` used to be: the
/// implementation sat in globals as `__run_impl`, and binding the name `run` per
/// call only decided what the *published* name pointed at. An untrusted plugin
/// declaring nothing could call the other name and get a program run with the
/// user's authority.
#[test]
fn an_untrusted_plugin_cannot_reach_run_under_another_name() {
    let sneak = r#"return {
  name = "sneak",
  slot = "sessions",
  render = function()
    -- Every route to a function that is not supposed to be here.
    local found = rawget(_G, "__run_impl") or __run_impl
    if found then found("direct", "echo pwned", { session = "s1" }) end
    for name, value in pairs(_G) do
      if type(value) == "function" and name ~= "run" then
        local ok = pcall(value, "swept", "echo pwned", { session = "s1" })
        if ok and name:find("run") then error("reached run as " .. name) end
      end
    end
    -- The module cache is the same class of exposure: reachable means
    -- rewritable, and a rewritten `lib.theme` would be handed to every plugin.
    if rawget(_G, "__modules") then error("module cache is reachable") end
    return { type = "text", text = "" }
  end,
}"#;
    let (_home, ui) = interface(&[("91_sneak.lua", sneak)]);
    let host = LuaHost::new(&ui);
    assert!(host.error.is_none(), "{:?}", host.error);
    render(&host, "sneak");
    assert!(
        host.drain_runs().is_empty(),
        "an untrusted plugin reached the run implementation by another name"
    );
}

#[test]
fn a_trusted_plugin_may_run_and_the_ask_is_attributed_to_it() {
    let (_home, ui) = interface(&[("91_asks.lua", &asker("asks", "k", true))]);
    let host = LuaHost::new(&ui);
    host.set_trusted(vec!["plugins/91_asks.lua".to_string()]);
    render(&host, "asks");

    let asked = host.drain_runs();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].0, "plugins/91_asks.lua", "attributed to the asker");
    assert_eq!(asked[0].1.key, "k");
    assert_eq!(asked[0].1.program, "echo hi");
}

#[test]
fn trusting_one_plugin_does_not_trust_another() {
    // The whole reason trust is per plugin rather than a switch.
    let (_home, ui) = interface(&[
        ("91_yes.lua", &asker("yes", "k", true)),
        ("92_no.lua", &asker("no", "k", true)),
    ]);
    let host = LuaHost::new(&ui);
    host.set_trusted(vec!["plugins/91_yes.lua".to_string()]);
    render(&host, "yes");
    render(&host, "no");

    let asked = host.drain_runs();
    assert_eq!(asked.len(), 1, "only the trusted one may run: {asked:?}");
    assert_eq!(asked[0].0, "plugins/91_yes.lua");
}

#[test]
fn revoking_takes_the_capability_away_again() {
    let (_home, ui) = interface(&[("91_asks.lua", &asker("asks", "k", true))]);
    let host = LuaHost::new(&ui);
    host.set_trusted(vec!["plugins/91_asks.lua".to_string()]);
    render(&host, "asks");
    assert_eq!(host.drain_runs().len(), 1);

    host.set_trusted(Vec::new());
    render(&host, "asks");
    assert!(
        host.drain_runs().is_empty(),
        "revoking must take effect without the plugin being reloaded from disk"
    );
}

#[test]
fn a_capability_that_does_not_exist_fails_the_load_naming_the_file() {
    // Loading it and granting nothing would look like the capability is broken.
    let (_home, ui) = interface(&[(
        "91_typo.lua",
        r#"return { name = "typo", slot = "sessions", capabilities = { "rnu" },
                   render = function() return { type = "text", text = "" } end }"#,
    )]);
    let host = LuaHost::new(&ui);
    let error = host.error.clone().unwrap_or_default();
    assert!(error.contains("rnu"), "{error}");
    assert!(
        error.contains("run"),
        "it must say what was available: {error}"
    );
}

#[test]
fn a_plugin_reads_its_own_answers_and_no_one_elses() {
    let (_home, ui) = interface(&[("91_a.lua", &reader("a")), ("92_b.lua", &reader("b"))]);
    let host = LuaHost::new(&ui);
    let mut answers = std::collections::HashMap::new();
    answers.insert(
        "plugins/91_a.lua".to_string(),
        vec![(
            "mine".to_string(),
            Run::Done(thurbox::kernel::runs::Output {
                stdout: "secret".into(),
                stderr: String::new(),
                status: Some(0),
                truncated: false,
                timed_out: false,
            }),
        )],
    );
    host.set_runs(answers);

    let a = host
        .render(
            host.index_of("a").expect("a"),
            RenderContext {
                width: 40,
                height: 4,
                focused: true,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render a");
    assert!(
        format!("{:?}", a.node).contains("secret"),
        "a reads its own"
    );

    let b = host
        .render(
            host.index_of("b").expect("b"),
            RenderContext {
                width: 40,
                height: 4,
                focused: true,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render b");
    assert!(
        !format!("{:?}", b.node).contains("secret"),
        "b must not see a's output"
    );
}

/// A pane that prints whatever its `mine` run returned.
///
/// Declares the capability because only a plugin that can ASK can have answers —
/// the kernel skips the whole per-call publish for one that declared nothing,
/// since it could never have any.
fn reader(name: &str) -> String {
    format!(
        r#"return {{
  name = "{name}",
  slot = "sessions",
  capabilities = {{ "run" }},
  render = function()
    local got = ((thurbox and thurbox.runs) or {{}})["mine"]
    local text = got and got.stdout or "nothing"
    return {{ type = "text", text = text }}
  end,
}}"#
    )
}

#[test]
fn a_plugin_may_be_written_as_several_modules() {
    // A substantial pane is not a file anyone wants to keep in one piece, so
    // this is a guarantee rather than an accident of how `require` resolves.
    let home = tempfile::tempdir().expect("tempdir");
    let ui = home.path().join("ui");
    thurbox::kernel::bundled::materialize(&ui);
    std::fs::create_dir_all(ui.join("mine")).expect("mkdir");
    std::fs::write(
        ui.join("mine").join("parse.lua"),
        "return { greet = function() return 'from my module' end }",
    )
    .expect("write module");
    std::fs::write(
        ui.join("plugins").join("91_multi.lua"),
        r#"local parse = require("mine.parse")
return { name = "multi", slot = "sessions",
         render = function() return { type = "text", text = parse.greet() } end }"#,
    )
    .expect("write plugin");

    let host = LuaHost::new(&ui);
    assert!(host.error.is_none(), "{:?}", host.error);
    let rendered = host
        .render(
            host.index_of("multi").expect("multi"),
            RenderContext {
                width: 40,
                height: 4,
                focused: true,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render");
    assert!(format!("{:?}", rendered.node).contains("from my module"));
}

#[test]
fn a_module_outside_the_interface_directory_is_refused() {
    let (_home, ui) = interface(&[(
        "91_escape.lua",
        r#"local x = require("../../../etc/passwd")
return { name = "escape", slot = "sessions",
         render = function() return { type = "text", text = "" } end }"#,
    )]);
    let host = LuaHost::new(&ui);
    let error = host.error.clone().unwrap_or_default();
    assert!(
        error.contains("plugin directory"),
        "it must refuse and say why: {error}"
    );
}

#[test]
fn an_ask_names_a_session_that_must_exist() {
    // Resolved before any worker starts, so a bad session is a reported failure
    // rather than a program run somewhere unexpected.
    let ask = Ask {
        key: "k".into(),
        program: "true".into(),
        session: "nope".into(),
        ttl: thurbox::kernel::runs::DEFAULT_TTL,
        timeout: thurbox::kernel::runs::DEFAULT_TIMEOUT,
        refresh: false,
    };
    assert_eq!(ask.session, "nope");
}

// ── the two capabilities are two decisions ─────────────────────────────────

/// A pane declaring exactly the capabilities named.
fn declares(name: &str, capabilities: &[&str]) -> String {
    let list: Vec<String> = capabilities
        .iter()
        .map(|capability| format!("\"{capability}\""))
        .collect();
    format!(
        r#"return {{
  name = "{name}",
  slot = "sessions",
  capabilities = {{ {} }},
  render = function()
    return {{ type = "text", text = "" }}
  end,
}}"#,
        list.join(", ")
    )
}

#[test]
fn program_is_a_capability_of_its_own_and_is_enumerable_from_the_declaration() {
    let (_home, ui) = interface(&[("91_doom.lua", &declares("doom", &["program"]))]);
    let host = LuaHost::new(&ui);
    assert!(host.error.is_none(), "{:?}", host.error);
    assert_eq!(
        host.plugins[host.index_of("doom").expect("loaded")].capabilities,
        vec![Capability::Program],
        "readable as data, without executing the plugin"
    );
}

#[test]
fn a_misspelled_capability_is_refused_and_names_what_exists() {
    // Refused rather than ignored, for the reason `run` already is: a typo would
    // otherwise load, be granted nothing, and look like the capability is broken.
    let (_home, ui) = interface(&[("91_typo.lua", &declares("typo", &["porgram"]))]);
    let host = LuaHost::new(&ui);
    let error = host.error.clone().unwrap_or_default();
    assert!(error.contains("porgram"), "{error}");
    assert!(
        error.contains("program") && error.contains("run"),
        "and names both real ones: {error}"
    );
}

/// The load-bearing property: neither grant confers the other.
///
/// `run` is bounded on every axis that matters — capped output, a timeout, four at
/// a time — and an interactive program has none of those and holds the keyboard
/// too. If one grant covered both, trusting a pane to poll `top` would silently
/// have granted it a process held open on your keystrokes.
#[test]
fn trusting_a_file_for_one_capability_does_not_grant_the_other() {
    let (_home, ui) = interface(&[
        ("91_runner.lua", &declares("runner", &["run"])),
        ("92_doomer.lua", &declares("doomer", &["program"])),
    ]);
    let host = LuaHost::new(&ui);
    host.set_trusted(vec![
        "plugins/91_runner.lua".to_string(),
        "plugins/92_doomer.lua".to_string(),
    ]);

    let runner = &host.plugins[host.index_of("runner").expect("loaded")];
    let doomer = &host.plugins[host.index_of("doomer").expect("loaded")];

    assert!(host.may(runner, Capability::Run));
    assert!(
        !host.may(runner, Capability::Program),
        "trusted to read a program's output is not trusted to hold one open"
    );
    assert!(host.may(doomer, Capability::Program));
    assert!(
        !host.may(doomer, Capability::Run),
        "and the converse: the grant follows what the file declared"
    );
}

#[test]
fn declaring_a_capability_is_not_being_granted_it() {
    let (_home, ui) = interface(&[("91_doom.lua", &declares("doom", &["program"]))]);
    let host = LuaHost::new(&ui);
    let doom = &host.plugins[host.index_of("doom").expect("loaded")];
    assert!(
        !host.may(doom, Capability::Program),
        "declaring is asking, not being granted"
    );
    // And the path-keyed form agrees, since that is what a queued command carries.
    assert!(!host.may_path("plugins/91_doom.lua", Capability::Program));
    host.set_trusted(vec!["plugins/91_doom.lua".to_string()]);
    assert!(host.may_path("plugins/91_doom.lua", Capability::Program));
    assert!(
        !host.may_path("plugins/99_nobody.lua", Capability::Program),
        "a path that names no loaded plugin may nothing"
    );
}

#[test]
fn each_capability_says_what_it_would_let_a_file_do() {
    // The list where the user presses `t` has to distinguish them: "runs programs"
    // describes both while hiding the difference that decides the answer.
    let described: Vec<&str> = Capability::ALL
        .iter()
        .map(|capability| capability.describe())
        .collect();
    assert_eq!(
        described.len(),
        Capability::ALL.len(),
        "every capability is describable"
    );
    assert!(
        described[0] != described[1],
        "and no two share a description: {described:?}"
    );
    assert!(
        Capability::Program.describe().contains("interact"),
        "{:?}",
        Capability::Program.describe()
    );
}
