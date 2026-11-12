//! The repository is one Cargo workspace, and thurbox is the member at
//! `crates/thurbox` (ADR-41): the components each live under `crates/`, and
//! everything that is not a crate (`ui/`, `examples/`, `docs/`, `vendor/`)
//! stays at the root, where the tooling that lints it looks.

use std::path::{Path, PathBuf};
use std::process::Command;

fn metadata() -> serde_json::Value {
    let out = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--offline",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run cargo metadata");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("cargo metadata is JSON")
}

fn canonical(path: &str) -> PathBuf {
    Path::new(path).canonicalize().expect("path exists")
}

#[test]
fn the_repository_root_is_a_workspace_with_thurbox_under_crates() {
    let meta = metadata();
    let crate_dir = canonical(env!("CARGO_MANIFEST_DIR"));
    let root = crate_dir
        .parent()
        .and_then(Path::parent)
        .expect("crates/thurbox has a grandparent");

    assert_eq!(
        canonical(meta["workspace_root"].as_str().unwrap()),
        root,
        "the workspace root is the repository root"
    );
    assert_eq!(crate_dir, root.join("crates").join("thurbox"));

    let thurbox = meta["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "thurbox")
        .expect("thurbox is a workspace member");
    assert_eq!(
        canonical(thurbox["manifest_path"].as_str().unwrap()),
        crate_dir.join("Cargo.toml")
    );
}

/// The interface, its examples and the patched dependencies are not part of
/// any one crate: they stay at the root, and the crate reaches them there.
#[test]
fn the_shared_trees_stay_at_the_repository_root() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for tree in ["ui", "examples", "vendor", "docs"] {
        assert!(
            root.join(tree).is_dir(),
            "{tree}/ is at the repository root"
        );
    }
    assert!(!root.join("src").exists(), "no crate sources at the root");
}

/// thurbox installs nothing into an agent's own configuration: Fleet's
/// harness profiles and `fleet signal` report a session's status (ADR-41), so
/// the extension machinery, its embedded extensions and its skill are gone.
#[test]
fn thurbox_ships_no_extensions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for gone in [
        &["extensions"][..],
        &[".agents", "skills", "thurbox-extensions"],
        &["crates", "thurbox", "src", "session", "extension_def.rs"],
        &["crates", "thurbox", "src", "agent", "extension_config.rs"],
    ] {
        let path = gone.iter().fold(root.clone(), |path, part| path.join(part));
        assert!(!path.exists(), "{} is still there", path.display());
    }
    let agents = std::fs::read_to_string(root.join("AGENTS.md")).expect("AGENTS.md");
    assert!(
        !agents.contains("thurbox-extensions"),
        "AGENTS.md still lists the extensions skill"
    );
}

/// Fleet runs every session (ADR-41), so what served thurbox's own engine
/// is gone: its session records, hook-driven status, lifecycle hooks, the
/// restore list and the pane discovery that resolved rows from before routes.
#[test]
fn the_engine_leaves_no_code_behind() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for gone in [
        &["session", "background_work.rs"][..],
        &["session", "hook_def.rs"],
        &["session", "launch_failure.rs"],
        &["session", "message.rs"],
        &["session", "op_error.rs"],
        &["session", "shared.rs"],
        &["session", "task.rs"],
        &["config", "hooks.rs"],
        &["agent", "generic.rs"],
        &["agent", "provider.rs"],
        &["kernel", "terminal", "discovery.rs"],
    ] {
        let path = gone.iter().fold(src.clone(), |path, part| path.join(part));
        assert!(!path.exists(), "{} is still there", path.display());
    }
    let restore = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("ui")
        .join("plugins")
        .join("80_restore.lua");
    assert!(!restore.exists(), "the restore list is still shipped");
}

/// thurbox keeps no database of its own (ADR-41): SQLite is out of the tree
/// of every target it builds.
///
/// The walk reads `Cargo.lock` rather than asking `cargo tree --target all`:
/// that needs every target's crates in the local cache, and a cache filled for
/// one host has only that host's. The lock records each package's resolved
/// dependencies for all targets and edge kinds, so the walk covers at least as
/// much and needs no network.
#[test]
fn rusqlite_is_not_in_thurboxs_dependency_tree() {
    let lock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    let lock = std::fs::read_to_string(lock).expect("read Cargo.lock");
    if let Some(path) = path_in_lock(&lock, "thurbox", "rusqlite") {
        panic!("rusqlite is in thurbox's tree: {}", path.join(" -> "));
    }
}

/// A registry crate and a Git copy of it can share a name and a version; the
/// walk follows the copy each entry names.
#[test]
fn the_lock_walk_tells_sources_apart() {
    let lock = r#"
        [[package]]
        name = "root"
        version = "0.1.0"
        dependencies = ["dep 1.0.0 (git+https://example.invalid/dep#abc)"]

        [[package]]
        name = "dep"
        version = "1.0.0"
        source = "registry+https://github.com/rust-lang/crates.io-index"

        [[package]]
        name = "dep"
        version = "1.0.0"
        source = "git+https://example.invalid/dep#abc"
        dependencies = ["rusqlite"]

        [[package]]
        name = "rusqlite"
        version = "0.37.0"
    "#;
    assert_eq!(
        path_in_lock(lock, "root", "rusqlite"),
        Some(vec!["root".into(), "dep".into(), "rusqlite".into()])
    );
}

/// The names on a path through `lock`'s dependency graph from package `from`
/// to package `to`, if there is one.
fn path_in_lock(lock: &str, from: &str, to: &str) -> Option<Vec<String>> {
    let lock: toml::Table = lock.parse().expect("Cargo.lock is TOML");
    let packages = lock["package"]
        .as_array()
        .expect("Cargo.lock lists packages");
    let field = |package: &toml::Value, key: &str| package.get(key)?.as_str().map(str::to_owned);
    // A dependency entry is `name`, then ` version` when the lock holds more
    // than one version of that name, then ` (source)` when one version comes
    // from more than one source.
    let find = |entry: &str| {
        let mut words = entry.splitn(3, ' ');
        let (name, version) = (words.next(), words.next());
        let source = words
            .next()
            .map(|s| s.trim_start_matches('(').trim_end_matches(')'));
        packages
            .iter()
            .position(|p| {
                field(p, "name").as_deref() == name
                    && version.is_none_or(|v| field(p, "version").as_deref() == Some(v))
                    && source.is_none_or(|s| field(p, "source").as_deref() == Some(s))
            })
            .unwrap_or_else(|| panic!("{entry} is not a package in Cargo.lock"))
    };

    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![(find(from), vec![from.to_owned()])];
    while let Some((index, path)) = stack.pop() {
        if path.last().is_some_and(|name| name == to) {
            return Some(path);
        }
        if !seen.insert(index) {
            continue;
        }
        for dep in packages[index]
            .get("dependencies")
            .and_then(|d| d.as_array())
            .into_iter()
            .flatten()
        {
            let dep = find(dep.as_str().unwrap());
            let mut path = path.clone();
            path.extend(field(&packages[dep], "name"));
            stack.push((dep, path));
        }
    }
    None
}
