//! The git worktree workspace adapter against real git repositories.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use fleet_api::Observation;
use fleet_api::plan::EffectOutcome;
use fleet_api::task::{RepositoriesWorkspace, Repository};
use fleet_api::workspace::Workspaces;
use fleet_runtime::{Repositories, WorktreeConfig, WorktreeWorkspaces};

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn sh(dir: &Path, args: &[&str]) {
    let mut git = std::process::Command::new("git");
    for var in fleet_runtime::worktree::GIT_LOCATION {
        git.env_remove(var);
    }
    let out = git.arg("-C").arg(dir).args(args).output().expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A git checkout at `src` with one commit on `main`.
fn source(src: &Path) {
    fs::create_dir_all(src).unwrap();
    sh(src, &["init", "-q", "-b", "main"]);
    sh(
        src,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.test",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "first",
        ],
    );
}

/// A source checkout with one commit on `main`, and an adapter over it.
fn setup(name: &str) -> (PathBuf, WorktreeWorkspaces, RepositoriesWorkspace) {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("worktree")
        .join(format!("{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let src = base.join("src");
    source(&src);
    let repos = Repositories::new(BTreeMap::from([(
        "https://example.test/app".to_string(),
        text(&src),
    )]));
    let ws = WorktreeWorkspaces::new(WorktreeConfig::new(base.join("workspaces"), repos));
    let named = RepositoriesWorkspace {
        repositories: vec![Repository {
            name: "app".into(),
            url: "https://example.test/app".into(),
            reference: Some("main".into()),
        }],
    };
    (base, ws, named)
}

#[test]
fn a_workspace_is_made_once_found_by_its_key_and_cleaned() {
    let (base, ws, named) = setup("lifecycle");
    assert_eq!(ws.find("local", "t.u1"), Observation::Known(None));
    let Observation::Known(EffectOutcome::Materialized { workspace, adopted }) =
        ws.materialize("local", "t.u1", &named)
    else {
        panic!("not materialized");
    };
    assert!(!adopted);
    assert!(Path::new(&workspace.dir).join(".git").exists());
    assert_eq!(
        workspace.trust_root.as_deref(),
        Some(text(&base.join("src")).as_str())
    );
    assert_eq!(
        ws.find("local", "t.u1"),
        Observation::Known(Some((*workspace).clone()))
    );
    // A second materialize adopts the whole one and runs no worktree add.
    assert_eq!(
        ws.materialize("local", "t.u1", &named),
        Observation::Known(EffectOutcome::Materialized {
            workspace: workspace.clone(),
            adopted: true
        })
    );
    assert_eq!(
        ws.clean(&workspace),
        Observation::Known(EffectOutcome::Deleted)
    );
    assert!(!base.join("workspaces/t.u1").exists());
    assert_eq!(ws.find("local", "t.u1"), Observation::Known(None));
    assert_eq!(
        ws.clean(&workspace),
        Observation::Known(EffectOutcome::Deleted)
    );
    // The branch, and whatever the worker committed on it, stays.
    sh(
        &base.join("src"),
        &["rev-parse", "--verify", "refs/heads/fleet/t.u1"],
    );
}

#[test]
fn a_partial_workspace_is_none_and_the_next_materialize_rolls_it_back() {
    let (base, ws, named) = setup("partial");
    // A crash after the worktree add, before the marker.
    let dir = base.join("workspaces/t.u1");
    fs::create_dir_all(&dir).unwrap();
    sh(
        &base.join("src"),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "fleet/t.u1",
            &text(&dir.join("app")),
            "main",
        ],
    );
    assert_eq!(ws.find("local", "t.u1"), Observation::Known(None));
    let Observation::Known(EffectOutcome::Materialized {
        workspace,
        adopted: false,
    }) = ws.materialize("local", "t.u1", &named)
    else {
        panic!("not materialized");
    };
    assert_eq!(
        ws.find("local", "t.u1"),
        Observation::Known(Some(*workspace))
    );
}

#[test]
fn an_unmapped_repository_or_a_remote_host_runs_nothing() {
    let (_base, ws, mut named) = setup("refusals");
    named.repositories[0].url = "https://example.test/other".into();
    assert_eq!(
        ws.materialize("local", "t.u1", &named),
        Observation::Known(EffectOutcome::Unmapped {
            url: "https://example.test/other".into()
        })
    );
    assert!(matches!(
        ws.materialize("remote", "t.u1", &named),
        Observation::Known(EffectOutcome::Failed { .. })
    ));
    assert!(matches!(
        ws.find("remote", "t.u1"),
        Observation::Unknown { .. }
    ));
    assert!(matches!(
        ws.materialize("local", "../t", &named),
        Observation::Known(EffectOutcome::Failed { .. })
    ));
}

#[test]
fn a_refused_ref_leaves_nothing_behind() {
    let (base, ws, mut named) = setup("bad-ref");
    named.repositories[0].reference = Some("no-such-ref".into());
    assert!(matches!(
        ws.materialize("local", "t.u1", &named),
        Observation::Known(EffectOutcome::Failed { .. })
    ));
    assert!(!base.join("workspaces/t.u1").exists());
}

#[test]
fn every_repository_after_the_first_is_named_as_an_extra_dir() {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("worktree")
        .join(format!("multi-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    source(&base.join("app"));
    source(&base.join("docs"));
    let repos = Repositories::new(BTreeMap::from([
        (
            "https://example.test/app".to_string(),
            text(&base.join("app")),
        ),
        (
            "https://example.test/docs".to_string(),
            text(&base.join("docs")),
        ),
    ]));
    let ws = WorktreeWorkspaces::new(WorktreeConfig::new(base.join("workspaces"), repos));
    let named = RepositoriesWorkspace {
        repositories: ["app", "docs"]
            .map(|n| Repository {
                name: n.into(),
                url: format!("https://example.test/{n}"),
                reference: None,
            })
            .to_vec(),
    };
    let Observation::Known(EffectOutcome::Materialized { workspace, .. }) =
        ws.materialize("local", "t.u1", &named)
    else {
        panic!("not materialized");
    };
    let made = base.join("workspaces/t.u1");
    assert_eq!(workspace.dir, text(&made.join("app")));
    assert_eq!(workspace.extra_dirs, vec![text(&made.join("docs"))]);
    assert!(made.join("docs/.git").exists());
    assert_eq!(
        ws.find("local", "t.u1"),
        Observation::Known(Some(*workspace))
    );
}

#[test]
fn a_workspace_whose_marker_predates_repository_names_is_cleaned_with_its_work_kept() {
    let (base, ws, named) = setup("old-marker");
    let Observation::Known(EffectOutcome::Materialized { workspace, .. }) =
        ws.materialize("local", "t.u1", &named)
    else {
        panic!("not materialized");
    };
    // A marker an earlier Fleet wrote: its worktrees carry no name.
    let marker = base.join("workspaces/t.u1/.fleet-workspace.json");
    let mut old: serde_json::Value = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
    for w in old["worktrees"].as_array_mut().unwrap() {
        w.as_object_mut().unwrap().remove("name");
    }
    fs::write(&marker, old.to_string()).unwrap();
    fs::write(Path::new(&workspace.dir).join("draft.txt"), "work\n").unwrap();

    assert_eq!(
        ws.clean(&workspace),
        Observation::Known(EffectOutcome::Deleted)
    );
    sh(
        &base.join("src"),
        &["rev-parse", "--verify", "refs/fleet/preserved/t.u1/app"],
    );
}
