//! thurbox's own architecture checks, beside the module rules and path
//! restrictions in `crates/architecture.toml` that `arch-rules` checks: the
//! runtime is named only through its gateways, `session` stays pure data, and
//! each kernel worker lives in a file of its own.

#[allow(dead_code)]
#[path = "architecture/resolver.rs"]
mod resolver;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use resolver::{strip_comments_and_strings, Tree};

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// The real tree, parsed once for every test in this file.
fn src_tree() -> &'static Tree {
    static TREE: OnceLock<Tree> = OnceLock::new();
    TREE.get_or_init(|| Tree::load(&src_root()))
}

/// The modules that may name `runtime_mux` itself: the `backend` gateway, and
/// the ones re-exporting a runtime type as their own — the shell grammar
/// (`shell`), the host model and `SessionId`
/// (`session`), the data directory and binary lookup (`paths`), the hosts
/// loader (`config::hosts`) and the TOML reader (`config::file`). The resolver
/// follows only crate-internal paths, so a `runtime_mux::tmux` named anywhere
/// else would reach an adapter past [`consumers_reach_no_concrete_backend`].
const RUNTIME_GATEWAYS: &[&str] = &[
    "backend",
    "shell",
    "session",
    "paths",
    "config::hosts",
    "config::file",
];

/// Each site outside `gateways` that names `runtime_mux` by path or imports it
/// under any name, as `dir/file.rs:line`, test code included.
fn runtime_outside_gateways(tree: &Tree, gateways: &[&str]) -> Vec<String> {
    let mut found = tree.mentions(&["runtime_mux"], "::", true);
    found.extend(tree.imports(&["runtime_mux"], true));
    let mut problems: Vec<String> = found
        .into_iter()
        .filter(|(file, _, _)| {
            let mut module: Vec<String> = file
                .with_extension("")
                .iter()
                .map(|part| part.to_string_lossy().into_owned())
                .collect();
            if module.last().is_some_and(|last| last == "mod") {
                module.pop();
            }
            let module = module.join("::");
            !gateways
                .iter()
                .any(|g| module == *g || module.starts_with(&format!("{g}::")))
        })
        .map(|(file, line, what)| {
            let file: Vec<_> = file.iter().map(|part| part.to_string_lossy()).collect();
            format!("{}:{line} {what}", file.join("/"))
        })
        .collect();
    problems.sort();
    problems.dedup();
    problems
}

/// Every crossing into the runtime goes through a gateway, so the resolver —
/// which sees crate-internal paths only — judges it: a module naming
/// `runtime_mux` itself, under any alias, would bypass every rule above.
#[test]
fn runtime_mux_is_named_only_by_its_gateways() {
    let problems = runtime_outside_gateways(src_tree(), RUNTIME_GATEWAYS);
    assert!(
        problems.is_empty(),
        "reach the runtime through crate::backend, or extend RUNTIME_GATEWAYS with why:\n  {}",
        problems.join("\n  ")
    );
}

/// Stripping keeps every newline of every source file, which is what lets a
/// violation's byte offset name its line. A `\` line continuation inside a
/// string once swallowed one, and every report below it pointed a line early.
#[test]
fn stripping_keeps_every_line() {
    // Fixed inputs first, so the property is pinned even once no file under
    // `src/` happens to hold a continuation.
    for src in [
        "let s = \"a \\\nb\";\ncrate::x",
        "let s = \"a \\\r\nb\";\r\ncrate::x",
        "let s = r#\"a\nb\"#; /* c\nd */ // e\ncrate::x",
    ] {
        assert_eq!(
            strip_comments_and_strings(src).matches('\n').count(),
            src.matches('\n').count(),
            "stripping {src:?} lost or added a line"
        );
    }
    for file in collect_files_with_extension(&src_root(), "rs") {
        let content = fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()));
        assert_eq!(
            strip_comments_and_strings(&content).matches('\n').count(),
            content.matches('\n').count(),
            "stripping {} lost or added a line",
            file.display()
        );
    }
}

