//! Writing a mod without a terminal.
//!
//! Each test is one of the questions a session asks before its first save — where
//! do I write, what do I start from, will it load — answered the way a script or
//! an agent would ask it: through the command, reading its output and its exit.
//!
//! `THURBOX_UI_DIR` is set per test. nextest runs a process per test, so the
//! override cannot leak between them.

use thurbox::cli::mods::{run, Action};

fn at(dir: &std::path::Path) -> std::path::PathBuf {
    let ui = dir.join("ui");
    std::fs::create_dir_all(&ui).expect("mkdir");
    std::env::set_var("THURBOX_CONFIG_DIR", dir);
    std::env::set_var("THURBOX_UI_DIR", &ui);
    ui
}

fn json(action: Action) -> serde_json::Value {
    run(action).expect("command").json
}

/// The bundled interface, as the repo ships it.
fn checkout_ui() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ui")
}

#[test]
fn shipped_menu_and_click_actions_are_in_the_catalog() {
    let home = tempfile::tempdir().expect("tempdir");
    at(home.path());
    std::env::set_var("THURBOX_UI_DIR", checkout_ui());
    let report = json(Action::Check);
    assert_eq!(report["ok"], true, "{report}");
    for warning in report["warnings"].as_array().expect("warnings") {
        let message = warning["warning"].as_str().unwrap_or_default();
        assert!(
            !message.contains("used by a menu or clickable node but has no catalog action"),
            "{warning}"
        );
    }
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
        ui.join("mods").display().to_string(),
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
fn standing_in_a_checkout_does_not_silently_load_its_interface() {
    // There used to be an automatic rule: a `./ui` beside the working directory
    // won. It made the interface the ONE config that ignored the
    // `thurbox`/`thurbox-dev` split — `cargo run` in the repository read
    // `~/.config/thurbox-dev` for agents, settings, themes and the database, and
    // the checkout for its panes, with nothing on screen saying which. Editing a
    // checkout's interface is an explicit `THURBOX_UI_DIR` now.
    std::env::remove_var("THURBOX_UI_DIR");
    // Point the CONFIG dir at a tempdir, not `THURBOX_UI_DIR` — the whole claim is
    // that the user's copy is what resolves, so the override must stay unset.
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("THURBOX_CONFIG_DIR", home.path());
    let repo = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    std::env::set_current_dir(&repo).expect("cd");
    assert!(
        repo.join("ui").is_dir(),
        "the checkout really does have one"
    );

    let report = json(Action::Dir);
    std::env::remove_var("THURBOX_CONFIG_DIR");
    assert_eq!(
        report["chosen"], "user-copy",
        "standing in the repository must not change which interface loads: {report}"
    );
    let dir = report["dir"].as_str().unwrap_or_default();
    assert!(
        dir.starts_with(&home.path().display().to_string()),
        "it resolves under the config dir, not the checkout: {report}"
    );
    assert!(
        !dir.starts_with(&repo.display().to_string()),
        "emphatically not the checkout: {report}"
    );
}

#[test]
fn an_override_at_a_checkout_is_reported_as_the_checkout() {
    // Asking for it explicitly is honoured, and named for what a reader
    // recognises rather than for the mechanism that delivered it.
    let repo = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    std::env::set_current_dir(&repo).expect("cd");
    std::env::set_var("THURBOX_UI_DIR", repo.join("ui"));

    let report = json(Action::Dir);
    assert_eq!(report["chosen"], "checkout", "{report}");
    assert_eq!(
        report["dir"],
        repo.join("ui").display().to_string(),
        "{report}"
    );
    std::env::remove_var("THURBOX_UI_DIR");
}

#[test]
fn asking_where_mods_live_writes_nothing() {
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
    let home = tempfile::tempdir().expect("tempdir");
    let missing = home.path().join("nowhere");
    std::env::set_var("THURBOX_UI_DIR", &missing);
    let error = run(Action::Dir).expect_err("must refuse");
    assert!(error.contains("THURBOX_UI_DIR"), "{error}");
    assert!(error.contains("nowhere"), "{error}");
}

// ── what do I start from ───────────────────────────────────────────────────

#[test]
fn a_new_mod_loads_before_it_is_edited() {
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
    assert!(
        !checked["warnings"]
            .to_string()
            .contains("no catalog action"),
        "shipped menu and clickable actions must be catalogued: {checked}"
    );
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
fn a_new_mod_answers_to_the_name_it_was_given() {
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
fn creating_over_an_existing_mod_is_refused() {
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
    // `search` is the reason the unplaced check has to open every panel before it
    // resolves the arrangement: the strip starts CLOSED, so `layout.lua`
    // legitimately names no `search` slot until something opens it. Without that,
    // the interface we ship would fail its own check.
    assert!(loaded.contains("search"), "{checked}");
    assert!(
        !checked["checked_at"].is_null(),
        "the size the verdict was reached at is reported, so a surprising one is \
         explicable: {checked}"
    );
}

#[test]
fn a_pane_nothing_places_is_a_failure_that_says_what_to_add() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    run(Action::New {
        name: "notes".into(),
    })
    .expect("new");

    // The starter takes `center`, which the arrangement always places. Moving it
    // to a slot nothing names is the whole difference between drawing and not.
    let file = ui.join("mods").join("90_notes.lua");
    let body = std::fs::read_to_string(&file).expect("read");
    std::fs::write(&file, body.replace("slot = \"center\"", "slot = \"notes\"")).expect("write");

    let output = run(Action::Check).expect("check runs");
    assert!(
        output.failure.is_some(),
        "a pane that draws nothing has to fail the exit, or CI cannot gate on it: {:?}",
        output.json
    );
    let unplaced = output.json["unplaced"].to_string();
    assert!(
        unplaced.contains("90_notes.lua"),
        "names the file: {unplaced}"
    );
    assert!(unplaced.contains("notes"), "and the slot: {unplaced}");
    let human = output.human.clone();
    assert!(
        human.contains("slot = \"notes\""),
        "and prints the line to add to layout.lua, since knowing the slot is not \
         the same as knowing the fix: {human}"
    );
}

/// The listing must not call a working pane unplaced.
///
/// `unplaced` means "its slot appears nowhere in the arrangement" — a defect with
/// no symptom, which is why `check` fails on it. The listing resolved placement
/// with an empty set on the reasoning that the arrangement needs a terminal, so
/// every pane in the shipped interface was reported as that defect. Placement is
/// knowable without a frame (`check` resolves it at a reference size); what needs
/// one is which occupant of a slot is in *front*.
#[test]
fn the_listing_does_not_report_working_panes_as_unplaced() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);

    let state_of = |listing: &serde_json::Value, name: &str| -> String {
        listing["files"]
            .as_array()
            .expect("files")
            .iter()
            .find(|row| row["name"] == name)
            .map(|row| row["state"].as_str().unwrap_or_default().to_string())
            .unwrap_or_else(|| panic!("{name} missing from {listing}"))
    };

    let listing = json(Action::List);
    for pane in ["sessions", "agent", "search"] {
        assert_eq!(
            state_of(&listing, pane),
            "hidden",
            "{pane} is placed by the shipped arrangement, so the listing must not \
             report it as the one state that means a defect"
        );
    }

    // And the signal survives: a pane whose slot nothing names still reads unplaced,
    // or this fix would have replaced one wrong answer with another.
    run(Action::New {
        name: "nowhere".into(),
    })
    .expect("new");
    let file = ui.join("mods").join("90_nowhere.lua");
    let body = std::fs::read_to_string(&file).expect("read");
    std::fs::write(
        &file,
        body.replace("slot = \"center\"", "slot = \"nowhere\""),
    )
    .expect("write");
    assert_eq!(state_of(&json(Action::List), "nowhere"), "unplaced");
}

