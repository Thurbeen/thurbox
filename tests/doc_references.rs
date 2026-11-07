//! References between the repository's documents resolve, and retired names
//! stay retired.
//!
//! A renamed document leaves its old name behind in every file that cited it,
//! and nothing else notices: a Markdown link to a missing file still renders,
//! a backticked `docs/X.md` is only text, and a skill naming a symbol that was
//! deleted reads as authoritative. This reads every tracked text file and
//! fails on:
//!
//! - a relative Markdown link whose target does not exist;
//! - a `docs/<NAME>.md` or `.agents/skills/<name>` path that does not exist,
//!   in any text file — a doc comment, a skill, a workflow, the website;
//! - a `github.com/Thurbeen/thurbox/blob/main/<path>` link whose path does not
//!   exist here;
//! - a `src/<path>` in a Markdown file that does not exist, unless it is
//!   another project's ([`FOREIGN_SOURCES`]);
//! - a name in [`RETIRED`], anywhere;
//! - a stated minimum Rust version other than `Cargo.toml`'s `rust-version`.
//!
//! Historical prose about code that went with v1 (`src/app`, `src/ui`) is not
//! checked: it says where something used to be, which is the point of it.

use std::path::{Path, PathBuf};

use regex::Regex;

/// Names that no longer exist and must not be cited again, each with what
/// replaced it. A name belongs here once nothing in the tree mentions it — a
/// historical mention elsewhere would fail this test, so such a name stays off.
const RETIRED: &[(&str, &str)] = &[
    ("V2-KERNEL", "docs/thurbox/kernel.md"),
    (
        "drain_remote_hook_events",
        "Terminals::drain_hook_events (src/kernel/terminal)",
    ),
    (
        "LocalTmuxBackend",
        "the registry's default backend (backend::wiring)",
    ),
    ("LOCAL_TMUX_BACKEND_TYPE", "Route::local"),
    ("RemoteSignalTarget", "StatusChannel::hook_signal_command"),
];

/// `src/` paths that Markdown cites from another project's tree, not this one.
const FOREIGN_SOURCES: &[&str] = &[
    // psmux's, where it picks the shell a window command runs through.
    "src/pane.rs",
];

/// A source path in this repository, not the tail of a longer one
/// (`vendor/crossterm/src/…`). It ends on a name character, so a sentence's
/// full stop or a directory's trailing slash is not part of it.
const SRC_PATH: &str = r"(?:^|[^\w.~/-])(src/[\w./-]*\w)";

/// A minimum Rust version as prose states one: `MSRV: 1.95,`, `MSRV is 1.95.`,
/// `Rust 1.95+`. A toolchain a measurement ran on (`Rust 1.99.1`) is not one.
const MSRV_CLAIM: &str = r"(?:MSRV(?: is)?:? |Rust )(1\.\d+)[+,.](?:\D|$)";

/// Tracked paths never read: fixtures that are deliberately old or broken,
/// and `.claude/skills`, whose entries are links into `.agents/skills` (read
/// under that name).
const SKIP_PREFIXES: &[&str] = &["tests/fixtures/", ".claude/"];

/// The location variables git exports to hook processes: a suite run from
/// this repository's own pre-commit hook would otherwise ask another index.
const GIT_LOCATION_ENV: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
    "GIT_NAMESPACE",
];

const TEXT_EXTENSIONS: &[&str] = &[
    "md", "rs", "html", "toml", "lua", "sh", "bats", "yml", "yaml", "js", "mjs", "py", "nix",
    "json",
];

/// A document's path in the repository, not the tail of a longer one:
/// `~/.agents/skills/…` is a user's install location, `website/docs/…` is
/// another tree, and a `docs/notes.md` outside a component directory is a
/// demo repository's. A document lives at `docs/README.md` or under one
/// component's directory (`docs/thurbox/`, `docs/fleet/`, `docs/runtime/`);
/// an upper-case `docs/<NAME>.md` is still matched, so a reference to a
/// document from before the component layout fails rather than passes.
const DOC_PATH: &str = r"(?:^|[^\w.~/-])(docs/[A-Z][A-Z0-9_-]*\.md|docs/(?:thurbox|fleet|runtime)/[\w/.-]*\w\.md|\.agents/skills/[a-z0-9-]+)";