// ---------------------------------------------------------------------------
// The resolver on fixture trees: each is a tiny `src/` under
// `tests/fixtures/architecture/`, never compiled, holding one shape the
// checks below must see through.
// ---------------------------------------------------------------------------

fn fixture(name: &str) -> Tree {
    Tree::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/architecture")
            .join(name),
    )
}

fn edge_set(tree: &Tree, nodes: &[&str], production_only: bool) -> BTreeSet<(String, String)> {
    tree.edges(nodes)
        .into_iter()
        .filter(|e| !(production_only && e.test))
        .map(|e| (e.from, e.to))
        .collect()
}

/// Aliases and re-exports are edges: a `type` alias of an adapter's type is a
/// crossing where it is declared, a use of that alias from elsewhere resolves
/// to the adapter, and so does a `pub use` re-export read through its parent:
/// what the concrete-backend check relies on.
#[test]
fn aliases_and_reexports_launder_nothing() {
    let tree = fixture("laundering");
    let found: BTreeSet<String> = tree
        .edges(&["agent", "agent::tmux", "kernel"])
        .iter()
        .filter(|e| e.from == "kernel" && e.to == "agent::tmux")
        .map(|e| format!("{} → {} @ {}", e.from, e.target(), e.site(&tree.root)))
        .collect();
    let expected: BTreeSet<String> = [
        // The alias itself, declared in kernel.
        "kernel → agent::tmux::Index @ kernel/mod.rs:1",
        // The alias used from another kernel file resolves through it.
        "kernel → agent::tmux::Index @ kernel/other.rs:2",
        // The parent's `pub use … as` re-export, read by path.
        "kernel → agent::tmux::Index @ kernel/other.rs:3",
        // A glob-free brace import holding `self`, then used by its binding.
        "kernel → agent::tmux @ kernel/other.rs:4",
        "kernel → agent::tmux::spawn @ kernel/other.rs:6",
        // A re-export whose path starts at an imported name (`adapter`).
        "kernel → agent::tmux::spawn @ kernel/other.rs:8",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    assert_eq!(found, expected);
}

/// `#[cfg(test)]` code is marked — an item, an inline module and a file
/// declared under `#[cfg(test)] mod x;` — so the multiplexer-variant check can
/// leave a test's own pinning out.
#[test]
fn test_code_makes_no_production_edge() {
    let tree = fixture("test_edges");
    let nodes = ["a", "b"];
    assert_eq!(
        edge_set(&tree, &nodes, true),
        [("a".to_string(), "b".to_string())].into_iter().collect()
    );
    assert_eq!(
        tree.edges(&nodes).iter().filter(|e| e.test).count(),
        4,
        "{:?}",
        tree.edges(&nodes)
    );
}

/// The external crates `session` may name: data formats and value types. A
/// palette in `ratatui` colours is a rendering decision, a `tokio` handle or a
/// `rusqlite` row is someone else's runtime, and none of them belongs in the
/// sink every other node depends on. `architecture_rules` resolves only
/// crate-internal edges, so this is how an external one is governed (#1477).
const SESSION_CRATES: &[&str] = &[
    "chrono",
    "chrono_tz",
    "cron",
    // The host model, the hyperlink table and `SessionId`, which the runtime
    // defines and a session is described in: `session` only re-exports them.
    "runtime_mux",
    "serde",
    "serde_json",
    "toml",
    "toml_edit",
    "unicode_width",
    "uuid",
];

/// Process state `session` may not hold, in its tests either: a value it
/// answers from somewhere other than its arguments makes the sink an owner of
/// the process. Settings are passed in by whoever loaded them, the local
/// platform's simulation lives with the code that decides by it, and an
/// environment variable is read by the caller (#1477).
const SESSION_STATE: &[&str] = &["thread_local!", "OnceLock", "LazyLock", "static mut"];
const SESSION_ENV: &[&str] = &["env::"];

/// Every crate `Cargo.toml` depends on, spelled as a path names it.
fn dependency_crates() -> Vec<String> {
    let manifest: toml::Table =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("read Cargo.toml")
            .parse()
            .expect("parse Cargo.toml");
    let mut tables = vec![manifest.get("dependencies")];
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        tables.extend(targets.values().map(|t| t.get("dependencies")));
    }
    let mut crates: Vec<String> = tables
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_table)
        .flat_map(|t| t.keys().map(|k| k.replace('-', "_")))
        .collect();
    crates.sort();
    crates.dedup();
    crates
}

