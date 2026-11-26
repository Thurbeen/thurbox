use std::path::{Path, PathBuf};

use thurbox::cli::mods::{run, Action};

fn interface(dir: &Path) -> PathBuf {
    let ui = dir.join("ui");
    std::fs::create_dir_all(&ui).expect("mkdir");
    std::env::set_var("THURBOX_UI_DIR", &ui);
    thurbox::kernel::bundled::materialize(&ui);
    ui
}

fn package(root: &Path, name: &str, version: &str, marker: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join("mod.toml"),
        format!(
            "name = \"{name}\"\n\
             description = \"a test pane\"\n\
             version = \"{version}\"\n\
             pane = {{ source = \"pane.lua\", path = \"mods/75_{name}.lua\" }}\n\
             [[module]]\n\
             source = \"util.lua\"\n\
             path = \"lib/{name}/util.lua\"\n"
        ),
    )
    .expect("manifest");
    std::fs::write(
        dir.join("pane.lua"),
        format!(
            "local util = require(\"lib.{name}.util\")\n\
             return {{\n\
               name = \"{name}\",\n\
               slot = \"{name}\",\n\
               render = function(ctx)\n\
                 return {{ kind = \"text\", text = util.label(ctx.width) }}\n\
               end,\n\
             }}\n"
        ),
    )
    .expect("pane");
    std::fs::write(
        dir.join("util.lua"),
        format!(
            "local u = {{}}\n\
             function u.label(w) return \"{marker} \" .. tostring(w) end\n\
             return u\n"
        ),
    )
    .expect("module");
    dir
}

fn install(src: &Path) -> thurbox::cli::output::CommandOutput {
    run(Action::Install {
        src: src.display().to_string(),
        as_file: None,
        pin: None,
    })
    .expect("install runs")
}

#[test]
fn every_distributed_package_installs_and_loads() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/panes");
    let mut names: Vec<String> = std::fs::read_dir(&root)
        .expect("examples/panes/")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert!(!names.is_empty(), "there is nothing to distribute");

    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    for name in &names {
        let report = install(&root.join(name));
        assert!(
            report.failure.is_none(),
            "{name} did not install: {:?}",
            report.json
        );
        let file = report.json["file"].as_str().unwrap_or_default();
        assert!(ui.join(file).is_file(), "{name} delivered no pane");
    }

    let checked = run(Action::Check).expect("check runs");
    let loaded = checked.json["loaded"].to_string();
    for name in &names {
        assert!(
            loaded.contains(name.as_str()),
            "{name} did not load: {loaded}"
        );
    }

    let listed: Vec<&str> = thurbox::kernel::packages::EXAMPLE_MODS
        .iter()
        .map(|(name, _)| *name)
        .collect();
    for name in &names {
        assert!(
            listed.contains(&name.as_str()),
            "{name} is in examples/panes/ but absent from EXAMPLE_MODS, so it cannot \
             be installed by name and a typo suggests nothing: {listed:?}"
        );
    }
    assert_eq!(
        listed.len(),
        names.len(),
        "and nothing is listed that is not there"
    );
}

#[test]
fn a_package_installs_its_pane_and_its_own_modules() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = package(home.path(), "atlas", "v0.3.1", "atlas");

    let report = install(&src);
    assert!(report.failure.is_none(), "{:?}", report.json);
    assert_eq!(report.json["outcome"], "installed", "{:?}", report.json);
    assert_eq!(report.json["version"], "v0.3.1", "{:?}", report.json);
    assert!(ui.join("mods/75_atlas.lua").is_file());
    assert!(
        ui.join("lib/atlas/util.lua").is_file(),
        "a package's shared module lands in a namespace of its own"
    );

    let spec = std::fs::read_to_string(ui.join("mods.toml")).expect("spec");
    assert!(spec.contains("mods/75_atlas.lua"), "{spec}");
    assert!(spec.contains("atlas"), "{spec}");
    let lock = std::fs::read_to_string(ui.join("mods.lock")).expect("lock");
    assert!(lock.contains("v0.3.1"), "{lock}");
    assert!(lock.contains("lib/atlas/util.lua"), "{lock}");

    let checked = run(Action::Check).expect("check runs");
    assert!(
        checked.json["loaded"].to_string().contains("atlas"),
        "{:?}",
        checked.json
    );
}

#[test]
fn installing_says_what_to_add_to_the_arrangement() {
    let home = tempfile::tempdir().expect("tempdir");
    let src = package(home.path(), "atlas", "v1", "atlas");
    let _ui = interface(home.path());

    let report = install(&src);
    let hint = report.json["placement_hint"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(hint.contains("atlas"), "names the slot: {hint}");
    assert!(
        hint.contains("slot = \"atlas\""),
        "and gives the line to add: {hint}"
    );
}

#[test]
fn a_single_lua_file_needs_a_destination_and_then_installs() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let loose = home.path().join("notes.lua");
    std::fs::write(
        &loose,
        "return { name = \"notes\", slot = \"center\", \
         render = function() return { kind = \"text\", text = \"notes\" } end }\n",
    )
    .expect("write");

    let refused = run(Action::Install {
        src: loose.display().to_string(),
        as_file: None,
        pin: None,
    });
    let error = refused.expect_err("should refuse");
    assert!(error.contains("--as"), "and says how to give one: {error}");

    let done = run(Action::Install {
        src: loose.display().to_string(),
        as_file: Some("mods/90_notes.lua".into()),
        pin: None,
    })
    .expect("install");
    assert!(done.failure.is_none(), "{:?}", done.json);
    assert!(ui.join("mods/90_notes.lua").is_file());
    assert_eq!(done.json["version"], "unpinned", "{:?}", done.json);
}

