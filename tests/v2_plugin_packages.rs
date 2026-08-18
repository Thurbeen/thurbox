//! Acquiring a pane without a terminal.
//!
//! Every test installs from a **local** package directory, so the suite needs no
//! network and nothing is pinned to what the repository's examples happen to
//! contain today. Local, URL and bare-name sources resolve through the same function; what
//! differs is only where a bare name points, which is covered by a unit test
//! beside the resolver.
//!
//! `THURBOX_UI_DIR` is set per test. nextest runs a process per test, so the
//! override cannot leak between them.

use std::path::{Path, PathBuf};

use thurbox::cli::plugins::{run, Action};

/// Point the interface directory at a fresh tempdir, with the bundled interface
/// delivered — a plugin `require`s `lib/`, so a bare directory is not one yet.
fn interface(dir: &Path) -> PathBuf {
    let ui = dir.join("ui");
    std::fs::create_dir_all(&ui).expect("mkdir");
    std::env::set_var("THURBOX_UI_DIR", &ui);
    thurbox::kernel::bundled::materialize(&ui);
    ui
}

/// A package: a pane that requires a module of its own, so delivery, namespacing
/// and `require` are all exercised by whether it loads.
fn package(root: &Path, name: &str, version: &str, marker: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join("plugin.toml"),
        format!(
            "name = \"{name}\"\n\
             description = \"a test pane\"\n\
             version = \"{version}\"\n\
             pane = {{ source = \"pane.lua\", path = \"plugins/75_{name}.lua\" }}\n\
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

// ── what we distribute ─────────────────────────────────────────────────────

/// Every example package in `ui-plugins/` installs and loads.
///
/// Installed from the checkout rather than by bare name, which is the same code
/// path with a local source — the suite must not need the network, and a bare name
/// resolves to the *published* tag, which by definition does not yet contain what
/// is being changed here.
///
/// `ui-plugins/` is not bundled, so without this nothing notices it rotting: a
/// renamed snapshot field would leave an example looking fine and failing the moment
/// somebody installed it — and an example that does not work is worse than none.
#[test]
fn every_distributed_package_installs_and_loads() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui-plugins");
    let mut names: Vec<String> = std::fs::read_dir(&root)
        .expect("ui-plugins/")
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
        // The manifest is what says where the pane goes; a package that delivered
        // nowhere would install "successfully" and leave the directory unchanged.
        let file = report.json["file"].as_str().unwrap_or_default();
        assert!(ui.join(file).is_file(), "{name} delivered no pane");
    }

    // They load together, which is the real test: a package that requires a
    // renamed module or reads a renamed snapshot field fails here.
    let checked = run(Action::Check).expect("check runs");
    let loaded = checked.json["loaded"].to_string();
    for name in &names {
        assert!(
            loaded.contains(name.as_str()),
            "{name} did not load: {loaded}"
        );
    }

    // And the listing names them, since that is what a bare-name install and a typo
    // suggestion are resolved against.
    let listed: Vec<&str> = thurbox::kernel::packages::EXAMPLE_PLUGINS
        .iter()
        .map(|(name, _)| *name)
        .collect();
    for name in &names {
        assert!(
            listed.contains(&name.as_str()),
            "{name} is in ui-plugins/ but absent from EXAMPLE_PLUGINS, so it cannot \
             be installed by name and a typo suggests nothing: {listed:?}"
        );
    }
    assert_eq!(
        listed.len(),
        names.len(),
        "and nothing is listed that is not there"
    );
}

// ── acquiring ──────────────────────────────────────────────────────────────

#[test]
fn a_package_installs_its_pane_and_its_own_modules() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = package(home.path(), "atlas", "v0.3.1", "atlas");

    let report = install(&src);
    assert!(report.failure.is_none(), "{:?}", report.json);
    assert_eq!(report.json["outcome"], "installed", "{:?}", report.json);
    assert_eq!(report.json["version"], "v0.3.1", "{:?}", report.json);
    assert!(ui.join("plugins/75_atlas.lua").is_file());
    assert!(
        ui.join("lib/atlas/util.lua").is_file(),
        "a package's shared module lands in a namespace of its own"
    );

    // The spec records the source, the destination and what it resolved to.
    let spec = std::fs::read_to_string(ui.join("plugins.toml")).expect("spec");
    assert!(spec.contains("plugins/75_atlas.lua"), "{spec}");
    assert!(spec.contains("atlas"), "{spec}");
    let lock = std::fs::read_to_string(ui.join("plugins.lock")).expect("lock");
    assert!(lock.contains("v0.3.1"), "{lock}");
    assert!(lock.contains("lib/atlas/util.lua"), "{lock}");

    // And it loads — which is the only real proof the namespaced `require`
    // resolved, since the pane requires its own module at load time.
    let checked = run(Action::Check).expect("check runs");
    assert!(
        checked.json["loaded"].to_string().contains("atlas"),
        "{:?}",
        checked.json
    );
}