#[test]
fn the_listing_reports_the_name_that_commands_accept() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);

    // A managed pane whose file stem differs from its directory and from its
    // declared name — the shape that made the two disagree.
    std::fs::create_dir_all(ui.join("vendor-tree/mods")).expect("mkdir");
    std::fs::write(
        ui.join("vendor-tree/mods/40_thing.lua"),
        "return { name = \"thing\", slot = \"center\",          render = function() return { type = \"text\", text = \"x\" } end }\n",
    )
    .expect("pane");
    let entry = thurbox::session::ModEntry {
        src: "git+https://example.com/vendor-tree".into(),
        file: "vendor-tree/mods/40_thing.lua".into(),
        pin: None,
    };
    thurbox::kernel::packages::add_to_spec(&ui, &entry).expect("spec");

    let listing = json(Action::List);
    let row = listing["files"]
        .as_array()
        .expect("files")
        .iter()
        .find(|row| row["file"] == "vendor-tree/mods/40_thing.lua")
        .cloned()
        .unwrap_or_else(|| panic!("missing from {listing}"));

    assert_eq!(
        row["entry"], "thing",
        "the managed row must carry the key `remove` takes: {row}"
    );
    // And an unmanaged file has no entry to report, rather than an invented one.
    let bundled = listing["files"]
        .as_array()
        .expect("files")
        .iter()
        .find(|row| row["file"] == "mods/10_sessions.lua")
        .cloned()
        .expect("bundled pane listed");
    assert!(
        bundled["entry"].is_null(),
        "a file the spec does not manage has no entry name: {bundled}"
    );
}