#[test]
fn a_destination_already_holding_an_unmanaged_file_is_refused() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = package(home.path(), "atlas", "v1", "atlas");
    let occupied = ui.join("mods/10_sessions.lua");
    let before = std::fs::read_to_string(&occupied).expect("read");

    let error = run(Action::Install {
        src: src.display().to_string(),
        as_file: Some("mods/10_sessions.lua".into()),
        pin: None,
    })
    .expect_err("should refuse");
    assert!(error.contains("10_sessions.lua"), "names the file: {error}");
    assert_eq!(
        std::fs::read_to_string(&occupied).expect("read"),
        before,
        "the existing file is left exactly as it was"
    );
    assert!(
        !ui.join("mods.toml").exists(),
        "and nothing is recorded either"
    );
}

#[test]
fn a_module_may_not_be_delivered_outside_its_own_namespace() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let theme = ui.join("lib/theme.lua");
    let before = std::fs::read_to_string(&theme).expect("read");

    let src = home.path().join("bad");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(
        src.join("mod.toml"),
        "name = \"bad\"\n\
         pane = { source = \"pane.lua\", path = \"mods/95_bad.lua\" }\n\
         [[module]]\n\
         source = \"theme.lua\"\n\
         path = \"lib/theme.lua\"\n",
    )
    .expect("manifest");
    std::fs::write(src.join("pane.lua"), "return {}\n").expect("pane");
    std::fs::write(src.join("theme.lua"), "return {}\n").expect("module");

    let error = run(Action::Install {
        src: src.display().to_string(),
        as_file: None,
        pin: None,
    })
    .expect_err("should refuse");
    assert!(error.contains("lib/bad/"), "names the namespace: {error}");
    assert_eq!(
        std::fs::read_to_string(&theme).expect("read"),
        before,
        "the shipped module is untouched"
    );
}

fn multi_pane_package(root: &Path, name: &str, version: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join("mod.toml"),
        format!(
            "name = \"{name}\"\n\
             description = \"two panes, one package\"\n\
             version = \"{version}\"\n\
             [[pane]]\n\
             source = \"main.lua\"\n\
             path = \"mods/75_{name}.lua\"\n\
             [[pane]]\n\
             source = \"notes.lua\"\n\
             path = \"mods/76_{name}_notes.lua\"\n\
             [[module]]\n\
             source = \"util.lua\"\n\
             path = \"lib/{name}/util.lua\"\n"
        ),
    )
    .expect("manifest");
    for (file, slot) in [
        ("main.lua", name.to_string()),
        ("notes.lua", format!("{name}_notes")),
    ] {
        std::fs::write(
            dir.join(file),
            format!(
                "local util = require(\"lib.{name}.util\")\n\
                 return {{\n\
                   name = \"{slot}\",\n\
                   slot = \"{slot}\",\n\
                   render = function(ctx)\n\
                     return {{ kind = \"text\", text = util.label(ctx.width) }}\n\
                   end,\n\
                 }}\n"
            ),
        )
        .expect("pane");
    }
    std::fs::write(
        dir.join("util.lua"),
        "local u = {}\nfunction u.label(w) return \"m \" .. tostring(w) end\nreturn u\n",
    )
    .expect("module");
    dir
}

#[test]
fn a_package_with_several_panes_installs_all_of_them() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = multi_pane_package(home.path(), "atlas", "v0.3.1");

    let report = install(&src);
    assert!(report.failure.is_none(), "{:?}", report.json);
    assert!(
        ui.join("mods/75_atlas.lua").is_file(),
        "the first pane lands"
    );
    assert!(
        ui.join("mods/76_atlas_notes.lua").is_file(),
        "and so does the second — one install, not two"
    );
    assert!(ui.join("lib/atlas/util.lua").is_file(), "with their module");

    let lock = thurbox::kernel::packages::read_lock(&ui).expect("lock");
    assert_eq!(lock.mods.len(), 1, "{lock:?}");
    let entry = &lock.mods[0];
    assert_eq!(entry.version, "v0.3.1");
    for file in [
        "mods/75_atlas.lua",
        "mods/76_atlas_notes.lua",
        "lib/atlas/util.lua",
    ] {
        assert!(entry.files.contains_key(file), "{file} recorded: {entry:?}");
    }

    let checked = run(Action::Check).expect("check runs");
    let loaded: Vec<&str> = checked.json["loaded"]
        .as_array()
        .expect("loaded is a list")
        .iter()
        .filter_map(|name| name.as_str())
        .collect();
    assert!(loaded.contains(&"atlas"), "{loaded:?}");
    assert!(loaded.contains(&"atlas_notes"), "{loaded:?}");
}

#[test]
fn a_singular_pane_manifest_installs_exactly_as_it_did() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = package(home.path(), "atlas", "v0.3.1", "atlas");

    let report = install(&src);
    assert!(report.failure.is_none(), "{:?}", report.json);
    assert_eq!(
        report.json["file"], "mods/75_atlas.lua",
        "{:?}",
        report.json
    );
    assert!(ui.join("mods/75_atlas.lua").is_file());

    let moved = package(home.path(), "beacon", "v1", "beacon");
    let done = run(Action::Install {
        src: moved.display().to_string(),
        as_file: Some("mods/88_beacon.lua".into()),
        pin: None,
    })
    .expect("install");
    assert!(done.failure.is_none(), "{:?}", done.json);
    assert!(
        ui.join("mods/88_beacon.lua").is_file(),
        "--as still redirects the one pane a singular manifest declares"
    );
    assert!(!ui.join("mods/75_beacon.lua").is_file());
}