#[test]
fn installing_says_what_to_add_to_the_arrangement() {
    // The one failure with no symptom: a pane that loads and draws nothing. The
    // instruction has to arrive before anyone goes looking for it.
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

    // No manifest means no proposed destination, and guessing one would put a pane
    // somewhere nobody asked for.
    let refused = run(Action::Install {
        src: loose.display().to_string(),
        as_file: None,
        pin: None,
    });
    let error = refused.expect_err("should refuse");
    assert!(error.contains("--as"), "and says how to give one: {error}");

    let done = run(Action::Install {
        src: loose.display().to_string(),
        as_file: Some("plugins/90_notes.lua".into()),
        pin: None,
    })
    .expect("install");
    assert!(done.failure.is_none(), "{:?}", done.json);
    assert!(ui.join("plugins/90_notes.lua").is_file());
    // Nothing to pin to, and saying so is better than inventing a version.
    assert_eq!(done.json["version"], "unpinned", "{:?}", done.json);
}

#[test]
fn a_destination_already_holding_an_unmanaged_file_is_refused() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = package(home.path(), "atlas", "v1", "atlas");
    let occupied = ui.join("plugins/10_sessions.lua");
    let before = std::fs::read_to_string(&occupied).expect("read");

    let error = run(Action::Install {
        src: src.display().to_string(),
        as_file: Some("plugins/10_sessions.lua".into()),
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
        !ui.join("plugins.toml").exists(),
        "and nothing is recorded either"
    );
}

#[test]
fn a_module_may_not_be_delivered_outside_its_own_namespace() {
    // `lib/theme.lua` is a namespace with one tenant. A package free to write there
    // could replace a module every other pane requires.
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let theme = ui.join("lib/theme.lua");
    let before = std::fs::read_to_string(&theme).expect("read");

    let src = home.path().join("bad");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(
        src.join("plugin.toml"),
        "name = \"bad\"\n\
         pane = { source = \"pane.lua\", path = \"plugins/95_bad.lua\" }\n\
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

// ── converging ─────────────────────────────────────────────────────────────

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
    // The reproduce-elsewhere case: the spec and the lock, with nothing delivered.
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = package(home.path(), "atlas", "v1", "atlas");
    install(&src);

    std::fs::remove_file(ui.join("plugins/75_atlas.lua")).expect("remove");
    std::fs::remove_file(ui.join("lib/atlas/util.lua")).expect("remove");
    // A genuinely fresh machine has the spec and the lock and no delivered files.
    // Its lock records versions but no digests — and that must read as "never
    // delivered here", not as "the user deleted these", or nothing is ever
    // installed from a checked-in lock.
    let lock = thurbox::kernel::packages::read_lock(&ui).expect("lock");
    let mut fresh = thurbox::session::PluginLock::default();
    for entry in lock.plugins {
        fresh.record(thurbox::session::LockEntry {
            files: Default::default(),
            removed: Vec::new(),
            ..entry
        });
    }
    thurbox::kernel::packages::write_lock(&ui, &fresh).expect("write");

    let report = run(Action::Sync).expect("sync");
    assert_eq!(report.json["changed"], true, "{:?}", report.json);
    assert!(ui.join("plugins/75_atlas.lua").is_file());
    assert!(ui.join("lib/atlas/util.lua").is_file());
}

#[test]
fn syncing_takes_back_what_the_spec_no_longer_lists() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = package(home.path(), "atlas", "v1", "atlas");
    install(&src);

    // The hand edit the spec exists for.
    std::fs::write(ui.join("plugins.toml"), "").expect("empty the spec");

    let report = run(Action::Sync).expect("sync");
    assert_eq!(
        report.json["entries"][0]["outcome"], "removed",
        "{:?}",
        report.json
    );
    assert!(!ui.join("plugins/75_atlas.lua").exists());
    assert!(
        !ui.join("lib/atlas").exists(),
        "and the namespace it owned goes with it"
    );
}