#[test]
fn a_pane_that_floats_is_not_reported_as_unplaced() {
    // A float draws ABOVE the arrangement, so it needs no slot at all. Reporting
    // one would fault the creation wizard, which is bundled and correct.
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    run(Action::New {
        name: "notes".into(),
    })
    .expect("new");

    let file = ui.join("mods").join("90_notes.lua");
    let body = std::fs::read_to_string(&file).expect("read");
    std::fs::write(
        &file,
        body.replace("slot = \"center\"", "slot = \"notes\",\n  floats = true"),
    )
    .expect("write");

    let output = run(Action::Check).expect("check runs");
    assert!(output.failure.is_none(), "{:?}", output.json);
    assert_eq!(output.json["ok"], true, "{:?}", output.json);
}

#[test]
fn a_pane_the_user_turned_off_is_not_reported_as_unplaced() {
    // Turning a pane off is the way back from a broken one, so a check that read
    // it anyway would report a failure the interface does not have. Removing the
    // slot from the arrangement is the CORRECT thing to do alongside it.
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("THURBOX_CONFIG_DIR", home.path());
    let ui = at(home.path());
    run(Action::New {
        name: "notes".into(),
    })
    .expect("new");

    let file = ui.join("mods").join("90_notes.lua");
    let body = std::fs::read_to_string(&file).expect("read");
    std::fs::write(&file, body.replace("slot = \"center\"", "slot = \"notes\"")).expect("write");

    // Unplaced while it is on …
    let on = run(Action::Check).expect("check runs");
    assert!(on.failure.is_some(), "{:?}", on.json);

    // … and not a failure once it is off.
    let mut registry = thurbox::kernel::registry::Registry::load();
    registry
        .set_disabled(&file.to_string_lossy(), true)
        .expect("disable");

    let off = run(Action::Check).expect("check runs");
    std::env::remove_var("THURBOX_CONFIG_DIR");
    assert!(off.failure.is_none(), "{:?}", off.json);
    assert!(
        !off.json["loaded"].to_string().contains("notes"),
        "and it is not reported as loaded either, because it was not read: {:?}",
        off.json
    );
}