#[test]
fn as_selects_which_pane_the_entry_is_keyed_on() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = multi_pane_package(home.path(), "atlas", "v1");

    let done = run(Action::Install {
        src: src.display().to_string(),
        as_file: Some("mods/76_atlas_notes.lua".into()),
        pin: None,
    })
    .expect("install");
    assert!(done.failure.is_none(), "{:?}", done.json);
    assert_eq!(
        done.json["file"], "mods/76_atlas_notes.lua",
        "the named pane is the one the spec entry is keyed on: {:?}",
        done.json
    );
    assert!(ui.join("mods/75_atlas.lua").is_file());
    assert!(ui.join("mods/76_atlas_notes.lua").is_file());

    let spec = std::fs::read_to_string(ui.join("mods.toml")).expect("spec");
    assert!(spec.contains("mods/76_atlas_notes.lua"), "{spec}");
}

#[test]
fn as_naming_no_declared_pane_is_refused_and_lists_them() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = multi_pane_package(home.path(), "atlas", "v1");

    let error = run(Action::Install {
        src: src.display().to_string(),
        as_file: Some("mods/90_elsewhere.lua".into()),
        pin: None,
    })
    .expect_err("should refuse");
    assert!(error.contains("mods/75_atlas.lua"), "{error}");
    assert!(error.contains("mods/76_atlas_notes.lua"), "{error}");
    assert!(
        !ui.join("mods.toml").exists(),
        "and nothing is recorded: {error}"
    );
}

#[test]
fn keying_an_installed_package_on_another_pane_is_refused() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = multi_pane_package(home.path(), "atlas", "v1");
    install(&src);

    let error = run(Action::Install {
        src: src.display().to_string(),
        as_file: Some("mods/76_atlas_notes.lua".into()),
        pin: None,
    })
    .expect_err("should refuse");
    assert!(
        error.contains("mods/75_atlas.lua"),
        "names the owner: {error}"
    );
    let spec = thurbox::kernel::packages::read_spec(&ui).expect("spec");
    assert_eq!(spec.mods.len(), 1, "{spec:?}");

    let again = install(&src);
    assert!(again.failure.is_none(), "{:?}", again.json);
}

#[test]
fn syncing_a_spec_that_keys_one_package_twice_is_refused() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = multi_pane_package(home.path(), "atlas", "v1");
    install(&src);
    let spec = std::fs::read_to_string(ui.join("mods.toml")).expect("spec");
    std::fs::write(
        ui.join("mods.toml"),
        format!(
            // A TOML literal string, so a Windows path's backslashes are not escapes.
            "{spec}\n[[mod]]\nsrc = '{}'\nfile = \"mods/76_atlas_notes.lua\"\n",
            src.display()
        ),
    )
    .expect("hand edit");

    let error = run(Action::Sync).expect_err("should refuse");
    assert!(
        error.contains("mods/75_atlas.lua"),
        "names the owner: {error}"
    );
    let lock = thurbox::kernel::packages::read_lock(&ui).expect("lock");
    assert_eq!(lock.mods.len(), 1, "no second record: {lock:?}");
}

#[test]
fn rekeying_the_sole_entry_by_hand_then_updating_converges() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = multi_pane_package(home.path(), "atlas", "v1");
    install(&src);
    let spec = std::fs::read_to_string(ui.join("mods.toml")).expect("spec");
    std::fs::write(
        ui.join("mods.toml"),
        spec.replace("mods/75_atlas.lua", "mods/76_atlas_notes.lua"),
    )
    .expect("hand edit");

    let updated = run(Action::Update { name: None }).expect("update converges");
    assert!(updated.failure.is_none(), "{:?}", updated.json);
    let lock = thurbox::kernel::packages::read_lock(&ui).expect("lock");
    assert_eq!(lock.mods.len(), 1, "{lock:?}");
    assert_eq!(lock.mods[0].file, "mods/76_atlas_notes.lua");
    assert!(ui.join("mods/75_atlas.lua").is_file());
    assert!(ui.join("mods/76_atlas_notes.lua").is_file());

    let synced = run(Action::Sync).expect("sync");
    assert_eq!(
        synced.json["entries"][0]["outcome"], "current",
        "{:?}",
        synced.json
    );
    assert!(ui.join("mods/75_atlas.lua").is_file());
}

#[test]
fn a_targeted_update_leaves_another_entrys_stale_record_alone() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = package(home.path(), "beacon", "v1", "beacon");
    install(&src);
    run(Action::Install {
        src: src.display().to_string(),
        as_file: Some("mods/88_beacon.lua".into()),
        pin: None,
    })
    .expect("second copy");
    let spec = std::fs::read_to_string(ui.join("mods.toml")).expect("spec");
    std::fs::write(
        ui.join("mods.toml"),
        spec.replace("mods/75_beacon.lua", "mods/77_beacon.lua"),
    )
    .expect("hand edit");

    let updated = run(Action::Update {
        name: Some("mods/88_beacon.lua".into()),
    })
    .expect("update runs");
    assert!(
        !updated.json.to_string().contains("removed"),
        "nothing of the other entry's is taken back: {:?}",
        updated.json
    );
    assert!(ui.join("mods/75_beacon.lua").is_file());
}

#[test]
fn a_targeted_update_leaves_a_live_entrys_old_record_alone() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = multi_pane_package(home.path(), "atlas", "v1");
    let manifest = std::fs::read_to_string(src.join("mod.toml")).expect("manifest");
    std::fs::write(
        src.join("mod.toml"),
        manifest.replace(
            "[[module]]",
            "[[pane]]\nsource = \"notes.lua\"\npath = \"mods/77_atlas_extra.lua\"\n[[module]]",
        ),
    )
    .expect("third pane");
    install(&src);
    let loose = home.path().join("extra.lua");
    std::fs::write(&loose, "return { name = \"extra\", slot = \"extra\" }\n").expect("loose");
    let spec = std::fs::read_to_string(ui.join("mods.toml")).expect("spec");
    std::fs::write(
        ui.join("mods.toml"),
        format!(
            "{}\n[[mod]]\nsrc = '{}'\nfile = \"mods/77_atlas_extra.lua\"\n",
            spec.replace("mods/75_atlas.lua", "mods/76_atlas_notes.lua"),
            loose.display()
        ),
    )
    .expect("hand edit");

    let _ = run(Action::Update {
        name: Some("mods/77_atlas_extra.lua".into()),
    });
    assert!(ui.join("mods/75_atlas.lua").is_file());
    assert!(
        ui.join("mods/76_atlas_notes.lua").is_file(),
        "the re-keyed entry's own pane is still installed"
    );
}

