use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};

const DEPRECATED_VERB: &str = "`thurbox-cli plugin` is deprecated; use `thurbox-cli mod`";
const DEPRECATED_FLAG: &str = "`perf --plugins` is deprecated; use `perf --mods`";

fn cli(root: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
    cmd.args(args)
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("THURBOX_CONFIG_DIR", root.join("thurbox-config"))
        .env("THURBOX_DATA_DIR", root.join("thurbox-data"));
    for var in [
        "THURBOX_UI_DIR",
        "THURBOX_SOCKET",
        "THURBOX_SESSION",
        "FLEET_CONFIG",
        "TMUX",
    ] {
        cmd.env_remove(var);
    }
    cmd.output().expect("run thurbox-cli")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn mod_is_the_verb_and_plugin_still_answers_with_a_deprecation() {
    let root = tempfile::tempdir().expect("tempdir");
    for sub in [["dir"], ["list"], ["events"]] {
        let new = cli(root.path(), &[&["mod"], &sub[..]].concat());
        let old = cli(root.path(), &[&["plugin"], &sub[..]].concat());
        assert!(new.status.success(), "mod {sub:?}: {}", text(&new.stderr));
        assert_eq!(new.status.code(), old.status.code(), "{sub:?}");
        assert_eq!(text(&new.stdout), text(&old.stdout), "{sub:?}");
        assert!(!text(&new.stderr).contains("deprecated"), "{sub:?}");
        assert_eq!(
            text(&old.stderr).matches(DEPRECATED_VERB).count(),
            1,
            "{sub:?}: {}",
            text(&old.stderr)
        );
    }
}

#[test]
fn help_offers_mod_and_hides_plugin() {
    let root = tempfile::tempdir().expect("tempdir");
    let help = text(&cli(root.path(), &["--help"]).stdout);
    assert!(help.contains("\n  mod "), "{help}");
    assert!(!help.contains("\n  plugin "), "{help}");
    let perf = text(&cli(root.path(), &["perf", "--help"]).stdout);
    assert!(perf.contains("--mods"), "{perf}");
    assert!(!perf.contains("--plugins"), "{perf}");
}

#[test]
fn perf_plugins_is_perf_mods_with_a_deprecation() {
    let root = tempfile::tempdir().expect("tempdir");
    let new = cli(root.path(), &["perf", "--mods", "--json"]);
    let old = cli(root.path(), &["perf", "--plugins", "--json"]);
    assert_eq!(new.status.code(), old.status.code());
    assert_eq!(text(&new.stdout), text(&old.stdout));
    assert!(!text(&new.stderr).contains("deprecated"));
    assert_eq!(text(&old.stderr).matches(DEPRECATED_FLAG).count(), 1);
}

const MIGRATION_NOTICE: &str = "plugins/ → mods/";
const STARTER: &str = include_str!("../../../examples/lua/plugin.lua");

fn digest(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn config_dir(root: &Path) -> PathBuf {
    root.join("thurbox-config")
}

fn ui_dir(root: &Path) -> PathBuf {
    config_dir(root).join("ui")
}

fn key(root: &Path, relative: &str) -> String {
    ui_dir(root).join(relative).to_string_lossy().into_owned()
}

fn as_an_old_release_left_it(ui: &Path) {
    if ui.join("mods").is_dir() {
        std::fs::rename(ui.join("mods"), ui.join("plugins")).expect("rename");
    }
    let manifest = ui.join(".bundled.json");
    let old = std::fs::read_to_string(&manifest)
        .expect("manifest")
        .replace("\"mods/", "\"plugins/");
    std::fs::write(&manifest, old).expect("manifest");
}

fn old_layout(root: &Path) {
    let made = cli(root, &["mod", "new", "mine"]);
    assert!(made.status.success(), "{}", text(&made.stderr));
    let ui = ui_dir(root);
    as_an_old_release_left_it(&ui);

    let agent = ui.join("plugins/20_agent.lua");
    let edited = std::fs::read_to_string(&agent).expect("agent") + "-- mine\n";
    std::fs::write(&agent, edited).expect("edit");

    std::fs::remove_file(ui.join("plugins/64_menu.lua")).expect("delete");
    let manifest = ui.join(".bundled.json");
    let mut records: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).expect("manifest")).expect("json");
    records["plugins/64_menu.lua"] = json!({ "removed": true });
    std::fs::write(&manifest, records.to_string()).expect("tombstone");

    let atlas = STARTER.replace("example", "atlas");
    std::fs::write(ui.join("plugins/75_atlas.lua"), &atlas).expect("atlas");
    std::fs::write(
        ui.join("plugins.toml"),
        "# my panes\n\n[[plugin]]\nsrc = \"atlas\"\nfile = \"plugins/75_atlas.lua\"\n",
    )
    .expect("spec");
    std::fs::write(
        ui.join("plugins.lock"),
        format!(
            "[[plugin]]\nsrc = \"atlas\"\nfile = \"plugins/75_atlas.lua\"\nresolved = \"x\"\n\
             version = \"v1\"\n\n[plugin.files]\n\"plugins/75_atlas.lua\" = \"{}\"\n",
            digest(&atlas)
        ),
    )
    .expect("lock");

    let mine = key(root, "plugins/90_mine.lua");
    std::fs::write(
        config_dir(root).join("plugin-state.json"),
        json!({ "trusted": { &mine: "feedface" }, "disabled": [&mine] }).to_string(),
    )
    .expect("state");
}