#[test]
fn a_broken_mod_is_named_with_its_reason_and_fails_the_exit() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    run(Action::New {
        name: "notes".into(),
    })
    .expect("new");
    std::fs::write(ui.join("mods").join("90_notes.lua"), "return { name =").expect("break it");

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
    std::fs::create_dir_all(ui.join("mods")).expect("mkdir");
    thurbox::kernel::bundled::materialize(&ui);
    for entry in std::fs::read_dir(ui.join("mods")).expect("read plugins") {
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
fn the_listing_tells_a_file_you_installed_from_one_you_wrote() {
    // The distinction a capability grant rests on: `run` is granted per file, so
    // "who shipped this" is the question to answer before granting it. Until the
    // installed origin existed, a third-party pane and one you wrote yourself were
    // the same answer.
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);
    std::fs::write(ui.join("mods").join("90_mine.lua"), "return {}\n").expect("write");

    let entry = thurbox::session::ModEntry {
        src: "atlas".into(),
        file: "mods/75_atlas.lua".into(),
        pin: Some("v0.3.1".into()),
    };
    let mut lock = thurbox::session::ModLock::default();
    thurbox::kernel::packages::deliver(
        &ui,
        &entry,
        "https://example.com/atlas",
        "v0.3.1",
        &[thurbox::kernel::packages::Payload {
            file: "mods/75_atlas.lua".into(),
            contents: "return {}\n".into(),
        }],
        &mut lock,
    )
    .expect("deliver");
    thurbox::kernel::packages::write_lock(&ui, &lock).expect("write lock");
    thurbox::kernel::packages::add_to_spec(&ui, &entry).expect("add to spec");

    let listing = json(Action::List);
    let row = |file: &str| -> serde_json::Value {
        listing["files"]
            .as_array()
            .expect("files")
            .iter()
            .find(|row| row["file"] == file)
            .cloned()
            .unwrap_or_else(|| panic!("{file} missing from {listing}"))
    };

    let installed = row("mods/75_atlas.lua");
    assert_eq!(installed["source"], "installed", "{installed}");
    assert_eq!(installed["installed_from"], "atlas", "{installed}");
    let mine = row("mods/90_mine.lua");
    assert_eq!(mine["source"], "user", "{mine}");
    assert!(mine["installed_from"].is_null(), "{mine}");

    // The spec is part of what the interface is made of, and it is not a pane that
    // failed to load — which is what it would read as without its own kind.
    let spec = row("mods.toml");
    assert_eq!(spec["kind"], "manifest", "{spec}");
    assert_ne!(spec["state"], "failed", "{spec}");

    // Edited locally, or re-tagged upstream under the same pin. Either way the
    // contents are no longer the ones the grant was made against.
    std::fs::write(ui.join("mods").join("75_atlas.lua"), "-- mine\n").expect("edit");
    let after = json(Action::List);
    let edited = after["files"]
        .as_array()
        .expect("files")
        .iter()
        .find(|row| row["file"] == "mods/75_atlas.lua")
        .cloned()
        .expect("row");
    assert_eq!(edited["source"], "installed · modified", "{edited}");
    assert_eq!(edited["installed_from"], "atlas", "{edited}");
}

#[test]
fn the_listing_tells_a_shipped_file_from_an_edited_one() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);

    let before = json(Action::List);
    let files = before["files"].to_string();
    assert!(files.contains("mods/10_sessions.lua"), "{files}");
    assert!(files.contains("\"source\":\"bundled\""), "{files}");

    // Editing a shipped file changes where it is reported as coming from, which
    // is what makes "my change did nothing" diagnosable.
    let sessions = ui.join("mods").join("10_sessions.lua");
    let body = std::fs::read_to_string(&sessions).expect("read");
    std::fs::write(&sessions, format!("-- mine\n{body}")).expect("write");

    let after = json(Action::List);
    let row = after["files"]
        .as_array()
        .expect("files")
        .iter()
        .find(|row| row["file"] == "mods/10_sessions.lua")
        .expect("the edited file")
        .clone();
    assert_eq!(row["source"], "edited", "{row}");
}

// ── the example ────────────────────────────────────────────────────────────

#[test]
fn the_documented_example_is_a_mod_that_loads() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);
    let example = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join("lua")
        .join("mod.lua");
    std::fs::copy(&example, ui.join("mods").join("90_example.lua")).expect("copy");

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
        .join("../../examples")
        .join("lua")
        .join("composite.lua");
    std::fs::copy(&example, ui.join("mods").join("95_composite.lua")).expect("copy");

    let host = thurbox::kernel::host::LuaHost::new(&ui);
    assert!(host.error.is_none(), "{:?}", host.error);
    let index = host
        .index_of("composite")
        .expect("the example did not load");
    assert_eq!(
        host.mods[index].capabilities,
        vec![thurbox::kernel::host::Capability::Run],
        "it must declare what it needs, or it would be granted nothing silently"
    );
}

#[test]
fn an_untrusted_composite_draws_how_to_trust_it_rather_than_failing() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);
    let example = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join("lua")
        .join("composite.lua");
    std::fs::copy(&example, ui.join("mods").join("95_composite.lua")).expect("copy");

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

// ── the install that cannot demonstrate itself ─────────────────────────────