#[test]
fn the_legacy_placement_hint_is_about_the_keyed_pane() {
    let home = tempfile::tempdir().expect("tempdir");
    let _ui = interface(home.path());
    let src = multi_pane_package(home.path(), "atlas", "v1");
    std::fs::write(
        src.join("main.lua"),
        "return { name = \"atlas\", slot = \"atlas\", floats = true, \
         render = function() return { kind = \"text\", text = \"a\" } end }\n",
    )
    .expect("floating pane");

    let report = install(&src);
    assert!(report.failure.is_none(), "{:?}", report.json);
    assert_eq!(report.json["file"], "mods/75_atlas.lua");
    assert!(
        report.json["placement_hint"].is_null(),
        "the keyed pane needs no hint: {:?}",
        report.json
    );
    assert!(
        report.json["placement_hints"]
            .to_string()
            .contains("atlas_notes"),
        "the second pane's hint is still reported: {:?}",
        report.json
    );
}

#[test]
fn removing_a_multi_pane_package_takes_back_every_pane() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = multi_pane_package(home.path(), "atlas", "v1");
    install(&src);

    run(Action::Remove {
        name: "atlas".into(),
    })
    .expect("remove runs");
    assert!(!ui.join("mods/75_atlas.lua").exists());
    assert!(
        !ui.join("mods/76_atlas_notes.lua").exists(),
        "one record covers both panes, so one removal takes both back"
    );
    assert!(!ui.join("lib/atlas/util.lua").exists());
}

#[test]
fn syncing_a_multi_pane_package_is_idempotent() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = multi_pane_package(home.path(), "atlas", "v1");
    install(&src);

    let synced = run(Action::Sync).expect("sync runs");
    assert!(synced.failure.is_none(), "{:?}", synced.json);
    assert_eq!(
        synced.json["entries"][0]["outcome"], "current",
        "a second run changes nothing: {:?}",
        synced.json
    );
    assert!(ui.join("mods/75_atlas.lua").is_file());
    assert!(ui.join("mods/76_atlas_notes.lua").is_file());
}

#[test]
fn syncing_an_agreeing_directory_changes_nothing() {
    let home = tempfile::tempdir().expect("tempdir");
    let _ui = interface(home.path());
    let src = package(home.path(), "atlas", "v1", "atlas");
    install(&src);

    let first = run(Action::Sync).expect("sync");
    assert_eq!(first.json["changed"], false, "{:?}", first.json);
    let again = run(Action::Sync).expect("sync");
    assert_eq!(again.json["changed"], false, "{:?}", again.json);
    assert_eq!(
        again.json["entries"][0]["outcome"], "current",
        "{:?}",
        again.json
    );
}

#[test]
fn syncing_installs_what_the_spec_lists_and_the_directory_lacks() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = package(home.path(), "atlas", "v1", "atlas");
    install(&src);

    std::fs::remove_file(ui.join("mods/75_atlas.lua")).expect("remove");
    std::fs::remove_file(ui.join("lib/atlas/util.lua")).expect("remove");
    let lock = thurbox::kernel::packages::read_lock(&ui).expect("lock");
    let mut fresh = thurbox::session::ModLock::default();
    for entry in lock.mods {
        fresh.record(thurbox::session::LockEntry {
            files: Default::default(),
            removed: Vec::new(),
            ..entry
        });
    }
    thurbox::kernel::packages::write_lock(&ui, &fresh).expect("write");

    let report = run(Action::Sync).expect("sync");
    assert_eq!(report.json["changed"], true, "{:?}", report.json);
    assert!(ui.join("mods/75_atlas.lua").is_file());
    assert!(ui.join("lib/atlas/util.lua").is_file());
}

#[test]
fn syncing_takes_back_what_the_spec_no_longer_lists() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = package(home.path(), "atlas", "v1", "atlas");
    install(&src);

    std::fs::write(ui.join("mods.toml"), "").expect("empty the spec");

    let report = run(Action::Sync).expect("sync");
    assert_eq!(
        report.json["entries"][0]["outcome"], "removed",
        "{:?}",
        report.json
    );
    assert!(!ui.join("mods/75_atlas.lua").exists());
    assert!(
        !ui.join("lib/atlas").exists(),
        "and the namespace it owned goes with it"
    );
}

#[test]
fn syncing_leaves_a_pane_the_spec_never_listed_alone() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let mine = ui.join("mods/90_mine.lua");
    std::fs::write(&mine, "return {}\n").expect("write");

    let report = run(Action::Sync).expect("sync");
    assert!(report.failure.is_none(), "{:?}", report.json);
    assert!(
        mine.is_file(),
        "a file nobody claimed is nobody's to remove"
    );
    assert!(
        !report.json["entries"].to_string().contains("90_mine"),
        "and it is not reported as a problem: {:?}",
        report.json
    );
}

#[test]
fn an_edit_to_an_installed_pane_survives_a_sync_and_is_reported() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = package(home.path(), "atlas", "v1", "atlas");
    install(&src);

    let pane = ui.join("mods/75_atlas.lua");
    std::fs::write(&pane, "-- mine\nreturn {}\n").expect("edit");
    package(home.path(), "atlas", "v2", "atlas v2");

    let report = run(Action::Sync).expect("sync");
    assert_eq!(
        report.json["entries"][0]["outcome"], "kept",
        "{:?}",
        report.json
    );
    assert_eq!(
        std::fs::read_to_string(&pane).expect("read"),
        "-- mine\nreturn {}\n",
        "the edit is the user's to keep"
    );
}

