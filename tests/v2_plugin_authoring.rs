//! Writing a plugin without a terminal.
//!
//! Each test is one of the questions a session asks before its first save — where
//! do I write, what do I start from, will it load — answered the way a script or
//! an agent would ask it: through the command, reading its output and its exit.
//!
//! `THURBOX_UI_DIR` is set per test. nextest runs a process per test, so the
//! override cannot leak between them.

use thurbox::cli::plugins::{run, Action};

/// Point the interface directory at a fresh tempdir and return it.
fn at(dir: &std::path::Path) -> std::path::PathBuf {
    let ui = dir.join("ui");
    std::fs::create_dir_all(&ui).expect("mkdir");
    std::env::set_var("THURBOX_UI_DIR", &ui);
    ui
}

fn json(action: Action) -> serde_json::Value {
    run(action).expect("command").json
}

/// The bundled interface, as the repo ships it.
fn checkout_ui() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui")
}

// ── where do I write ───────────────────────────────────────────────────────

#[test]
fn the_directory_report_names_the_rule_that_chose_it() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());

    let report = json(Action::Dir);
    assert_eq!(report["dir"], ui.display().to_string());
    assert_eq!(report["chosen"], "override");
    assert_eq!(
        report["plugins_dir"],
        ui.join("plugins").display().to_string(),
        "the directory a plugin file actually goes in is spelled out"
    );
    assert!(
        report["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("THURBOX_UI_DIR"),
        "{report}"
    );
}

#[test]
fn a_checkout_is_told_apart_from_the_users_own_copy() {
    // The mistake this exists to prevent: editing `~/.config/thurbox/ui` while a
    // `./ui` in the working directory is what the interface actually loads.
    std::env::remove_var("THURBOX_UI_DIR");
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::env::set_current_dir(&repo).expect("cd");

    let report = json(Action::Dir);
    assert_eq!(report["chosen"], "checkout", "{report}");
    // Absolute, not the bare `ui` it is found by. Trust, the disabled set and
    // every rebinding are keyed by this directory joined with a filename and
    // compared verbatim, so a relative one is not an identity: every checkout on
    // the machine shares it, and trusting a plugin in one worktree would grant
    // `run` to a same-named file in another.
    assert_eq!(
        report["dir"],
        repo.join("ui").display().to_string(),
        "{report}"
    );
}

#[test]
fn asking_where_plugins_live_writes_nothing() {
    // A read must not deliver an interface as a side effect of being asked: only
    // `new` and the TUI itself write.
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());

    json(Action::Dir);
    let entries: Vec<_> = std::fs::read_dir(&ui)
        .expect("read")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect();
    assert!(entries.is_empty(), "asking wrote {entries:?} into {ui:?}");
}

#[test]
fn an_override_that_is_not_there_is_an_error_naming_it() {
    // Falling back silently would send the plugin somewhere the user did not ask
    // for; the TUI refuses to start for the same reason.
    let home = tempfile::tempdir().expect("tempdir");
    let missing = home.path().join("nowhere");
    std::env::set_var("THURBOX_UI_DIR", &missing);
    let error = run(Action::Dir).expect_err("must refuse");
    assert!(error.contains("THURBOX_UI_DIR"), "{error}");
    assert!(error.contains("nowhere"), "{error}");
}

// ── what do I start from ───────────────────────────────────────────────────

#[test]
fn a_new_plugin_loads_before_it_is_edited() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());

    let created = json(Action::New {
        name: "notes".into(),
    });
    let file = created["created"].as_str().expect("a path").to_string();
    assert!(std::path::Path::new(&file).is_file(), "{file}");
    assert!(
        created["delivered_interface"] == true,
        "an empty directory is made into an interface first, or the starter's \
         `require` would fail: {created}"
    );

    // The proof that matters: the kernel accepts it.
    let checked = json(Action::Check);
    assert_eq!(checked["ok"], true, "{checked}");
    let loaded: Vec<&str> = checked["loaded"]
        .as_array()
        .expect("loaded")
        .iter()
        .filter_map(|name| name.as_str())
        .collect();
    assert!(loaded.contains(&"notes"), "{loaded:?}");
    let _ = ui;
}

#[test]
fn a_new_plugin_answers_to_the_name_it_was_given() {
    let home = tempfile::tempdir().expect("tempdir");
    at(home.path());
    let created = json(Action::New {
        name: "notes".into(),
    });
    let body = std::fs::read_to_string(created["created"].as_str().expect("path")).expect("read");
    assert!(body.contains("name = \"notes\""), "{body}");
    assert!(
        !body.contains("example"),
        "no trace of the example's own name is left in it"
    );
}

#[test]
fn creating_over_an_existing_plugin_is_refused() {
    let home = tempfile::tempdir().expect("tempdir");
    at(home.path());
    run(Action::New {
        name: "notes".into(),
    })
    .expect("first");
    let error = run(Action::New {
        name: "notes".into(),
    })
    .expect_err("second must refuse");
    assert!(error.contains("already exists"), "{error}");
}

#[test]
fn a_name_that_would_escape_the_directory_is_refused() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    for bad in ["../outside", "nested/name"] {
        let error = run(Action::New { name: bad.into() }).expect_err("must refuse");
        assert!(!error.is_empty(), "{bad}");
    }
    assert!(
        !home.path().join("outside.lua").exists(),
        "nothing was written outside {}",
        ui.display()
    );
}

// ── will it load ───────────────────────────────────────────────────────────

