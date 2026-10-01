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
//! - a name in [`RETIRED`], anywhere.
//!
//! Historical prose about code that went with v1 (`src/app`, `src/ui`) is not
//! checked: it says where something used to be, which is the point of it.

use std::path::{Path, PathBuf};

use regex::Regex;

/// Names that no longer exist and must not be cited again, each with what
/// replaced it. A name belongs here once nothing in the tree mentions it — a
/// historical mention elsewhere would fail this test, so such a name stays off.
const RETIRED: &[(&str, &str)] = &[
    ("V2-KERNEL", "docs/KERNEL.md"),
    (
        "drain_remote_hook_events",
        "Terminals::drain_hook_events (src/kernel/terminal)",
    ),
    (
        "LocalTmuxBackend",
        "the registry's default backend (backend::wiring)",
    ),
    ("LOCAL_TMUX_BACKEND_TYPE", "Route::local"),
    ("RemoteSignalTarget", "SessionBackend::hook_signal_command"),
];

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
/// another tree, and a lowercase `docs/notes.md` is a demo repository's.
const DOC_PATH: &str = r"(?:^|[^\w.~/-])(docs/[A-Z][A-Z0-9_-]*\.md|\.agents/skills/[a-z0-9-]+)";

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

/// The checks above are only as good as their patterns, so each is shown a
/// reference it must catch.
#[test]
fn the_patterns_catch_what_they_are_for() {
    let path = Regex::new(DOC_PATH).unwrap();
    let hits =
        |s: &str| -> Vec<String> { path.captures_iter(s).map(|m| m[1].to_string()).collect() };
    assert_eq!(hits("see `docs/V2-KERNEL.md`."), ["docs/V2-KERNEL.md"]);
    assert_eq!(hits("docs/KERNEL.md owns it"), ["docs/KERNEL.md"]);
    assert_eq!(
        hits("the `.agents/skills/thurbox-kernel/` skill"),
        [".agents/skills/thurbox-kernel"]
    );
    assert!(hits("~/.agents/skills/thurbox-ui/SKILL.md").is_empty());
    assert!(hits("website/docs/INDEX.md").is_empty());
    assert!(hits("a demo repo's docs/notes.md").is_empty());

    let link = Regex::new(MD_LINK).unwrap();
    let targets =
        |s: &str| -> Vec<String> { link.captures_iter(s).map(|m| m[1].to_string()).collect() };
    assert_eq!(targets("[a](gone.md)"), ["gone.md"]);
    assert_eq!(targets("[a](gone.md#part)"), ["gone.md"]);
    assert_eq!(targets("[guide](missing.md \"Guide\")"), ["missing.md"]);
}