#[test]
fn a_deleted_pane_is_not_silently_reinstalled() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = package(home.path(), "atlas", "v1", "atlas");
    install(&src);
    std::fs::remove_file(ui.join("mods/75_atlas.lua")).expect("delete");

    for _ in 0..2 {
        let report = run(Action::Sync).expect("sync");
        assert_eq!(
            report.json["entries"][0]["outcome"], "deleted",
            "{:?}",
            report.json
        );
        assert!(
            !ui.join("mods/75_atlas.lua").exists(),
            "convergence must not undo a removal"
        );
    }
}

#[test]
fn updating_reports_the_version_it_came_from() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = package(home.path(), "atlas", "v0.3.1", "atlas");
    install(&src);

    let current = run(Action::Update { name: None }).expect("update");
    assert!(current.failure.is_none(), "{:?}", current.json);
    assert_eq!(current.json["moved"], 0, "{:?}", current.json);

    package(home.path(), "atlas", "v0.4.0", "atlas v2");
    let moved = run(Action::Update {
        name: Some("atlas".into()),
    })
    .expect("update");
    assert_eq!(moved.json["moved"], 1, "{:?}", moved.json);
    let entry = &moved.json["entries"][0];
    assert_eq!(entry["from"], "v0.3.1", "{entry}");
    assert_eq!(entry["version"], "v0.4.0", "{entry}");
    assert!(
        std::fs::read_to_string(ui.join("lib/atlas/util.lua"))
            .expect("read")
            .contains("atlas v2"),
        "and the files actually moved with it"
    );
    let _ = src;
}

#[test]
fn removing_works_without_the_source_and_refuses_an_unknown_name() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = package(home.path(), "atlas", "v1", "atlas");
    install(&src);

    std::fs::remove_dir_all(&src).expect("delete the source");

    let error = run(Action::Remove {
        name: "nope".into(),
    })
    .expect_err("should refuse");
    assert!(error.contains("nope"), "{error}");

    let removed = run(Action::Remove {
        name: "atlas".into(),
    })
    .expect("remove");
    assert!(removed.failure.is_none(), "{:?}", removed.json);
    assert!(!ui.join("mods/75_atlas.lua").exists());
    assert!(!ui.join("lib/atlas").exists());
    assert!(
        !std::fs::read_to_string(ui.join("mods.toml"))
            .expect("spec")
            .contains("75_atlas"),
        "its spec entry is gone"
    );
    assert!(
        !ui.join("mods.lock").exists(),
        "and so is its record — an empty lock leaves no file behind"
    );
    let checked = run(Action::Check).expect("check");
    assert!(checked.failure.is_none(), "{:?}", checked.json);
}

const NOT_UTF8_PAYLOAD: [u8; 4] = [0x00, 0xff, 0xfe, 0x01];

fn mod_repo(root: &Path, marker: &str) -> PathBuf {
    mod_repo_in(root, marker, "sha1")
}

fn mod_repo_in(root: &Path, marker: &str, object_format: &str) -> PathBuf {
    let repo = root.join("thurbox-widget");
    std::fs::create_dir_all(repo.join("mods")).expect("mkdir");
    std::fs::create_dir_all(repo.join("lib")).expect("mkdir");
    std::fs::create_dir_all(repo.join("bin")).expect("mkdir");

    std::fs::write(
        repo.join("mods/40_widget.lua"),
        "local util = require(\"thurbox-widget.lib.util\")\n\
         return {\n\
           name = \"widget\",\n\
           slot = \"widget\",\n\
           render = function(ctx)\n\
             local p = (thurbox and thurbox.platform) or {}\n\
             return { type = \"text\", text = util.label() .. \" \" .. tostring(p.os) }\n\
           end,\n\
         }\n",
    )
    .expect("pane");
    std::fs::write(
        repo.join("lib/util.lua"),
        format!("local u = {{}}\nfunction u.label() return \"{marker}\" end\nreturn u\n"),
    )
    .expect("module");
    std::fs::write(repo.join("bin/payload.bin"), NOT_UTF8_PAYLOAD).expect("payload");

    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            // A git hook exports these; unscrubbed, git acts on the real repository.
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    };
    git(&[
        "init",
        "--initial-branch=main",
        &format!("--object-format={object_format}"),
    ]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "T"]);
    git(&["config", "commit.gpgsign", "false"]);
    // With tag.gpgsign set, git annotates every tag and demands a message for it.
    git(&["config", "tag.gpgsign", "false"]);
    git(&["add", "-A"]);
    git(&["commit", "-m", "seed"]);
    repo
}

// git ignores `--depth` on a plain local path; a Windows drive path needs `file:///`.
fn file_url(repo: &Path) -> String {
    let path = repo.display().to_string().replace('\\', "/");
    match path.starts_with('/') {
        true => format!("file://{path}"),
        false => format!("file:///{path}"),
    }
}

fn git_in(repo: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit_more(repo: &Path, marker: &str) -> String {
    std::fs::write(
        repo.join("lib/util.lua"),
        format!("local u = {{}}\nfunction u.label() return \"{marker}\" end\nreturn u\n"),
    )
    .expect("module");
    git_in(repo, &["add", "-A"]);
    git_in(repo, &["commit", "-m", marker]);
    git_in(repo, &["rev-parse", "HEAD"])
}

#[test]
fn installing_at_a_commit_pin_checks_out_that_commit() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let repo = mod_repo(home.path(), "first");
    let pinned = git_in(&repo, &["rev-parse", "HEAD"]);
    commit_more(&repo, "second");

    let report = run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: Some(pinned.clone()),
    })
    .expect("install");
    assert!(report.failure.is_none(), "{:?}", report.json);
    assert_eq!(report.json["version"], pinned);

    assert!(
        std::fs::read_to_string(ui.join("thurbox-widget/lib/util.lua"))
            .expect("module")
            .contains("first"),
        "the working copy must be at the pin, not the tip"
    );
    let lock = thurbox::kernel::packages::read_lock(&ui).expect("lock");
    assert_eq!(
        lock.entry("thurbox-widget/mods/40_widget.lua")
            .expect("recorded")
            .version,
        pinned
    );
}