/// A Markdown inline link's target: up to whitespace, a fragment or the
/// closing parenthesis, whatever title follows it.
const MD_LINK: &str = r"\]\(([^)\s#]+)[^)]*\)";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The tracked files, relative to the root. Tracked rather than walked, so a
/// scratch file or build output in an ignored directory cannot fail the suite.
fn tracked() -> Vec<String> {
    let mut git = std::process::Command::new("git");
    git.args(["ls-files", "-z"]).current_dir(root());
    for var in GIT_LOCATION_ENV {
        git.env_remove(var);
    }
    let out = git.output().expect("run git ls-files");
    assert!(
        out.status.success(),
        "git ls-files failed — this test reads a git checkout: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("utf-8 paths")
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect()
}

/// Every text file but this one, whose examples are the stale references the
/// checks look for, as (path relative to the root, contents).
fn texts() -> Vec<(String, String)> {
    let this = file!().replace('\\', "/");
    tracked()
        .into_iter()
        .filter(|rel| *rel != this && !SKIP_PREFIXES.iter().any(|skip| rel.starts_with(skip)))
        .filter(|rel| {
            let path = Path::new(rel);
            rel == "justfile"
                || path
                    .extension()
                    .is_some_and(|ext| TEXT_EXTENSIONS.contains(&ext.to_string_lossy().as_ref()))
        })
        .filter_map(|rel| {
            let text = std::fs::read_to_string(root().join(&rel)).ok()?;
            Some((rel, text))
        })
        .collect()
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count() + 1
}

#[test]
fn every_relative_markdown_link_resolves() {
    let link = Regex::new(MD_LINK).unwrap();
    let mut broken = Vec::new();
    for (rel, text) in texts().iter().filter(|(rel, _)| rel.ends_with(".md")) {
        let dir = root().join(rel);
        let dir = dir.parent().expect("a file has a directory");
        for m in link.captures_iter(text) {
            let target = &m[1];
            if target.contains("://") || target.starts_with("mailto:") {
                continue;
            }
            if !dir.join(target).exists() {
                let at = line_of(text, m.get(1).unwrap().start());
                broken.push(format!("{rel}:{at}: {target}"));
            }
        }
    }
    assert!(
        broken.is_empty(),
        "Markdown links to files that do not exist:\n  {}",
        broken.join("\n  ")
    );
}

#[test]
fn every_document_path_names_a_document() {
    let path = Regex::new(DOC_PATH).unwrap();
    let blob =
        Regex::new(r"github\.com/Thurbeen/thurbox/(?:blob|tree)/main/([\w.][^\s\x22'<>)#`]+)")
            .unwrap();
    let mut missing = Vec::new();
    for (rel, text) in texts() {
        let found = path
            .captures_iter(&text)
            .chain(blob.captures_iter(&text))
            .map(|m| m.get(1).unwrap());
        for m in found {
            let target = m.as_str().trim_end_matches('.');
            if !root().join(target).exists() {
                missing.push(format!("{rel}:{}: {target}", line_of(&text, m.start())));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "references to documents that do not exist:\n  {}",
        missing.join("\n  ")
    );
}

/// Documents name source files to send a reader there, so a file that moved
/// strands them. v1's `src/app` and `src/ui` are history, and history keeps
/// its old paths.
#[test]
fn every_source_path_in_a_document_exists() {
    let path = Regex::new(SRC_PATH).unwrap();
    let mut missing = Vec::new();
    for (rel, text) in texts().iter().filter(|(rel, _)| rel.ends_with(".md")) {
        for m in path.captures_iter(text).map(|m| m.get(1).unwrap()) {
            let target = m.as_str();
            // By directory, so `src/ui_control.rs` is still checked.
            let v1 = ["src/app", "src/ui"].iter().any(|dir| {
                target == *dir
                    || target
                        .strip_prefix(dir)
                        .is_some_and(|rest| rest.starts_with('/'))
            });
            if v1 {
                continue;
            }
            if !FOREIGN_SOURCES.contains(&target) && !root().join(target).exists() {
                missing.push(format!("{rel}:{}: {target}", line_of(text, m.start())));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "source paths that do not exist:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn retired_names_stay_retired() {
    let mut cited = Vec::new();
    for (rel, text) in texts() {
        for (name, replaced_by) in RETIRED {
            for (offset, _) in text.match_indices(name) {
                cited.push(format!(
                    "{rel}:{}: `{name}` is gone — cite {replaced_by}",
                    line_of(&text, offset)
                ));
            }
        }
    }
    assert!(
        cited.is_empty(),
        "retired names cited:\n  {}",
        cited.join("\n  ")
    );
}

/// The supported Rust is whatever the manifest declares. A copy anywhere else
/// drifts the moment a dependency raises the floor, as `1.75` did for years
/// after the lockfile needed 1.95.
#[test]
fn every_stated_rust_version_is_the_manifests() {
    let manifest = std::fs::read_to_string(root().join("Cargo.toml")).expect("read Cargo.toml");
    let declared = Regex::new(r#"(?m)^rust-version = "([\d.]+)""#)
        .unwrap()
        .captures(&manifest)
        .expect("Cargo.toml declares rust-version")[1]
        .to_string();
    let stated = Regex::new(MSRV_CLAIM).unwrap();
    let mut wrong = Vec::new();
    for (rel, text) in texts() {
        for m in stated.captures_iter(&text).map(|m| m.get(1).unwrap()) {
            if m.as_str() != declared {
                let at = line_of(&text, m.start());
                wrong.push(format!(
                    "{rel}:{at}: {} (Cargo.toml: {declared})",
                    m.as_str()
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "minimum Rust versions that are not Cargo.toml's:\n  {}",
        wrong.join("\n  ")
    );
}

/// The checks above are only as good as their patterns, so each is shown a
/// reference it must catch.
#[test]
fn the_patterns_catch_what_they_are_for() {
    let path = Regex::new(DOC_PATH).unwrap();
    let hits =
        |s: &str| -> Vec<String> { path.captures_iter(s).map(|m| m[1].to_string()).collect() };
    assert_eq!(hits("see `docs/V2-KERNEL.md`."), ["docs/V2-KERNEL.md"]);
    assert_eq!(hits("docs/thurbox/kernel.md owns it"), ["docs/thurbox/kernel.md"]);
    assert_eq!(
        hits("see docs/fleet/controllers/task.md."),
        ["docs/fleet/controllers/task.md"]
    );
    assert_eq!(hits("docs/KERNEL.md was its name"), ["docs/KERNEL.md"]);
    assert_eq!(
        hits("the `.agents/skills/thurbox-kernel/` skill"),
        [".agents/skills/thurbox-kernel"]
    );
    assert!(hits("~/.agents/skills/thurbox-ui/SKILL.md").is_empty());
    assert!(hits("website/docs/INDEX.md").is_empty());
    assert!(hits("a demo repo's docs/notes.md").is_empty());

    let src = Regex::new(SRC_PATH).unwrap();
    let sources =
        |s: &str| -> Vec<String> { src.captures_iter(s).map(|m| m[1].to_string()).collect() };
    assert_eq!(
        sources("see `src/storage/schema.rs`."),
        ["src/storage/schema.rs"]
    );
    assert_eq!(sources("in src/kernel/ itself"), ["src/kernel"]);
    assert!(sources("vendor/crossterm/src/event.rs").is_empty());
    assert!(sources("a crate's ~/src/lib.rs").is_empty());

    let msrv = Regex::new(MSRV_CLAIM).unwrap();
    let claims =
        |s: &str| -> Vec<String> { msrv.captures_iter(s).map(|m| m[1].to_string()).collect() };
    assert_eq!(claims("- MSRV: 1.75, Edition 2021"), ["1.75"]);
    assert_eq!(claims("MSRV is Rust 1.75, Edition 2021"), ["1.75"]);
    assert_eq!(claims("Requires Rust 1.75+."), ["1.75"]);
    assert_eq!(claims("`map_or(true, ..)`: MSRV 1.75."), ["1.75"]);
    assert_eq!(claims("this crate's MSRV is 1.75.\n"), ["1.75"]);
    assert!(claims("Measured with Rust 1.99.1, cargo-nextest").is_empty());

    let link = Regex::new(MD_LINK).unwrap();
    let targets =
        |s: &str| -> Vec<String> { link.captures_iter(s).map(|m| m[1].to_string()).collect() };
    assert_eq!(targets("[a](gone.md)"), ["gone.md"]);
    assert_eq!(targets("[a](gone.md#part)"), ["gone.md"]);
    assert_eq!(targets("[guide](missing.md \"Guide\")"), ["missing.md"]);
}