/// Each site in `session`, as `dir/file.rs:line what` with `/` on every
/// platform, so a fixture's expectation reads the same on Windows.
fn in_session(found: Vec<(PathBuf, usize, &str)>) -> Vec<String> {
    found
        .into_iter()
        .filter(|(file, _, _)| file.starts_with("session"))
        .map(|(file, line, what)| {
            let file: Vec<_> = file.iter().map(|part| part.to_string_lossy()).collect();
            format!("{}:{line} {what}", file.join("/"))
        })
        .collect()
}

/// What keeps `session` from being pure data in `tree`: a crate of `foreign`
/// named by path or imported under any name, process state, or an environment
/// read.
fn session_impurities(tree: &Tree, foreign: &[&str]) -> Vec<String> {
    let mut problems = in_session(tree.mentions(foreign, "::", true));
    problems.extend(in_session(tree.imports(foreign, true)));
    problems.extend(in_session(tree.mentions(SESSION_STATE, "", true)));
    problems.extend(in_session(tree.mentions(SESSION_ENV, "", false)));
    problems.sort();
    problems.dedup();
    problems
}

/// `session` is the dependency sink and documented as pure data: it names
/// only the external crates [`SESSION_CRATES`] lists, holds no process state
/// and reads no environment.
#[test]
fn session_is_pure_data() {
    let crates = dependency_crates();
    assert!(
        crates.iter().any(|c| c == "ratatui"),
        "read the manifest: {crates:?}"
    );
    let foreign: Vec<&str> = crates
        .iter()
        .map(String::as_str)
        .filter(|c| !SESSION_CRATES.contains(c))
        .collect();
    let problems = session_impurities(src_tree(), &foreign);
    assert!(
        problems.is_empty(),
        "session must stay pure data — move these out, or extend SESSION_CRATES with why:\n  {}",
        problems.join("\n  ")
    );
}

/// A consumer naming the runtime directly — by path, or through an alias of
/// an adapter or of the crate — is found; a gateway, a comment and a string
/// are not.
#[test]
fn runtime_access_outside_the_gateways_is_found() {
    assert_eq!(
        runtime_outside_gateways(&fixture("runtime_imports"), &["backend", "session"]),
        [
            "kernel/mod.rs:1 runtime_mux",
            "kernel/mod.rs:2 runtime_mux",
            "kernel/mod.rs:7 runtime_mux"
        ]
    );
}

/// A crate imported under another name is still that crate: `use ratatui as
/// rendering;` leaves no `ratatui::` behind for the path scan to find.
#[test]
fn session_purity_sees_a_renamed_import() {
    assert_eq!(
        session_impurities(&fixture("session_imports"), &["ratatui"]),
        [
            "session/mod.rs:1 ratatui",
            "session/mod.rs:8 ratatui",
            "session/mod.rs:9 ratatui"
        ]
    );
}

/// The scan behind [`session_is_pure_data`] counts `#[cfg(test)]` code only
/// when asked to: state a module's tests hold is still state that module holds.
#[test]
fn the_mention_scan_takes_test_code_only_when_asked() {
    let tree = fixture("in_place_writes");
    let sites = |with_tests| -> Vec<String> {
        tree.mentions(&["fs::write"], "(", with_tests)
            .iter()
            .map(|(file, line, _)| format!("{}:{line}", file.display()))
            .collect()
    };
    assert_eq!(sites(false), ["config.rs:5", "config.rs:9"]);
    assert_eq!(sites(true), ["config.rs:5", "config.rs:9", "config.rs:15"]);
}