#[test]
fn an_abbreviated_commit_pin_names_the_prefix_not_a_rebase() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let repo = mod_repo(home.path(), "first");
    let short = git_in(&repo, &["rev-parse", "--short=8", "HEAD"]);

    let error = run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: Some(short.clone()),
    })
    .expect_err("an abbreviated id cannot be obtained");

    assert!(
        error.contains(&short) && error.contains("abbreviated"),
        "the error names the pin and what is wrong with it: {error}"
    );
    assert!(
        !error.contains("rebased"),
        "and must not blame a rebase that did not happen: {error}"
    );
    assert!(
        !ui.join("thurbox-widget").exists(),
        "the clone it had to take back leaves nothing behind"
    );
    assert!(
        !thurbox::kernel::packages::spec_path(&ui).exists()
            && !thurbox::kernel::packages::lock_path(&ui).exists(),
        "and nothing is recorded"
    );
}

#[test]
fn a_hex_named_tag_pin_still_installs() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let repo = mod_repo(home.path(), "first");
    git_in(&repo, &["tag", "20240115"]);
    let tagged = git_in(&repo, &["rev-parse", "HEAD"]);
    commit_more(&repo, "second");

    let report = run(Action::Install {
        src: format!("git+{}", file_url(&repo)),
        as_file: None,
        pin: Some("20240115".to_string()),
    })
    .expect("a hex-looking tag is a tag");
    assert!(report.failure.is_none(), "{:?}", report.json);
    assert_eq!(report.json["version"], tagged);
    assert!(
        std::fs::read_to_string(ui.join("thurbox-widget/lib/util.lua"))
            .expect("module")
            .contains("first"),
        "and the working copy is at the tag, not the tip"
    );
}

#[test]
fn installing_at_a_tag_pin_checks_out_the_tag() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let repo = mod_repo(home.path(), "first");
    git_in(&repo, &["tag", "v1"]);
    let tagged = git_in(&repo, &["rev-parse", "HEAD"]);
    commit_more(&repo, "second");

    let report = run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: Some("v1".to_string()),
    })
    .expect("install");
    assert!(report.failure.is_none(), "{:?}", report.json);
    assert_eq!(report.json["version"], tagged);
    assert!(
        std::fs::read_to_string(ui.join("thurbox-widget/lib/util.lua"))
            .expect("module")
            .contains("first"),
        "the working copy must be at the tag, not the tip"
    );
}

#[test]
fn installing_at_a_sha256_commit_pin_checks_out_that_commit() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let repo = mod_repo_in(home.path(), "first", "sha256");
    let pinned = git_in(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(pinned.len(), 64, "a sha256 object id: {pinned}");
    commit_more(&repo, "second");

    let report = run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: Some(pinned.clone()),
    })
    .expect("install");
    assert!(report.failure.is_none(), "{:?}", report.json);
    assert_eq!(report.json["version"], pinned);
    assert!(
        std::fs::read_to_string(ui.join("thurbox-widget/lib/util.lua"))
            .expect("module")
            .contains("first"),
        "the working copy must be at the pin, not the tip"
    );
}

#[test]
fn a_commit_pinned_spec_reproduces_on_a_fresh_machine() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let repo = mod_repo(home.path(), "first");
    let pinned = git_in(&repo, &["rev-parse", "HEAD"]);
    commit_more(&repo, "second");
    run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: Some(pinned.clone()),
    })
    .expect("install");

    std::fs::remove_dir_all(ui.join("thurbox-widget")).expect("remove");
    let fresh = run(Action::Sync).expect("sync");
    assert_eq!(fresh.json["entries"][0]["outcome"], "installed");
    assert_eq!(fresh.json["entries"][0]["version"], pinned);
    assert!(
        std::fs::read_to_string(ui.join("thurbox-widget/lib/util.lua"))
            .expect("module")
            .contains("first"),
        "a spec that reproduces the branch tip reproduces nothing"
    );
}

#[test]
fn advancing_a_commit_pinned_entry_holds_its_pin() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let repo = mod_repo(home.path(), "first");
    let pinned = git_in(&repo, &["rev-parse", "HEAD"]);
    run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: Some(pinned.clone()),
    })
    .expect("install");
    commit_more(&repo, "second");

    let report = run(Action::Update {
        name: Some("40_widget".into()),
    })
    .expect("update");
    assert!(report.failure.is_none(), "{:?}", report.json);
    assert_eq!(
        report.json["entries"][0]["outcome"], "current",
        "a pinned entry does not advance: {:?}",
        report.json
    );
    assert!(
        std::fs::read_to_string(ui.join("thurbox-widget/lib/util.lua"))
            .expect("module")
            .contains("first"),
        "and its bytes stay at the pin"
    );
}

#[test]
fn advancing_a_branch_pinned_entry_follows_the_branch() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let repo = mod_repo(home.path(), "first");
    run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: Some("main".to_string()),
    })
    .expect("install");
    let moved = commit_more(&repo, "second");

    let report = run(Action::Update {
        name: Some("40_widget".into()),
    })
    .expect("update");
    assert!(report.failure.is_none(), "{:?}", report.json);
    assert_eq!(
        report.json["entries"][0]["outcome"], "updated",
        "a branch pin follows its branch: {:?}",
        report.json
    );
    assert_eq!(report.json["entries"][0]["version"], moved);
    assert!(
        std::fs::read_to_string(ui.join("thurbox-widget/lib/util.lua"))
            .expect("module")
            .contains("second"),
        "reporting an update while the bytes stay put is the worst of both"
    );
}

