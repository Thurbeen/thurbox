use std::path::{Path, PathBuf};

use regex::Regex;

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

const FOREIGN_SOURCES: &[&str] = &[
    // psmux's, where it picks the shell a window command runs through.
    "src/pane.rs",
];

const SRC_PATH: &str = r"(?:^|[^\w.~/-])(src/[\w./-]*\w)";

const MSRV_CLAIM: &str = r"(?:MSRV(?: is)?:? |Rust )(1\.\d+)[+,.](?:\D|$)";

const DECISION: &str = r"\b(ADR-\d+[a-z]?)\b";

const PERF_DECISION: &str = r"\b(ADR-P\d+)\b";

const SKIP_PREFIXES: &[&str] = &["crates/thurbox/tests/fixtures/", ".claude/"];

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

const DOC_PATH: &str = r"(?:^|[^\w.~/-])(docs/[A-Z][A-Z0-9_-]*\.md|docs/(?:thurbox|fleet|runtime)/[\w/.-]*\w\.md|\.agents/skills/[a-z0-9-]+)";

const MD_LINK: &str = r"\]\(([^)\s#]+)[^)]*\)";

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

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
            if !FOREIGN_SOURCES.contains(&target) && !crate_root().join(target).exists() {
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

#[test]
fn every_cited_decision_is_in_architecture_md() {
    let architecture = std::fs::read_to_string(root().join("docs/thurbox/architecture.md"))
        .expect("read architecture.md");
    let decisions: Vec<String> = Regex::new(r"(?m)^## (ADR-\d+[a-z]?):")
        .unwrap()
        .captures_iter(&architecture)
        .map(|m| m[1].to_string())
        .collect();
    let cited = Regex::new(DECISION).unwrap();
    assert_eq!(
        cited
            .captures_iter("ADR-8b, ADR-41 and ADR-P6")
            .map(|m| m[1].to_string())
            .collect::<Vec<_>>(),
        ["ADR-8b", "ADR-41"]
    );
    let mut dangling = Vec::new();
    for (rel, text) in texts() {
        for m in cited.captures_iter(&text).map(|m| m.get(1).unwrap()) {
            if !decisions.iter().any(|d| d == m.as_str()) {
                let at = line_of(&text, m.start());
                dangling.push(format!("{rel}:{at}: {}", m.as_str()));
            }
        }
    }
    assert!(
        dangling.is_empty(),
        "decisions cited that docs/thurbox/architecture.md does not have:\n  {}",
        dangling.join("\n  ")
    );
}

#[test]
fn every_cited_performance_decision_is_in_performance_md() {
    let performance = std::fs::read_to_string(root().join("docs/thurbox/performance.md"))
        .expect("read performance.md");
    let decisions: Vec<String> = Regex::new(r"(?m)^## (ADR-P\d+):")
        .unwrap()
        .captures_iter(&performance)
        .map(|m| m[1].to_string())
        .collect();
    let cited = Regex::new(PERF_DECISION).unwrap();
    assert_eq!(
        cited
            .captures_iter("ADR-8b, ADR-P16 and ADR-P2x")
            .map(|m| m[1].to_string())
            .collect::<Vec<_>>(),
        ["ADR-P16"]
    );
    let mut dangling = Vec::new();
    for (rel, text) in texts() {
        for m in cited.captures_iter(&text).map(|m| m.get(1).unwrap()) {
            if !decisions.iter().any(|d| d == m.as_str()) {
                let at = line_of(&text, m.start());
                dangling.push(format!("{rel}:{at}: {}", m.as_str()));
            }
        }
    }
    assert!(
        dangling.is_empty(),
        "performance decisions cited that docs/thurbox/performance.md does not have:\n  {}",
        dangling.join("\n  ")
    );
}

#[test]
fn every_stated_rust_version_is_the_manifests() {
    let manifest =
        std::fs::read_to_string(root().join("crates/thurbox/Cargo.toml")).expect("read Cargo.toml");
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

#[test]
fn the_patterns_catch_what_they_are_for() {
    let path = Regex::new(DOC_PATH).unwrap();
    let hits =
        |s: &str| -> Vec<String> { path.captures_iter(s).map(|m| m[1].to_string()).collect() };
    assert_eq!(hits("see `docs/V2-KERNEL.md`."), ["docs/V2-KERNEL.md"]);
    assert_eq!(
        hits("docs/thurbox/kernel.md owns it"),
        ["docs/thurbox/kernel.md"]
    );
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
