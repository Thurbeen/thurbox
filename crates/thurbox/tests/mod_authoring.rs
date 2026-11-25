use thurbox::cli::mods::{run, Action};

// nextest runs each test in its own process, so these env overrides cannot leak.
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
    std::env::remove_var("THURBOX_UI_DIR");
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

#[test]
fn the_shipped_interface_checks_out() {
    std::env::set_var("THURBOX_UI_DIR", checkout_ui());
    let checked = json(Action::Check);
    assert_eq!(checked["ok"], true, "{checked}");
    let loaded = checked["loaded"].to_string();
    for pane in ["sessions", "agent", "new_session", "confirm", "search"] {
        assert!(loaded.contains(pane), "{pane} missing from {loaded}");
    }
    assert!(
        loaded.contains("search"),
        "`search` starts closed, so the check opens every panel before it resolves the \
         arrangement: {checked}"
    );
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
    assert!(
        output.failure.is_none(),
        "a float draws above the arrangement and needs no slot: {:?}",
        output.json
    );
    assert_eq!(output.json["ok"], true, "{:?}", output.json);
}

#[test]
fn a_pane_the_user_turned_off_is_not_reported_as_unplaced() {
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

    let on = run(Action::Check).expect("check runs");
    assert!(on.failure.is_some(), "{:?}", on.json);

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

#[test]
fn the_listing_tells_a_file_you_installed_from_one_you_wrote() {
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

    let spec = row("mods.toml");
    assert_eq!(spec["kind"], "manifest", "{spec}");
    assert_ne!(
        spec["state"], "failed",
        "the spec is not a pane that failed to load: {spec}"
    );

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
    assert!(
        format!("{:?}", rendered.node).contains("session(s)"),
        "the example must render what it reads"
    );
}

#[test]
fn the_composite_example_loads_and_declares_what_it_needs() {
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

#[test]
fn a_pane_sharing_a_switch_slot_with_no_pill_is_warned_about() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = at(home.path());
    thurbox::kernel::bundled::materialize(&ui);

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
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("THURBOX_CONFIG_DIR", home.path());
    std::env::set_var("THURBOX_UI_DIR", checkout_ui());
    let output = run(Action::Check).expect("check runs");
    assert!(output.failure.is_none(), "{:?}", output.json);
    assert_eq!(
        output.json["warnings"].as_array().map(Vec::len),
        Some(0),
        "the first occupant of a switch slot is the one shown, so the shipped interface \
         needs no pill and must be quiet: {:?}",
        output.json
    );
}

#[test]
fn a_dropped_pill_says_whether_the_palette_has_its_action_or_nothing_does() {
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("THURBOX_CONFIG_DIR", home.path());
    let ui = at(home.path());
    run(Action::New {
        name: "notes".into(),
    })
    .expect("new");

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

#[test]
fn two_mods_claiming_one_global_chord_are_both_named() {
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