#[test]
fn a_clone_that_fails_leaves_nothing_behind() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let repo = mod_repo(home.path(), "widget");

    for pin in [
        None,
        Some("0123456789abcdef0123456789abcdef01234567".to_string()),
    ] {
        let src = match &pin {
            None => format!("git+{}", home.path().join("no-such-repo").display()),
            Some(_) => format!("git+{}", repo.display()),
        };
        let pinned = pin.is_some();
        let report = run(Action::Install {
            src,
            as_file: None,
            pin,
        });
        let error = report.expect_err("the install must fail");

        match pinned {
            true => assert!(
                error.contains("cloned, but commit") && error.contains("rebased"),
                "a missing commit must not read as a failed clone: {error}"
            ),
            false => assert!(
                error.contains("clone failed"),
                "and a failed clone must say so: {error}"
            ),
        }

        assert!(
            !ui.join("thurbox-widget").exists() && !ui.join("no-such-repo").exists(),
            "no working copy, whole or partial"
        );
        assert!(
            !thurbox::kernel::packages::spec_path(&ui).exists(),
            "no spec entry"
        );
        assert!(
            !thurbox::kernel::packages::lock_path(&ui).exists(),
            "no record"
        );
    }
}

#[test]
fn installing_a_repository_delivers_its_payload_and_loads_its_pane() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let repo = mod_repo(home.path(), "widget");

    let report = run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: None,
    })
    .expect("install runs");
    assert!(report.failure.is_none(), "{:?}", report.json);

    assert!(ui.join("thurbox-widget/mods/40_widget.lua").is_file());
    assert!(ui.join("thurbox-widget/lib/util.lua").is_file());
    assert_eq!(
        std::fs::read(ui.join("thurbox-widget/bin/payload.bin")).expect("read"),
        NOT_UTF8_PAYLOAD,
        "the bytes survive — this is what the text fetch path cannot do"
    );
    assert!(
        ui.join("thurbox-widget/.git").exists(),
        "the clone keeps its .git, which is what makes update a fetch"
    );

    let lock = thurbox::kernel::packages::read_lock(&ui).expect("lock");
    let entry = lock
        .entry("thurbox-widget/mods/40_widget.lua")
        .expect("recorded");
    assert_eq!(
        entry.version.len(),
        40,
        "the lock records a full commit id, not the moving ref: {}",
        entry.version
    );

    let checked = run(Action::Check).expect("check runs");
    assert!(
        checked.json["loaded"].to_string().contains("widget"),
        "{:?}",
        checked.json
    );

    let listing = run(Action::List).expect("list runs");
    let row = listing.json["files"]
        .as_array()
        .expect("files")
        .iter()
        .find(|row| row["file"] == "thurbox-widget/mods/40_widget.lua")
        .cloned()
        .unwrap_or_else(|| panic!("the pane must be listed: {:?}", listing.json));
    assert_eq!(row["source"], "installed", "{row}");
    assert!(
        !listing.json["files"].to_string().contains("payload.bin"),
        "a repository's assets are not interface files"
    );
}

#[test]
fn an_edit_inside_a_working_copy_survives_sync_and_update() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let repo = mod_repo(home.path(), "widget");
    run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: None,
    })
    .expect("install");

    let pane = ui.join("thurbox-widget/mods/40_widget.lua");
    std::fs::write(&pane, "-- mine\nreturn {}\n").expect("edit");

    for action in [
        Action::Sync,
        Action::Update {
            name: Some("40_widget".into()),
        },
    ] {
        let report = run(action).expect("runs");
        assert!(report.failure.is_none(), "{:?}", report.json);
        assert_eq!(
            report.json["entries"][0]["outcome"], "kept",
            "a dirty working copy is reported as kept: {:?}",
            report.json
        );
        assert_eq!(
            std::fs::read_to_string(&pane).expect("read"),
            "-- mine\nreturn {}\n",
            "and is never moved"
        );
    }
}

#[test]
fn syncing_a_repository_entry_is_idempotent_and_clones_what_is_missing() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let repo = mod_repo(home.path(), "widget");
    run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: None,
    })
    .expect("install");

    let again = run(Action::Sync).expect("sync");
    assert_eq!(
        again.json["entries"][0]["outcome"], "current",
        "{:?}",
        again.json
    );
    assert_eq!(again.json["changed"], false, "{:?}", again.json);

    std::fs::remove_dir_all(ui.join("thurbox-widget")).expect("remove");
    let fresh = run(Action::Sync).expect("sync");
    assert_eq!(
        fresh.json["entries"][0]["outcome"], "installed",
        "a spec that cannot be applied on a fresh machine defeats the lock: {:?}",
        fresh.json
    );
    assert!(ui.join("thurbox-widget/bin/payload.bin").is_file());
}

#[test]
fn removing_a_repository_takes_the_whole_working_copy() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let repo = mod_repo(home.path(), "widget");
    run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: None,
    })
    .expect("install");

    std::fs::remove_dir_all(&repo).expect("delete the source");
    let removed = run(Action::Remove {
        name: "40_widget".into(),
    })
    .expect("remove");
    assert!(removed.failure.is_none(), "{:?}", removed.json);
    assert!(
        !ui.join("thurbox-widget").exists(),
        "the directory and its .git go together, not just the files the lock named"
    );
    assert!(!ui.join("mods.lock").exists());
    let checked = run(Action::Check).expect("check");
    assert!(checked.failure.is_none(), "{:?}", checked.json);
}