#[test]
fn a_pane_sharing_a_switch_slot_with_no_pill_is_warned_about() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);

    // `20_agent` already occupies `center` in switch mode and sorts first, so this
    // one is the alternate.
    std::fs::write(
        ui.join("mods").join("90_hidden.lua"),
        "return {\n\
           name = \"hidden\",\n\
           slot = \"center\",\n\
           render = function() return { type = \"text\", text = \"hi\" } end,\n\
         }\n",
    )
    .expect("write");

    let output = run(Action::Check).expect("check runs");
    assert!(
        output.failure.is_none(),
        "a warning must not fail the exit, or the check stops being a gate: {:?}",
        output.json
    );
    let warnings = output.json["warnings"].to_string();
    assert!(warnings.contains("90_hidden.lua"), "{warnings}");
    assert!(warnings.contains("pill"), "and says what to do: {warnings}");
    assert!(
        output.human.contains("90_hidden.lua"),
        "and a human reading the output sees it: {}",
        output.human
    );
}

#[test]
fn declaring_a_pill_is_enough_to_be_findable() {
    // The action band is kernel chrome and enumerates declared pills without invoking
    // anything, so a pill is the one advertisement that is automatic.
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);
    std::fs::write(
        ui.join("mods").join("90_hidden.lua"),
        "return {\n\
           name = \"hidden\",\n\
           slot = \"center\",\n\
           pills = { { action = \"hidden.open\", label = \"Hidden\", priority = 10 } },\n\
           render = function() return { type = \"text\", text = \"hi\" } end,\n\
         }\n",
    )
    .expect("write");

    let output = run(Action::Check).expect("check runs");
    assert!(output.failure.is_none(), "{:?}", output.json);
    assert_eq!(
        output.json["warnings"].as_array().map(Vec::len),
        Some(0),
        "a pane the band offers is findable: {:?}",
        output.json
    );
}

#[test]
fn the_pane_that_draws_by_default_is_not_warned_about() {
    // The first occupant of a switch slot is the one shown, so it needs no pill —
    // warning about it would train the reader to ignore the warning.
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("THURBOX_CONFIG_DIR", home.path());
    std::env::set_var("THURBOX_UI_DIR", checkout_ui());
    let output = run(Action::Check).expect("check runs");
    assert!(output.failure.is_none(), "{:?}", output.json);
    assert_eq!(
        output.json["warnings"].as_array().map(Vec::len),
        Some(0),
        "the shipped interface must be quiet: {:?}",
        output.json
    );
}