fn listed(output: &Output) -> BTreeMap<String, Value> {
    assert!(output.status.success(), "{}", text(&output.stderr));
    let report: Value = serde_json::from_slice(&output.stdout).expect("json");
    report["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|row| (row["file"].as_str().expect("file").to_string(), row.clone()))
        .collect()
}

fn assert_migrated(root: &Path, files: &BTreeMap<String, Value>) {
    let ui = ui_dir(root);
    assert!(
        files.keys().all(|file| !file.starts_with("plugins")),
        "{:?}",
        files.keys()
    );
    assert_eq!(files["mods/90_mine.lua"]["source"], "user");
    assert_eq!(files["mods/90_mine.lua"]["state"], "disabled");
    assert_eq!(files["mods/20_agent.lua"]["source"], "edited");
    assert_eq!(files["mods/64_menu.lua"]["source"], "removed");
    assert_eq!(files["mods/10_sessions.lua"]["source"], "bundled");
    assert_eq!(files["mods/75_atlas.lua"]["source"], "installed");
    assert_eq!(files["mods/75_atlas.lua"]["entry"], "atlas");
    assert!(files.contains_key("mods.toml"), "{:?}", files.keys());

    for old in ["plugins", "plugins.toml", "plugins.lock"] {
        assert!(!ui.join(old).exists(), "{old} is still there");
    }
    assert!(!config_dir(root).join("plugin-state.json").exists());

    let spec = std::fs::read_to_string(ui.join("mods.toml")).expect("mods.toml");
    assert!(spec.contains("# my panes"), "{spec}");
    assert!(spec.contains("[[mod]]"), "{spec}");
    assert!(spec.contains("file = \"mods/75_atlas.lua\""), "{spec}");
    let lock = std::fs::read_to_string(ui.join("mods.lock")).expect("mods.lock");
    assert!(lock.contains("mods/75_atlas.lua"), "{lock}");
    assert!(!lock.contains("plugins/"), "{lock}");

    let state: Value = serde_json::from_str(
        &std::fs::read_to_string(config_dir(root).join("mod-state.json")).expect("mod-state"),
    )
    .expect("json");
    let mine = key(root, "mods/90_mine.lua");
    assert_eq!(state["trusted"][&mine], "feedface", "{state}");
    assert_eq!(state["disabled"], json!([mine]), "{state}");
}

#[test]
fn an_old_layout_moves_to_mods_keeping_trust_disabled_edits_and_installs() {
    let root = tempfile::tempdir().expect("tempdir");
    old_layout(root.path());

    let first = cli(root.path(), &["mod", "list", "--json"]);
    assert_eq!(
        text(&first.stderr).matches(MIGRATION_NOTICE).count(),
        1,
        "{}",
        text(&first.stderr)
    );
    assert_migrated(root.path(), &listed(&first));

    let second = cli(root.path(), &["mod", "list", "--json"]);
    assert!(
        !text(&second.stderr).contains("→ mods"),
        "{}",
        text(&second.stderr)
    );
    assert_migrated(root.path(), &listed(&second));
}

#[test]
fn the_deprecated_plugin_verb_migrates_an_old_layout_too() {
    let root = tempfile::tempdir().expect("tempdir");
    old_layout(root.path());

    let output = cli(root.path(), &["plugin", "list", "--json"]);
    assert_eq!(
        text(&output.stderr).matches(MIGRATION_NOTICE).count(),
        1,
        "{}",
        text(&output.stderr)
    );
    assert_migrated(root.path(), &listed(&output));
}

#[test]
fn config_validate_reads_the_migrated_mod_state() {
    let root = tempfile::tempdir().expect("tempdir");
    old_layout(root.path());

    let output = cli(root.path(), &["config", "validate", "--json"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr).matches(MIGRATION_NOTICE).count(), 1);
    let report: Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(report["mod_state_json"]["exists"], true, "{report}");
    assert!(config_dir(root.path()).join("mod-state.json").is_file());
    assert!(!ui_dir(root.path()).join("plugins").exists());
}

#[test]
fn a_clash_stays_in_plugins_and_is_reported() {
    let root = tempfile::tempdir().expect("tempdir");
    old_layout(root.path());
    let ui = ui_dir(root.path());
    std::fs::create_dir_all(ui.join("mods")).expect("mods");
    std::fs::write(ui.join("mods/90_mine.lua"), "-- newer\n").expect("clash");

    let output = cli(root.path(), &["mod", "list", "--json"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("plugins/90_mine.lua"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(ui.join("mods/90_mine.lua")).expect("kept"),
        "-- newer\n"
    );
    assert!(ui.join("plugins/90_mine.lua").is_file());
    assert!(ui.join("mods/75_atlas.lua").is_file());
    assert!(!ui.join("plugins/75_atlas.lua").exists());
}

#[test]
fn a_package_with_only_plugin_toml_installs_into_mods() {
    let root = tempfile::tempdir().expect("tempdir");
    let package = root.path().join("legacy");
    std::fs::create_dir_all(&package).expect("package");
    std::fs::write(
        package.join("plugin.toml"),
        "name = \"legacy\"\npane = { source = \"legacy.lua\", path = \"plugins/88_legacy.lua\" }\n",
    )
    .expect("manifest");
    std::fs::write(
        package.join("legacy.lua"),
        STARTER.replace("example", "legacy"),
    )
    .expect("pane");

    let output = cli(root.path(), &["mod", "install", &package.to_string_lossy()]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let said = format!("{}{}", text(&output.stdout), text(&output.stderr));
    assert!(said.contains("plugin.toml"), "{said}");
    let ui = ui_dir(root.path());
    assert!(ui.join("mods/88_legacy.lua").is_file());
    assert!(!ui.join("plugins").exists());
    let spec = std::fs::read_to_string(ui.join("mods.toml")).expect("mods.toml");
    assert!(spec.contains("file = \"mods/88_legacy.lua\""), "{spec}");
}

#[test]
fn an_as_plugins_destination_lands_in_mods() {
    let root = tempfile::tempdir().expect("tempdir");
    let single = root.path().join("single.lua");
    std::fs::write(&single, STARTER.replace("example", "single")).expect("pane");

    let output = cli(
        root.path(),
        &[
            "mod",
            "install",
            &single.to_string_lossy(),
            "--as",
            "plugins/89_single.lua",
        ],
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    let ui = ui_dir(root.path());
    assert!(ui.join("mods/89_single.lua").is_file());
    assert!(!ui.join("plugins").exists());
}

#[test]
fn a_migration_that_cannot_save_leaves_the_old_layout_and_refuses() {
    let root = tempfile::tempdir().expect("tempdir");
    old_layout(root.path());
    let ui = ui_dir(root.path());
    let manifest = ui.join(".bundled.json");
    let mut permissions = std::fs::metadata(&manifest)
        .expect("metadata")
        .permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&manifest, permissions).expect("read-only");

    for verb in ["mod", "plugin"] {
        let output = cli(root.path(), &[verb, "list", "--json"]);
        assert!(!output.status.success(), "{verb}: {}", text(&output.stdout));
        assert!(
            text(&output.stderr).contains("could not finish moving plugins to mods"),
            "{verb}: {}",
            text(&output.stderr)
        );
    }
    assert!(ui.join("plugins/90_mine.lua").is_file());
    assert!(ui.join("plugins/20_agent.lua").is_file());
    assert!(!ui.join("mods").exists());
    assert!(ui.join("plugins.toml").is_file());
    assert!(config_dir(root.path()).join("plugin-state.json").is_file());
}