#[test]
fn syncing_leaves_a_pane_the_spec_never_listed_alone() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let mine = ui.join("plugins/90_mine.lua");
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

    let pane = ui.join("plugins/75_atlas.lua");
    std::fs::write(&pane, "-- mine\nreturn {}\n").expect("edit");
    // Upstream moves under them, which is when a manager is most tempted to win.
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
    std::fs::remove_file(ui.join("plugins/75_atlas.lua")).expect("delete");

    for _ in 0..2 {
        let report = run(Action::Sync).expect("sync");
        assert_eq!(
            report.json["entries"][0]["outcome"], "deleted",
            "{:?}",
            report.json
        );
        assert!(
            !ui.join("plugins/75_atlas.lua").exists(),
            "convergence must not undo a removal"
        );
    }
}

// ── moving a pin, and removing ─────────────────────────────────────────────

#[test]
fn updating_reports_the_version_it_came_from() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let src = package(home.path(), "atlas", "v0.3.1", "atlas");
    install(&src);

    // Nothing newer yet: success, not a failure — an update that exits non-zero on
    // "already current" is one nobody can schedule.
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

    // Everything removal needs is in the record, so a source that has gone away is
    // not a reason to be stuck with the pane.
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
    assert!(!ui.join("plugins/75_atlas.lua").exists());
    assert!(!ui.join("lib/atlas").exists());
    assert!(
        !std::fs::read_to_string(ui.join("plugins.toml"))
            .expect("spec")
            .contains("75_atlas"),
        "its spec entry is gone"
    );
    assert!(
        !ui.join("plugins.lock").exists(),
        "and so is its record — an empty lock leaves no file behind"
    );
    // The interface still loads with the pane gone.
    let checked = run(Action::Check).expect("check");
    assert!(checked.failure.is_none(), "{:?}", checked.json);
}

// ── a plugin that carries more than Lua ────────────────────────────────────

/// Build a throwaway plugin **repository**: a pane in a nested directory, a module
/// beside it, and a payload no text path could carry.
fn plugin_repo(root: &Path, marker: &str) -> PathBuf {
    let repo = root.join("thurbox-widget");
    std::fs::create_dir_all(repo.join("plugins")).expect("mkdir");
    std::fs::create_dir_all(repo.join("lib")).expect("mkdir");
    std::fs::create_dir_all(repo.join("bin")).expect("mkdir");

    // The pane requires a module by the repo-relative path `require` already
    // resolves, and reads the platform to find its own payload.
    std::fs::write(
        repo.join("plugins/40_widget.lua"),
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
    // Bytes that are not valid UTF-8: the exact thing the text fetch path corrupts.
    std::fs::write(repo.join("bin/payload.bin"), [0x00u8, 0xff, 0xfe, 0x01]).expect("payload");

    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            // The pre-commit hook exports these; without scrubbing them a test's git
            // call lands in the real repository.
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "--initial-branch=main"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "T"]);
    git(&["config", "commit.gpgsign", "false"]);
    git(&["add", "-A"]);
    git(&["commit", "-m", "seed"]);
    repo
}

