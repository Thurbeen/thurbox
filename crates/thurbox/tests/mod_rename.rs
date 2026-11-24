//! Interface plugins are mods. `thurbox-cli mod` is the verb; `thurbox-cli
//! plugin` and `perf --plugins` keep working for scripts written before the
//! rename, and say once on stderr what replaced them.

use std::path::Path;
use std::process::{Command, Output};

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