/// A pill the band drops is the one declaration with no symptom anywhere: no
/// button, no warning, no log line, and the author's next move is reading
/// `bands.rs`. The drop itself is right — a chip that lights and does nothing
/// costs a press to discover — so what `check` owes them is which of the two
/// mistakes they made.
#[test]
fn a_dropped_pill_says_whether_the_palette_has_its_action_or_nothing_does() {
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("THURBOX_CONFIG_DIR", home.path());
    let ui = at(home.path());
    run(Action::New {
        name: "notes".into(),
    })
    .expect("new");

    // Two pills, neither of which the band can draw: one names a chord-less
    // `commands` entry, the other is a typo for it.
    let file = ui.join("mods").join("90_notes.lua");
    let body = std::fs::read_to_string(&file).expect("read");
    std::fs::write(
        &file,
        body.replace(
            "  settings = {",
            "  commands = {\n\
            \x20   { action = \"notes.open\", desc = \"open the notes\" },\n\
            \x20 },\n\n\
            \x20 pills = {\n\
            \x20   { action = \"notes.open\", label = \"Notes\" },\n\
            \x20   { action = \"notes.opne\", label = \"Memory\" },\n\
            \x20 },\n\n\
            \x20 settings = {",
        ),
    )
    .expect("write");

    let output = run(Action::Check).expect("check runs");
    std::env::remove_var("THURBOX_CONFIG_DIR");
    assert!(output.failure.is_none(), "{:?}", output.json);

    let dropped = output.json["dropped_pills"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let of = |label: &str| -> serde_json::Value {
        dropped
            .iter()
            .find(|row| row["label"] == label)
            .cloned()
            .unwrap_or_else(|| panic!("no report for the {label:?} pill: {:?}", output.json))
    };

    let palette_only = of("Notes");
    assert_eq!(palette_only["action"], "notes.open");
    assert_eq!(palette_only["file"], "mods/90_notes.lua");
    assert_eq!(
        palette_only["reason"], "palette-only",
        "an action the palette can reach is a reasonable thing to have written: \
         {palette_only}"
    );

    let unknown = of("Memory");
    assert_eq!(unknown["action"], "notes.opne");
    assert_eq!(
        unknown["reason"], "unknown",
        "and a typo is not the same mistake: {unknown}"
    );

    let human = output.human.clone();
    assert!(
        human.contains("Notes") && human.contains("notes.open"),
        "the terminal report names the pill and its action, since the JSON is not \
         what an author reads: {human}"
    );
    assert!(
        human.contains("Memory") && human.contains("notes.opne"),
        "both of them: {human}"
    );
}

// ── the chord somebody else already spent ──────────────────────────────────

#[test]
fn two_mods_claiming_one_global_chord_are_both_named() {
    let home = tempfile::tempdir().expect("tempdir");
    // The overrides decide who wins a clash, so the check is run against a config
    // directory of this test's own rather than whatever the machine has rebound.
    std::env::set_var("THURBOX_CONFIG_DIR", home.path());
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);

    // Pills so the only thing left to report is the chord: both take the `center`
    // switch slot, and an alternate with no pill earns a warning of its own.
    for (file, name) in [("90_notes.lua", "notes"), ("91_scratch.lua", "scratch")] {
        std::fs::write(
            ui.join("mods").join(file),
            format!(
                "return {{\n  \
                   name = {name:?},\n  \
                   slot = \"center\",\n  \
                   keys = {{ {{ key = \"f7\", action = \"{name}.toggle\", \
                     scope = \"global\", desc = \"toggle\" }} }},\n  \
                   pills = {{ {{ action = \"{name}.toggle\", label = {name:?}, \
                     priority = 10 }} }},\n  \
                   render = function() return {{ type = \"text\", text = \"hi\" }} end,\n\
                 }}\n"
            ),
        )
        .expect("write");
    }

    let output = run(Action::Check).expect("check runs");
    assert!(
        output.failure.is_none(),
        "a chord two authors both wanted is a judgement call, like a missing pill — \
         failing the exit on it would make the check unusable as a gate: {:?}",
        output.json
    );
    let warnings = output.json["warnings"].to_string();
    assert!(warnings.contains("f7"), "names the chord: {warnings}");
    for both in [
        "90_notes.lua",
        "91_scratch.lua",
        "notes.toggle",
        "scratch.toggle",
    ] {
        assert!(
            warnings.contains(both),
            "names both claimants — {both} missing from {warnings}"
        );
    }
    // Which one fires is the whole question the author is asking, and the registry
    // answers it the same way `resolve` does: between two equals the earlier
    // declaration wins, and files load in name order.
    assert!(
        warnings.contains("notes.toggle wins"),
        "and says which one fires: {warnings}"
    );
    let human = output.human.clone();
    assert!(
        human.contains("f7") && human.contains("notes.toggle wins"),
        "and a human reading the output sees it: {human}"
    );
}

#[test]
fn the_same_chord_in_two_mod_scopes_is_not_reported() {
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("THURBOX_CONFIG_DIR", home.path());
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);

    for (file, name) in [("90_notes.lua", "notes"), ("91_scratch.lua", "scratch")] {
        std::fs::write(
            ui.join("mods").join(file),
            format!(
                "return {{\n  \
                   name = {name:?},\n  \
                   slot = \"center\",\n  \
                   keys = {{ {{ key = \"f7\", action = \"{name}.toggle\", \
                     desc = \"toggle\" }} }},\n  \
                   pills = {{ {{ action = \"{name}.toggle\", label = {name:?}, \
                     priority = 10 }} }},\n  \
                   render = function() return {{ type = \"text\", text = \"hi\" }} end,\n\
                 }}\n"
            ),
        )
        .expect("write");
    }

    let output = run(Action::Check).expect("check runs");
    assert_eq!(
        output.json["warnings"].as_array().map(Vec::len),
        Some(0),
        "focus decides between two plugin-scoped claims: {:?}",
        output.json
    );
}