/// The whole point: a repository install delivers Lua in the author's layout AND
/// bytes beside it, and the pane loads.
#[test]
fn installing_a_repository_delivers_its_payload_and_loads_its_pane() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let repo = plugin_repo(home.path(), "widget");

    // A local path that ends in `.git`? No — the form has to be explicit, so the
    // `git+` prefix is what says "clone this".
    let report = run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: None,
    })
    .expect("install runs");
    assert!(report.failure.is_none(), "{:?}", report.json);

    // Delivered in the layout the author chose, payload included.
    assert!(ui.join("thurbox-widget/plugins/40_widget.lua").is_file());
    assert!(ui.join("thurbox-widget/lib/util.lua").is_file());
    assert_eq!(
        std::fs::read(ui.join("thurbox-widget/bin/payload.bin")).expect("read"),
        [0x00u8, 0xff, 0xfe, 0x01],
        "the bytes survive — this is what the text fetch path cannot do"
    );
    assert!(
        ui.join("thurbox-widget/.git").exists(),
        "the clone keeps its .git, which is what makes update a fetch"
    );

    // The lock records the COMMIT, not the ref: `main` moves, a commit does not.
    let lock = thurbox::kernel::packages::read_lock(&ui).expect("lock");
    let entry = lock
        .entry("thurbox-widget/plugins/40_widget.lua")
        .expect("recorded");
    assert_eq!(
        entry.version.len(),
        40,
        "a full commit id: {}",
        entry.version
    );

    // And the pane LOADS, from outside `plugins/`, requiring its own module.
    let checked = run(Action::Check).expect("check runs");
    assert!(
        checked.json["loaded"].to_string().contains("widget"),
        "{:?}",
        checked.json
    );

    // The inventory accounts for it, with the origin its entry names.
    let listing = run(Action::List).expect("list runs");
    let row = listing.json["files"]
        .as_array()
        .expect("files")
        .iter()
        .find(|row| row["file"] == "thurbox-widget/plugins/40_widget.lua")
        .cloned()
        .unwrap_or_else(|| panic!("the pane must be listed: {:?}", listing.json));
    assert_eq!(row["source"], "installed", "{row}");
    // The rest of the working copy is deliberately not walked.
    assert!(
        !listing.json["files"].to_string().contains("payload.bin"),
        "a repository's assets are not interface files"
    );
}

/// git owns "your edits are yours" for a working copy, and this is the property
/// that makes keeping `.git` worth its cost.
#[test]
fn an_edit_inside_a_working_copy_survives_sync_and_update() {
    let home = tempfile::tempdir().expect("tempdir");
    let ui = interface(home.path());
    let repo = plugin_repo(home.path(), "widget");
    run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: None,
    })
    .expect("install");

    let pane = ui.join("thurbox-widget/plugins/40_widget.lua");
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
    let repo = plugin_repo(home.path(), "widget");
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

    // A fresh machine: the spec and lock are there, the working copy is not.
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
    let repo = plugin_repo(home.path(), "widget");
    run(Action::Install {
        src: format!("git+{}", repo.display()),
        as_file: None,
        pin: None,
    })
    .expect("install");

    // Offline: everything removal needs is on disk.
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
    assert!(!ui.join("plugins.lock").exists());
    let checked = run(Action::Check).expect("check");
    assert!(checked.failure.is_none(), "{:?}", checked.json);
}

/// A repository with several panes names its entry point in its own manifest — so a
/// manifest is not irrelevant to a cloned plugin, and one repository serves both
/// install mechanisms rather than presenting two doors that behave differently.
#[test]
fn a_cloned_repository_takes_its_pane_from_its_own_manifest() {
    let home = tempfile::tempdir().expect("tempdir");
    let _ui = interface(home.path());
    let repo = plugin_repo(home.path(), "widget");

    // A second pane makes auto-detection ambiguous, and a manifest that resolves it.
    std::fs::write(
        repo.join("plugins/50_extra.lua"),
        "return { name = \"extra\", slot = \"extra\", render = function() return \"\" end }\n",
    )
    .expect("second pane");
    // Modules at `lib/…` inside its own tree — legitimate for a clone, and something
    // the copy-model rules would reject, which is why the manifest is read leniently.
    std::fs::write(
        repo.join("plugin.toml"),
        "name = \"widget\"\n\
         pane = { source = \"plugins/40_widget.lua\", path = \"plugins/40_widget.lua\" }\n\
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
        report.json["file"], "thurbox-widget/plugins/40_widget.lua",
        "{:?}",
        report.json
    );
    // And only that one is loaded as a pane; the other is present, not claimed.
    let checked = run(Action::Check).expect("check runs");
    let loaded = checked.json["loaded"].to_string();
    assert!(loaded.contains("widget"), "{loaded}");
    assert!(!loaded.contains("extra"), "{loaded}");
}

#[test]
fn a_repository_with_several_panes_and_no_manifest_says_so() {
    let home = tempfile::tempdir().expect("tempdir");
    let _ui = interface(home.path());
    let repo = plugin_repo(home.path(), "widget");
    std::fs::write(
        repo.join("plugins/50_extra.lua"),
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

    // Guessing which is the entry point would be a silent wrong answer.
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
