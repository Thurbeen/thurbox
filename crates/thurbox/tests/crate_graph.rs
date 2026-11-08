//! The workspace's crate graph against `crates/architecture.toml`.
//!
//! The file declares every workspace crate and the workspace crates it may
//! depend on — the component boundaries of `docs/README.md`, one level above
//! the module rules `architecture_rules.rs` holds inside thurbox. This test
//! reads the real graph from `cargo metadata` and fails when a member has no
//! entry, an entry names no member, or a member depends on a workspace crate
//! its entry does not list. Fleet's `fleet-arch` checks the same file once
//! Fleet is in the repository.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/thurbox sits two levels under the workspace root")
        .to_path_buf()
}

/// `crates/architecture.toml`: each crate's declared workspace dependencies.
fn declared() -> BTreeMap<String, BTreeSet<String>> {
    let path = workspace_root().join("crates/architecture.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let table: toml::Table = text.parse().expect("architecture.toml parses");
    let crates = table
        .get("crates")
        .and_then(toml::Value::as_table)
        .expect("architecture.toml has a [crates] table");
    crates
        .iter()
        .map(|(name, entry)| {
            let entry = entry.as_table().expect("each crate entry is a table");
            assert!(
                entry.get("role").and_then(toml::Value::as_str).is_some(),
                "crate `{name}` has no role"
            );
            let deps = entry
                .get("deps")
                .and_then(toml::Value::as_array)
                .unwrap_or_else(|| panic!("crate `{name}` has no deps list"))
                .iter()
                .map(|d| d.as_str().expect("a dep is a crate name").to_string())
                .collect();
            (name.clone(), deps)
        })
        .collect()
}

/// The workspace members and, for each, the members it depends on in any
/// section (normal, dev or build), from `cargo metadata --no-deps`.
fn actual() -> BTreeMap<String, BTreeSet<String>> {
    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--offline",
        ])
        .current_dir(workspace_root())
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata is JSON");
    let packages = metadata["packages"].as_array().expect("packages");
    let members: BTreeSet<String> = packages
        .iter()
        .map(|p| p["name"].as_str().expect("package name").to_string())
        .collect();
    packages
        .iter()
        .map(|p| {
            let deps = p["dependencies"]
                .as_array()
                .expect("dependencies")
                .iter()
                .filter_map(|d| d["name"].as_str())
                .filter(|d| members.contains(*d))
                .map(str::to_string)
                .collect();
            (p["name"].as_str().unwrap().to_string(), deps)
        })
        .collect()
}

#[test]
fn every_workspace_crate_depends_only_on_what_architecture_toml_declares() {
    let declared = declared();
    let actual = actual();
    let mut problems = Vec::new();
    for name in actual.keys() {
        if !declared.contains_key(name) {
            problems.push(format!("member `{name}` has no entry"));
        }
    }
    for name in declared.keys() {
        if !actual.contains_key(name) {
            problems.push(format!("entry `{name}` is not a workspace member"));
        }
    }
    for (name, deps) in &actual {
        let allowed = declared.get(name).cloned().unwrap_or_default();
        for dep in deps.difference(&allowed) {
            problems.push(format!(
                "`{name}` depends on `{dep}`, which its entry does not list"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "crates/architecture.toml disagrees with the workspace:\n  {}",
        problems.join("\n  ")
    );
}

/// The multiplexer runtime is its own crate, and thurbox reaches it as a
/// dependency rather than holding the code.
#[test]
fn the_multiplexer_runtime_is_a_crate_thurbox_depends_on() {
    let actual = actual();
    assert!(
        actual.contains_key("runtime-mux"),
        "runtime-mux is not a workspace member: {actual:?}"
    );
    assert!(
        actual["thurbox"].contains("runtime-mux"),
        "thurbox does not depend on runtime-mux"
    );
    assert!(
        actual["runtime-mux"].is_empty(),
        "runtime-mux depends on another workspace crate: {:?}",
        actual["runtime-mux"]
    );
}