#[test]
fn a_cloned_repository_takes_its_pane_from_its_own_manifest() {
    let home = tempfile::tempdir().expect("tempdir");
    let _ui = interface(home.path());
    let repo = mod_repo(home.path(), "widget");

    std::fs::write(
        repo.join("mods/50_extra.lua"),
        "return { name = \"extra\", slot = \"extra\", render = function() return \"\" end }\n",
    )
    .expect("second pane");
    std::fs::write(
        repo.join("mod.toml"),
        "name = \"widget\"\n\
         pane = { source = \"mods/40_widget.lua\", path = \"mods/40_widget.lua\" }\n\
         [[module]]\n\
         source = \"lib/util.lua\"\n\
         path = \"lib/util.lua\"\n",
    )
    .expect("manifest");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .status()
            .expect("git");
    };
    git(&["add", "-A"]);
    git(&["commit", "-m", "manifest"]);

    let report = run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: None,
    })
    .expect("install runs");
    assert!(
        report.failure.is_none(),
        "the manifest resolves what auto-detection could not: {:?}",
        report.json
    );
    assert_eq!(
        report.json["file"], "thurbox-widget/mods/40_widget.lua",
        "{:?}",
        report.json
    );
    let checked = run(Action::Check).expect("check runs");
    let loaded = checked.json["loaded"].to_string();
    assert!(loaded.contains("widget"), "{loaded}");
    assert!(!loaded.contains("extra"), "{loaded}");
}

#[test]
fn a_cloned_repository_declaring_several_panes_asks_which_to_load() {
    let home = tempfile::tempdir().expect("tempdir");
    let _ui = interface(home.path());
    let repo = mod_repo(home.path(), "widget");
    std::fs::write(
        repo.join("mods/50_extra.lua"),
        "return { name = \"extra\", slot = \"extra\", render = function() return \"\" end }\n",
    )
    .expect("second pane");
    std::fs::write(
        repo.join("mod.toml"),
        "name = \"widget\"\n\
         [[pane]]\nsource = \"mods/40_widget.lua\"\npath = \"mods/40_widget.lua\"\n\
         [[pane]]\nsource = \"mods/50_extra.lua\"\npath = \"mods/50_extra.lua\"\n",
    )
    .expect("manifest");
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-m", "manifest"]);

    let error = run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: None,
    })
    .expect_err("several panes is not one entry point");
    assert!(error.contains("mods/40_widget.lua"), "{error}");
    assert!(error.contains("mods/50_extra.lua"), "{error}");
    assert!(error.contains("--as"), "and says how to choose: {error}");

    let chosen = run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: Some("mods/50_extra.lua".into()),
        pin: None,
    })
    .expect("install with --as");
    assert!(chosen.failure.is_none(), "{:?}", chosen.json);
    assert_eq!(chosen.json["file"], "thurbox-widget/mods/50_extra.lua");
}

#[test]
fn a_cloned_repository_with_a_malformed_second_pane_still_asks() {
    let home = tempfile::tempdir().expect("tempdir");
    let _ui = interface(home.path());
    let repo = mod_repo(home.path(), "widget");
    std::fs::write(
        repo.join("mod.toml"),
        "name = \"widget\"\n\
         [[pane]]\nsource = \"mods/40_widget.lua\"\npath = \"mods/40_widget.lua\"\n\
         [[pane]]\npath = \"mods/50_extra.lua\"\n",
    )
    .expect("manifest");
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-m", "manifest"]);

    let error = run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: None,
    })
    .expect_err("two declarations are not one entry point");
    assert!(error.contains("2 panes"), "{error}");
    assert!(error.contains("--as"), "{error}");
}

#[test]
fn a_repository_with_several_panes_and_no_manifest_says_so() {
    let home = tempfile::tempdir().expect("tempdir");
    let _ui = interface(home.path());
    let repo = mod_repo(home.path(), "widget");
    std::fs::write(
        repo.join("mods/50_extra.lua"),
        "return { name = \"extra\", slot = \"extra\", render = function() return \"\" end }\n",
    )
    .expect("second pane");
    for args in [vec!["add", "-A"], vec!["commit", "-m", "two"]] {
        std::process::Command::new("git")
            .args(&args)
            .current_dir(&repo)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .status()
            .expect("git");
    }

    let error = run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: None,
    })
    .expect_err("should refuse");
    assert!(
        error.contains("40_widget.lua"),
        "lists what it found: {error}"
    );
    assert!(error.contains("50_extra.lua"), "{error}");
    assert!(
        error.contains("--as"),
        "and says how to resolve it: {error}"
    );
}

#[test]
fn a_repository_read_through_plugin_toml_says_so() {
    let home = tempfile::tempdir().expect("tempdir");
    interface(home.path());
    let repo = mod_repo(home.path(), "first");
    std::fs::write(
        repo.join("plugin.toml"),
        "name = \"thurbox-widget\"\n\
         pane = { source = \"mods/40_widget.lua\", path = \"mods/40_widget.lua\" }\n",
    )
    .expect("manifest");
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-m", "manifest"]);

    let report = run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: None,
    })
    .expect("install");
    assert!(report.failure.is_none(), "{:?}", report.json);
    assert!(
        report.json["legacy_manifest"]
            .as_str()
            .is_some_and(|note| note.contains("plugin.toml")),
        "{:?}",
        report.json
    );
}

#[test]
fn an_invalid_plugin_toml_is_named_in_its_error() {
    let home = tempfile::tempdir().expect("tempdir");
    interface(home.path());
    let package = home.path().join("broken");
    std::fs::create_dir_all(&package).expect("mkdir");
    std::fs::write(
        package.join("plugin.toml"),
        "name = \"\"\npane = { source = \"a.lua\", path = \"plugins/50_a.lua\" }\n",
    )
    .expect("manifest");
    std::fs::write(package.join("a.lua"), "return {}\n").expect("pane");

    let error = run(Action::Install {
        src: package.display().to_string(),
        as_file: None,
        pin: None,
    })
    .expect_err("an invalid manifest is refused");
    assert!(error.contains("plugin.toml: name is empty"), "{error}");
    assert!(!error.contains("mod.toml"), "{error}");
}
