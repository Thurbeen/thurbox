//! The repository is one Cargo workspace, and thurbox is the member at
//! `crates/thurbox` (ADR-41): the components each live under `crates/`, and
//! everything that is not a crate (`ui/`, `extensions/`, `docs/`, `vendor/`)
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

/// The interface, its extensions and the patched dependencies are not part of
/// any one crate: they stay at the root, and the crate reaches them there.
#[test]
fn the_shared_trees_stay_at_the_repository_root() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for tree in ["ui", "extensions", "examples", "vendor", "docs"] {
        assert!(
            root.join(tree).is_dir(),
            "{tree}/ is at the repository root"
        );
    }
    assert!(!root.join("src").exists(), "no crate sources at the root");
}

/// thurbox keeps no database of its own (ADR-41): SQLite is out of the tree
/// of every target it builds.
#[test]
fn rusqlite_is_not_in_thurboxs_dependency_tree() {
    let out = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--offline",
            "--edges",
            "normal,build,dev",
            "--target",
            "all",
            "-p",
            "thurbox",
            "-i",
            "rusqlite",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run cargo tree");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && stderr.contains("did not match any packages"),
        "rusqlite is still in the tree:\n{}{stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
}