/// Every persisted proptest seed must still name a source file that exists.
///
/// Proptest's persisted failure-seed store has a layout that is a path contract
/// rather than incidental, and the default `FileFailurePersistence::
/// SourceParallel` writes to *two* places depending on the source file:
///
/// - It climbs the source file's ancestors for a directory holding `lib.rs` or
///   `main.rs`. For anything under `src/` that directory is `src/` itself, so a
///   proptest in `src/agent/backend.rs` persists to its sibling tree,
///   `proptest-regressions/agent/backend.txt`.
/// - An integration test under `tests/` has no such ancestor — there is no
///   `tests/lib.rs`, and none above it — so the climb fails and proptest falls
///   back to `WithSource`, writing a file *beside the source*: a proptest in
///   `tests/render_props.rs` persists to
///   `tests/render_props.proptest-regressions`.
///
/// Proptest resolves that path at run time and **says nothing when it misses** —
/// a renamed or moved source file silently stops re-running its saved cases, so
/// the regression a seed was written for quietly stops being covered. Both
/// locations are swept here, since either kind of seed can be orphaned by a
/// rename.
///
/// This is the check that a wide rename needs and that nothing else provides.
#[test]
fn every_proptest_seed_still_names_a_live_source_file() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut orphans = Vec::new();

    // SourceParallel: the tree is rooted at `src/`, so the seed's relative path
    // names exactly one source file. `tests/` is deliberately not a candidate —
    // seeds for an integration test never land here, and accepting one would
    // let an unrelated `tests/foo.rs` mask a genuinely orphaned seed.
    let seeds_dir = root.join("proptest-regressions");
    if seeds_dir.exists() {
        for seed in collect_files_with_extension(&seeds_dir, "txt") {
            let rel = seed
                .strip_prefix(&seeds_dir)
                .expect("seed is under proptest-regressions/")
                .with_extension("rs");
            if !root.join("src").join(&rel).exists() {
                orphans.push(format!(
                    "  proptest-regressions/{} -> src/{} does not exist",
                    rel.with_extension("txt").display(),
                    rel.display()
                ));
            }
        }
    }

    // WithSource fallback: a seed beside its integration test.
    let tests_dir = root.join("tests");
    for seed in collect_files_with_extension(&tests_dir, "proptest-regressions") {
        let source = seed.with_extension("rs");
        if !source.exists() {
            let rel = |p: &Path| {
                p.strip_prefix(root)
                    .unwrap_or(p)
                    .display()
                    .to_string()
                    .replace('\\', "/")
            };
            orphans.push(format!(
                "  {} -> {} does not exist",
                rel(&seed),
                rel(&source)
            ));
        }
    }

    assert!(
        orphans.is_empty(),
        "orphaned proptest seeds — the source file moved or was renamed without \
         its seed, so proptest silently stopped replaying these cases:\n{}\n\
         Move the seed alongside the source file, or delete it if the property \
         is gone.",
        orphans.join("\n")
    );
}