#[test]
fn a_mod_taking_a_kernel_chord_is_reported_against_the_kernel() {
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("THURBOX_CONFIG_DIR", home.path());
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);

    std::fs::write(
        ui.join("mods").join("90_notes.lua"),
        "return {\n  \
           name = \"notes\",\n  \
           slot = \"center\",\n  \
           keys = { { key = \"f1\", action = \"notes.help\", scope = \"global\", \
             desc = \"my own help\" } },\n  \
           pills = { { action = \"notes.help\", label = \"Notes\", priority = 10 } },\n  \
           render = function() return { type = \"text\", text = \"hi\" } end,\n\
         }\n",
    )
    .expect("write");

    let output = run(Action::Check).expect("check runs");
    let warnings = output.json["warnings"].to_string();
    assert!(warnings.contains("f1"), "names the chord: {warnings}");
    assert!(
        warnings.contains("notes.help") && warnings.contains("help.open"),
        "and both claimants, the kernel's by its action: {warnings}"
    );
    // Nothing on disk declares it, so the kernel is named by its owner rather than by
    // a file — a row whose `file` were blank would read as a defect in the interface.
    assert!(
        warnings.contains("kernel"),
        "the claim with no file behind it is named for its owner: {warnings}"
    );
}

#[test]
fn raw_gestures_are_reported_and_kernel_action_names_are_rejected() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);
    std::fs::write(
        ui.join("mods/90_notes.lua"),
        "return { name = 'notes', slot = 'center',\n\
         keys = { { key = 'f7', action = 'kernel.quit', desc = 'spoof' } },\n\
         on_key = function() return true end,\n\
         on_scroll = function() return true end,\n\
         on_click = function() return true end,\n\
         render = function() return { type = 'text', text = 'notes', role = 'action:notes.unknown' } end }",
    ).expect("write plugin");
    let output = run(Action::Check).expect("check");
    let warnings = output.json["warnings"].to_string();
    assert!(warnings.contains("kernel.quit is reserved"), "{warnings}");
    assert!(warnings.contains("on_click handles gestures"), "{warnings}");
    assert!(!warnings.contains("on_key handles gestures"), "{warnings}");
    assert!(
        !warnings.contains("on_scroll handles gestures"),
        "{warnings}"
    );
    assert!(
        warnings.contains("notes.unknown is used by a menu or clickable node"),
        "{warnings}"
    );
}

#[test]
fn constructed_click_actions_are_checked_against_the_catalog() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);
    std::fs::write(
        ui.join("mods/90_notes.lua"),
        "local MISSING = 'notes.unknown'\n\
         return { name = 'notes', slot = 'center',\n\
         render = function() return { type = 'text', text = 'notes', role = 'action:' .. MISSING } end }",
    )
    .expect("write plugin");
    let output = run(Action::Check).expect("check");
    let warnings = output.json["warnings"].to_string();
    assert!(
        warnings.contains("notes.unknown is used by a menu or clickable node"),
        "{warnings}"
    );
}

#[test]
fn expression_built_clicks_do_not_report_a_partial_action_name() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);
    std::fs::write(
        ui.join("mods/90_notes.lua"),
        "local SUFFIX = 'open'\n\
         local ACTION = 'notes.' .. SUFFIX\n\
         return { name = 'notes', slot = 'center',\n\
         keys = { { key = 'f7', action = 'notes.open', desc = 'open' } },\n\
         on_action = function() return true end,\n\
         render = function() return { type = 'text', text = 'notes', role = 'action:' .. ACTION } end }",
    )
    .expect("write plugin");
    let output = run(Action::Check).expect("check");
    let warnings = output.json["warnings"].to_string();
    assert!(!warnings.contains("notes. is used by"), "{warnings}");
    assert!(
        warnings.contains("constructed click action ACTION cannot be advertised"),
        "{warnings}"
    );
}

