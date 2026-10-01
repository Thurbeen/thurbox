//! References between the repository's documents resolve, and retired names
//! stay retired.
//!
//! A renamed document leaves its old name behind in every file that cited it,
//! and nothing else notices: a Markdown link to a missing file still renders,
//! a backticked `docs/X.md` is only text, and a skill naming a symbol that was
//! deleted reads as authoritative. This walks every tracked-looking text file
//! and fails on:
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

/// Directories never walked: build output, dependencies, fixtures that are
/// deliberately old or broken, and `.claude/skills`, whose entries are links
/// into `.agents/skills` (walked under that name).
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "_site",
    ".direnv",
    ".lavish",
    ".claude",
];

const TEXT_EXTENSIONS: &[&str] = &[
    "md", "rs", "html", "toml", "lua", "sh", "bats", "yml", "yaml", "js", "mjs", "py", "nix",
    "json",
];

/// A document's path in the repository, not the tail of a longer one:
/// `~/.agents/skills/…` is a user's install location, `website/docs/…` is
/// another tree, and a lowercase `docs/notes.md` is a demo repository's.
const DOC_PATH: &str = r"(?:^|[^\w.~/-])(docs/[A-Z][A-Z0-9_-]*\.md|\.agents/skills/[a-z0-9-]+)";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            let fixtures = name == "fixtures" && dir.ends_with("tests");
            if !SKIP_DIRS.contains(&name.as_ref()) && !fixtures {
                walk(&path, found);
            }
        } else if name == "justfile"
            || path
                .extension()
                .is_some_and(|ext| TEXT_EXTENSIONS.contains(&ext.to_string_lossy().as_ref()))
        {
            found.push(path);
        }
    }
}

/// Every text file but this one, whose examples are the stale references the
/// checks look for, as (path relative to the root, contents).
fn texts() -> Vec<(String, String)> {
    let root = root();
    let this = root.join(file!());
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();
    files
        .into_iter()
        .filter(|path| *path != this)
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let rel = path.strip_prefix(&root).ok()?.display().to_string();
            Some((rel, text))
        })
        .collect()
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count() + 1
}

#[test]
fn every_relative_markdown_link_resolves() {
    let link = Regex::new(r"\]\(([^)\s#]+)(?:#[^)]*)?\)").unwrap();
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
}