#[test]
fn the_shipped_interface_checks_out() {
    std::env::set_var("THURBOX_UI_DIR", checkout_ui());
    let checked = json(Action::Check);
    assert_eq!(checked["ok"], true, "{checked}");
    // Every bundled pane, by name — this is also what catches a bundled file that
    // stopped loading.
    let loaded = checked["loaded"].to_string();
    for pane in ["sessions", "agent", "new_session", "confirm", "search"] {
        assert!(loaded.contains(pane), "{pane} missing from {loaded}");
    }
}

#[test]
fn a_broken_plugin_is_named_with_its_reason_and_fails_the_exit() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    run(Action::New {
        name: "notes".into(),
    })
    .expect("new");
    std::fs::write(ui.join("plugins").join("90_notes.lua"), "return { name =").expect("break it");

    let output = run(Action::Check).expect("check runs");
    assert!(
        output.failure.is_some(),
        "a broken interface has to fail the exit status, or a script cannot gate on it"
    );
    let error = output.json["error"].as_str().unwrap_or_default();
    assert!(error.contains("90_notes"), "names the file: {error}");
    assert!(
        error.contains("syntax") || error.contains("expected"),
        "and the reason: {error}"
    );
}

#[test]
fn an_interface_with_no_panes_is_reported_not_failed() {
    // A user who removed every pane has a working interface with nothing in it —
    // v2 supports that deliberately, so the check must not call it broken.
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    std::fs::create_dir_all(ui.join("plugins")).expect("mkdir");
    // `layout.lua` and `lib/` are what the host needs; no plugins at all.
    thurbox::kernel::bundled::materialize(&ui);
    for entry in std::fs::read_dir(ui.join("plugins")).expect("read plugins") {
        std::fs::remove_file(entry.expect("entry").path()).expect("remove");
    }

    let output = run(Action::Check).expect("check runs");
    assert!(output.failure.is_none(), "{:?}", output.json);
    assert_eq!(output.json["ok"], true);
    assert_eq!(
        output.json["loaded"].as_array().map(Vec::len),
        Some(0),
        "{:?}",
        output.json
    );
}

// ── what is loaded, and where it came from ─────────────────────────────────

#[test]
fn the_listing_tells_a_shipped_file_from_an_edited_one() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);

    let before = json(Action::List);
    let files = before["files"].to_string();
    assert!(files.contains("plugins/10_sessions.lua"), "{files}");
    assert!(files.contains("\"source\":\"bundled\""), "{files}");

    // Editing a shipped file changes where it is reported as coming from, which
    // is what makes "my change did nothing" diagnosable.
    let sessions = ui.join("plugins").join("10_sessions.lua");
    let body = std::fs::read_to_string(&sessions).expect("read");
    std::fs::write(&sessions, format!("-- mine\n{body}")).expect("write");

    let after = json(Action::List);
    let row = after["files"]
        .as_array()
        .expect("files")
        .iter()
        .find(|row| row["file"] == "plugins/10_sessions.lua")
        .expect("the edited file")
        .clone();
    assert_eq!(row["source"], "edited", "{row}");
}

// ── the example ────────────────────────────────────────────────────────────

#[test]
fn the_documented_example_is_a_plugin_that_loads() {
    // It is what `plugin new` writes and what the guide shows, so it cannot be
    // allowed to rot: this builds an interface out of it and renders it.
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);
    let example = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("examples")
        .join("plugin.lua");
    std::fs::copy(&example, ui.join("plugins").join("90_example.lua")).expect("copy");

    let host = thurbox::kernel::host::LuaHost::new(&ui);
    assert!(host.error.is_none(), "{:?}", host.error);
    let index = host.index_of("example").expect("the example did not load");

    let rendered = host
        .render(
            index,
            thurbox::kernel::host::RenderContext {
                width: 40,
                height: 10,
                focused: true,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render");
    // And it does something with what it was given, rather than drawing a stub.
    assert!(
        format!("{:?}", rendered.node).contains("session(s)"),
        "the example must render what it reads"
    );
}

#[test]
fn the_composite_example_loads_and_declares_what_it_needs() {
    // The proof the proposal owes: a pane over more than one program, parsed
    // rather than echoed, built from the published primitives. If it stops
    // loading, the claim that a complex pane is writable stops being true.
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);
    let example = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("examples")
        .join("composite.lua");
    std::fs::copy(&example, ui.join("plugins").join("95_composite.lua")).expect("copy");

    let host = thurbox::kernel::host::LuaHost::new(&ui);
    assert!(host.error.is_none(), "{:?}", host.error);
    let index = host
        .index_of("composite")
        .expect("the example did not load");
    assert_eq!(
        host.plugins[index].capabilities,
        vec![thurbox::kernel::host::Capability::Run],
        "it must declare what it needs, or it would be granted nothing silently"
    );
}

#[test]
fn an_untrusted_composite_draws_how_to_trust_it_rather_than_failing() {
    // The state every user of a capability-using plugin meets first. A pane that
    // looks broken before you have done anything is a plugin nobody keeps.
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);
    let example = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("examples")
        .join("composite.lua");
    std::fs::copy(&example, ui.join("plugins").join("95_composite.lua")).expect("copy");

    let host = thurbox::kernel::host::LuaHost::new(&ui);
    // Nothing trusted, which is the default: `run` is absent, not refusing.
    let index = host.index_of("composite").expect("loaded");
    let rendered = host
        .render(
            index,
            thurbox::kernel::host::RenderContext {
                width: 50,
                height: 12,
                focused: true,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("an untrusted plugin must still render");
    let drawn = format!("{:?}", rendered.node);
    assert!(
        drawn.contains("trust it"),
        "it must say how to grant it: {drawn}"
    );
}