#[test]
fn commented_literal_click_actions_are_recognized() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);
    std::fs::write(
        ui.join("mods/90_notes.lua"),
        "local ACTION = 'notes.open' -- button action\n\
         return { name = 'notes', slot = 'center',\n\
         keys = { { key = 'f7', action = 'notes.open', desc = 'open' } },\n\
         on_action = function() return true end,\n\
         render = function() return { type = 'text', text = 'notes', role = 'action:' .. ACTION } end }",
    )
    .expect("write plugin");
    let output = run(Action::Check).expect("check");
    let warnings = output.json["warnings"].to_string();
    assert!(
        !warnings.contains("constructed click action ACTION cannot be advertised"),
        "{warnings}"
    );
}

#[test]
fn a_mod_cannot_claim_the_kernel_owner_name() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);
    std::fs::write(
        ui.join("mods/90_kernel.lua"),
        "return { name = 'kernel', slot = 'center',\n\
         keys = { { key = 'f7', action = 'mine.open', desc = 'open' } },\n\
         on_action = function() return true end,\n\
         render = function() return { type = 'text', text = 'unsafe owner' } end }",
    )
    .expect("write plugin");
    let output = run(Action::Check).expect("check");
    assert_eq!(output.json["ok"], false, "{output:?}");
    assert!(
        output.json.to_string().contains("kernel is reserved"),
        "{output:?}"
    );
}

#[test]
fn a_name_two_files_answer_to_names_both_of_them() {
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("THURBOX_CONFIG_DIR", home.path());
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);

    // No `name`, so both take `notes` from the filename — the way a user ends up
    // with two of them without ever typing the name at all.
    for file in ["90_notes.lua", "91_notes.lua"] {
        std::fs::write(
            ui.join("mods").join(file),
            "return {\n  \
               slot = \"center\",\n  \
               keys = { { key = \"f7\", action = \"notes.toggle\", scope = \"global\", \
                 desc = \"toggle\" } },\n  \
               pills = { { action = \"notes.toggle\", label = \"Notes\", priority = 10 } },\n  \
               render = function() return { type = \"text\", text = \"hi\" } end,\n\
             }\n",
        )
        .expect("write");
    }

    let output = run(Action::Check).expect("check runs");
    let warnings = output.json["warnings"].to_string();
    for file in ["90_notes.lua", "91_notes.lua"] {
        assert!(
            warnings.contains(file),
            "an ambiguous name names every file that answers to it — {file} missing \
             from {warnings}"
        );
    }
}

/// A pane that declares `strip = true` is placed by the arrangement thurbox
/// ships, so a fresh install checks out with no edit to `layout.lua`.
#[test]
fn a_strip_checks_out_under_the_shipped_layout_with_no_edit() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    run(Action::New {
        name: "ticker".into(),
    })
    .expect("new");
    let file = ui.join("mods").join("90_ticker.lua");
    let body = std::fs::read_to_string(&file).expect("read");
    std::fs::write(
        &file,
        body.replace(
            "slot = \"center\"",
            "slot = \"ticker\",\n  strip = true,\n  size = { len = 2 }",
        ),
    )
    .expect("write");

    let output = run(Action::Check).expect("check runs");
    assert!(
        output.failure.is_none(),
        "the shipped layout places strips: {}",
        output.human
    );
}

/// An arrangement written before strips existed has no loop for them. The fix
/// is that loop, once — not a line per strip, which would pin a height the pane
/// already declares — so that is what the failure says.
#[test]
fn a_strip_under_a_layout_without_the_loop_is_told_to_add_the_loop() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    run(Action::New {
        name: "ticker".into(),
    })
    .expect("new");
    let file = ui.join("mods").join("90_ticker.lua");
    let body = std::fs::read_to_string(&file).expect("read");
    std::fs::write(
        &file,
        body.replace("slot = \"center\"", "slot = \"ticker\",\n  strip = true"),
    )
    .expect("write");
    std::fs::write(
        ui.join("layout.lua"),
        "return function(ctx)\n  return { children = {\n    { slot = \"sessions\", len = 1 },\n    \
         { slot = \"center\" },\n    { slot = \"footer\", len = 1 },\n  } }\nend\n",
    )
    .expect("write layout");

    let output = run(Action::Check).expect("check runs");
    assert!(
        output.failure.is_some(),
        "an unplaced strip fails the check"
    );
    assert!(
        output.human.contains("ctx.strips"),
        "and names the loop as the fix: {}",
        output.human
    );
}