/// Every file under `dir` (recursively) with the given extension.
fn collect_files_with_extension(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => panic!("cannot read {}: {e}", dir.display()),
    };
    for entry in entries {
        let path = entry.expect("readable directory entry").path();
        if path.is_dir() {
            out.extend(collect_files_with_extension(&path, ext));
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// Where each off-loop job of the kernel's two runtime stores may start a
/// thread: the directory, and the files under it allowed to name `thread::` in
/// production code. The store and the paint side never block, so neither
/// starts one of its own — each worker is a file, and a job that reaches the
/// world without one shows up here rather than as a frozen frame (#1474,
/// #1477). A text scan, because `std` is outside the crate graph the resolver
/// follows.
const KERNEL_THREADS: &[(&str, &[&str])] = &[
    (
        "kernel/snapshot",
        &["kernel/snapshot/fleet.rs", "kernel/snapshot/git.rs"],
    ),
    (
        "kernel/terminal",
        &[
            "kernel/terminal/attach.rs",
            "kernel/terminal/shells.rs",
            // A stream pane's reader: one per pane, as the backend's own.
            "kernel/terminal/stream_pane.rs",
        ],
    ),
];

/// What one worker alone may reach, resolved through imports and aliases: the
/// directory, the resolved path the reach starts with, and the one file allowed
/// to make it, so a slow `git` delays a stat and never a frame.
const KERNEL_REACHES: &[(&str, &str, &str)] =
    &[("kernel/snapshot", "git::", "kernel/snapshot/git.rs")];

/// `file` relative to `root`, with `/` on every platform.
fn relative(root: &Path, file: &Path) -> String {
    let file = file.strip_prefix(root).unwrap_or(file);
    let parts: Vec<_> = file.iter().map(|p| p.to_string_lossy()).collect();
    parts.join("/")
}

/// Every production site breaking [`KERNEL_THREADS`] or [`KERNEL_REACHES`] in
/// `tree`, as `dir/file.rs:line what`, and every production glob import under
/// those directories.
fn misplaced_kernel_work(tree: &Tree) -> Vec<String> {
    let mut found = Vec::new();
    for (file, line, needle) in tree.mentions(&["thread::"], "", false) {
        let file = relative(Path::new(""), &file);
        let allowed = KERNEL_THREADS
            .iter()
            .find(|(dir, _)| file.starts_with(&format!("{dir}/")))
            .map(|(_, allowed)| *allowed);
        if allowed.is_some_and(|allowed| !allowed.contains(&file.as_str())) {
            found.push(format!("{file}:{line} {needle}"));
        }
    }
    // A glob import binds nothing the resolver can follow, so a reach through
    // one would never reach the check below.
    for (file, line, _) in tree.mentions(&["*;", "*}"], "", false) {
        let file = relative(Path::new(""), &file);
        if KERNEL_THREADS
            .iter()
            .any(|(dir, _)| file.starts_with(&format!("{dir}/")))
        {
            found.push(format!("{file}:{line} glob import"));
        }
    }
    for reference in tree.references(&["kernel", "git"]) {
        if reference.test {
            continue;
        }
        let file = relative(&tree.root, &reference.file);
        let path = reference.path.join("::");
        for (dir, reach, allowed) in KERNEL_REACHES {
            if file.starts_with(&format!("{dir}/")) && path.starts_with(reach) && file != *allowed {
                found.push(format!("{file}:{} {path}", reference.line));
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// The snapshot store is split by worker — git stats, pane probes, the
/// Fleet watch — and `Terminals` by attach, shells and paint, so each worker's
/// reach into the world is in a file of its own and the read model and the
/// paint path start none.
#[test]
fn each_kernel_worker_lives_in_a_file_of_its_own() {
    let named = KERNEL_THREADS
        .iter()
        .flat_map(|(_, allowed)| allowed.iter())
        .chain(KERNEL_REACHES.iter().map(|(_, _, allowed)| allowed));
    for file in named {
        assert!(
            src_root().join(file).is_file(),
            "{file} is named in KERNEL_THREADS or KERNEL_REACHES but does not exist"
        );
    }
    let problems = misplaced_kernel_work(src_tree());
    assert!(
        problems.is_empty(),
        "move these into the worker that owns them, or extend KERNEL_THREADS or KERNEL_REACHES \
         with why:\n  {}",
        problems.join("\n  ")
    );
}

/// The scans behind [`each_kernel_worker_lives_in_a_file_of_its_own`] find a
/// worker's reach outside its file — through a `Builder`, an alias or a renamed
/// import too, and the glob import a reach could hide behind — and none of its lookalikes: a comment, a string, test code,
/// or the worker's own file.
#[test]
fn the_kernel_worker_scan_sees_only_misplaced_production_reaches() {
    assert_eq!(
        misplaced_kernel_work(&fixture("kernel_workers")),
        [
            "kernel/snapshot/mod.rs:10 thread::",
            "kernel/snapshot/mod.rs:11 git::stats",
            "kernel/snapshot/probe.rs:1 glob import",
            "kernel/terminal/mod.rs:4 thread::",
            "kernel/terminal/paint.rs:2 thread::",
        ]
    );
}
