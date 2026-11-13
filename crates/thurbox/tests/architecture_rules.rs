//! thurbox's own architecture checks, over the module rules in
//! `crates/architecture.toml`. `arch-rules` holds every module to those rules:
//! each has one, every reference keeps to it, no cycle, no unused grant. What
//! stays here is particular to thurbox: no consumer reaches a concrete
//! backend, the runtime is named only through its gateways, one multiplexer
//! is named only where one is chosen, `session` stays pure data, files thurbox
//! reads back are written atomically, and each kernel worker lives in a file
//! of its own.

#[allow(dead_code)]
#[path = "architecture/resolver.rs"]
mod resolver;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use resolver::{strip_comments_and_strings, Edge, Reference, Tree};

/// thurbox's module rules as `crates/architecture.toml` declares them.
struct Rules {
    /// Each module and every module it is granted, in any form or by path
    /// only.
    grants: BTreeMap<String, Vec<String>>,
    /// The modules whose every file module has a rule of its own.
    governed: Vec<String>,
}

fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../architecture.toml");
        let file: toml::Table = fs::read_to_string(&path)
            .expect("read crates/architecture.toml")
            .parse()
            .expect("parse crates/architecture.toml");
        let thurbox = &file["crates"]["thurbox"];
        let names = |v: &toml::Value| -> Vec<String> {
            v.as_array()
                .expect("a list of module names")
                .iter()
                .map(|n| n.as_str().expect("a module name").to_string())
                .collect()
        };
        let grants = thurbox["modules"]
            .as_table()
            .expect("thurbox declares its modules")
            .iter()
            .map(|(name, rule)| {
                let mut all = names(&rule["use"]);
                if let Some(path_only) = rule.get("path_only") {
                    all.extend(names(path_only));
                }
                (name.clone(), all)
            })
            .collect();
        Rules {
            grants,
            governed: names(&thurbox["governed"]),
        }
    })
}

fn node_names() -> Vec<&'static str> {
    rules().grants.keys().map(String::as_str).collect()
}

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// The real tree, parsed once for every test in this file.
fn src_tree() -> &'static Tree {
    static TREE: OnceLock<Tree> = OnceLock::new();
    TREE.get_or_init(|| Tree::load(&src_root()))
}

fn describe(tree: &Tree, edge: &Edge) -> String {
    format!(
        "{} → {} @ {}{}",
        edge.from,
        edge.target(),
        edge.site(&tree.root),
        if edge.test { " [test]" } else { "" },
    )
}

/// Where `Multiplexer` is reached, as a resolved path: its variants are the
/// one kind of item a node may reach *through* a grant and still not name.
/// `session` re-exports it from the runtime, so a path stops at
/// `session::Multiplexer`; a tree that defines it in `session/multiplexer.rs`
/// resolves one segment deeper.
const MULTIPLEXER: &[&[&str]] = &[
    &["session", "Multiplexer"],
    &["session", "multiplexer", "Multiplexer"],
];

/// The multiplexers a route can name. A name here is not an implementation:
/// which of them work is what the registry says, at runtime.
const MULTIPLEXER_VARIANTS: &[&str] = &["Tmux", "Psmux", "Rmux", "Herdr"];

/// The one node that may decide something by naming *one* multiplexer: the
/// route grammar and its defaults (`session`). The runtime's factory and
/// adapters are the only other places one is chosen, and `runtime-mux`'s own
/// architecture test holds them. Anywhere else, a `Multiplexer::Psmux` is a
/// consumer choosing behaviour — or an OS — by multiplexer, which is what the
/// route and the registry exist to decide.
fn may_name_a_multiplexer(node: &str) -> bool {
    node == "session"
}

/// The multiplexer variant a resolved path names, if it names one.
fn multiplexer_variant(reference: &Reference) -> Option<&str> {
    let path = &reference.path;
    MULTIPLEXER.iter().find_map(|at| {
        (path.len() == at.len() + 1
            && path[..at.len()].iter().eq(at.iter())
            && MULTIPLEXER_VARIANTS.contains(&path[at.len()].as_str()))
        .then(|| path[at.len()].as_str())
    })
}

/// Every reference to a specific multiplexer from production code in a node
/// that may not name one, as an edge to `session` whose item is the variant
/// (`Multiplexer::Psmux`).
///
/// Test code is left out: a test names a multiplexer to pin what happens for
/// it, which is the opposite of deciding behaviour by one.
fn variant_violations(tree: &Tree, nodes: &[&str]) -> Vec<Edge> {
    tree.references(nodes)
        .into_iter()
        .filter(|r| !r.test && !may_name_a_multiplexer(&r.from))
        .filter_map(|r| {
            let variant = multiplexer_variant(&r)?;
            Some(Edge {
                item: format!("Multiplexer::{variant}"),
                from: r.from,
                to: "session".to_string(),
                file: r.file,
                line: r.line,
                in_use: r.in_use,
                test: r.test,
            })
        })
        .collect()
}

/// A specific multiplexer is named only in `session`, where a route is
/// decided: production code anywhere else naming one is choosing behaviour by
/// multiplexer.
#[test]
fn a_multiplexer_variant_is_named_only_in_session() {
    let tree = src_tree();
    let found: Vec<String> = variant_violations(tree, &node_names())
        .iter()
        .map(|e| describe(tree, e))
        .collect();
    assert!(
        found.is_empty(),
        "\nname no multiplexer outside `session`:\n  {}\n",
        found.join("\n  ")
    );
}

/// What of the runtime a consumer must never reach by name: one multiplexer's
/// adapter, the protocol grammar only some multiplexers speak, or the factory
/// that names them — items of the `backend` gateway. The adapters' own rules
/// (peers, one multiplexer each, the factory the only one to name them) are
/// `runtime-mux`'s architecture test.
const CONCRETE_BACKEND: &[&str] = &["tmux", "psmux", "rmux", "tmux_compat", "wiring"];

/// The one node that may build the registry: `main`'s body. The exempt crate
/// roots (`main`, `bin/`) may too. A consumer that builds its own registry
/// sees a different set of backends from the one the process was wired with.
const COMPOSITION_ROOT: &str = "coordinator";

/// Each reach of a concrete adapter, the protocol helper or the factory
/// through the `backend` gateway in `tree`, except the composition root's
/// reach of the factory. References are resolved through `use`, `super::`,
/// brace groups, re-exports and `type` aliases, test code included.
fn concrete_backend_reaches(tree: &Tree, nodes: &[&str]) -> Vec<String> {
    tree.edges(nodes)
        .iter()
        .filter(|e| e.to == "backend" && CONCRETE_BACKEND.contains(&e.item.as_str()))
        .filter(|e| !(e.from == COMPOSITION_ROOT && e.item == "wiring"))
        .map(|e| describe(tree, e))
        .collect()
}

/// Every verb a consumer needs — lifecycle, pane I/O, status delivery and
/// polling, the heartbeat — goes through the contract, so no module reaches a
/// concrete backend at all, and only the composition root reaches the factory.
#[test]
fn consumers_reach_no_concrete_backend() {
    let found = concrete_backend_reaches(src_tree(), &node_names());
    assert!(
        found.is_empty(),
        "\na module reaches a concrete backend:\n  {}\n",
        found.join("\n  ")
    );
}

/// Each grant in `modules` of a whole parent in `governed`.
fn whole_parent_grants(
    modules: &BTreeMap<String, Vec<String>>,
    governed: &[String],
) -> Vec<String> {
    modules
        .iter()
        .flat_map(|(name, grants)| {
            grants
                .iter()
                .filter(|to| governed.contains(to))
                .map(move |to| format!("{name} is granted all of `{to}`"))
        })
        .collect()
}

/// No rule grants a whole `governed` parent, so a module added under `agent`
/// is not reachable until someone decides it is.
#[test]
fn no_grant_covers_a_governed_parent() {
    let rules = rules();
    assert!(
        rules.governed.iter().any(|g| g == "agent"),
        "`agent` must stay governed, so a grant names what it reaches"
    );
    let found = whole_parent_grants(&rules.grants, &rules.governed);
    assert!(
        found.is_empty(),
        "\nname the submodules each uses:\n  {}\n",
        found.join("\n  ")
    );
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

/// A specific multiplexer is named only where a multiplexer is decided. The
/// variant is caught however it is reached — by path, through the parent's
/// re-export, through an imported name, as a `use` leaf — while the type
/// itself and its associated items (`Multiplexer::ALL`) stay free to use, and
/// a test may name the one it pins.
#[test]
fn a_multiplexer_variant_is_named_only_where_one_is_chosen() {
    let tree = fixture("mux_variants");
    let found: BTreeSet<String> = variant_violations(&tree, &["session", "kernel"])
        .iter()
        .map(|e| format!("{} → {} @ {}", e.from, e.target(), e.site(&tree.root)))
        .collect();
    let expected: BTreeSet<String> = [
        "kernel → session::Multiplexer::Psmux @ kernel/mod.rs:3",
        "kernel → session::Multiplexer::Rmux @ kernel/mod.rs:5",
        "kernel → session::Multiplexer::Herdr @ kernel/mod.rs:8",
        "kernel → session::Multiplexer::Tmux @ kernel/mod.rs:10",
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

/// A consumer reaching an adapter or the factory through the gateway is found,
/// and so is a reach through an imported name; the contract and the composition
/// root's reach of the factory are not.
#[test]
fn a_reach_of_a_concrete_backend_is_found() {
    assert_eq!(
        concrete_backend_reaches(
            &fixture("runtime_imports"),
            &["backend", "coordinator", "kernel", "session"]
        ),
        [
            "kernel → backend::psmux @ kernel/mod.rs:16",
            "kernel → backend::tmux @ kernel/mod.rs:11",
            "kernel → backend::wiring @ kernel/mod.rs:12",
            "kernel → backend::psmux @ kernel/mod.rs:17",
        ]
    );
}

/// A grant of a governed parent is found, whether in any form or by path only;
/// a grant of one of its submodules is not.
#[test]
fn a_grant_of_a_governed_parent_is_found() {
    let grants = BTreeMap::from([
        (
            "cli".to_string(),
            vec!["agent".to_string(), "agent::fetch".to_string()],
        ),
        ("kernel".to_string(), vec!["config".to_string()]),
    ]);
    assert_eq!(
        whole_parent_grants(&grants, &["agent".to_string(), "config".to_string()]),
        [
            "cli is granted all of `agent`",
            "kernel is granted all of `config`"
        ]
    );
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
/// The calls that create or truncate a file in place.
const IN_PLACE_WRITES: &[&str] = &["fs::write", "fs::copy", "File::create", "OpenOptions::new"];

/// Production calls in one file allowed to write in place, how many, and why.
/// Everything else a user edits or thurbox reads back goes through
/// `paths::write_atomically`, which a crash, a full disk or a second writer
/// cannot leave half-written. The count is exact, so a further write in an
/// allowed file needs its own reason.
struct InPlaceWrite {
    file: &'static str,
    call: &'static str,
    sites: usize,
    why: &'static str,
}

const IN_PLACE_ALLOWED: &[InPlaceWrite] = &[
    InPlaceWrite {
        file: "kernel/snapshot/fleet.rs",
        call: "OpenOptions::new",
        sites: 1,
        why: "fleet up's standard error, created exclusively, unlinked once spawned and read once \
              it exits",
    },
    InPlaceWrite {
        file: "update/self_update.rs",
        call: "fs::copy",
        sites: 1,
        why: "copies the new binary onto a staged .new file that is renamed into place",
    },
    InPlaceWrite {
        file: "update/version_check.rs",
        call: "fs::write",
        sites: 1,
        why: "a cache: one that fails to parse is fetched again",
    },
    InPlaceWrite {
        file: "clipboard.rs",
        call: "OpenOptions::new",
        sites: 1,
        why: "opens /dev/tty to send an OSC 52 sequence, not a file",
    },
    InPlaceWrite {
        file: "ui_control.rs",
        call: "OpenOptions::new",
        sites: 2,
        why: "appends to the control audit log, on Unix and on Windows",
    },
    InPlaceWrite {
        file: "ui_control.rs",
        call: "File::create",
        sites: 1,
        why: "creates the empty audit log on Windows so its ACL is set before the first append",
    },
    InPlaceWrite {
        file: "ui_control.rs",
        call: "fs::write",
        sites: 2,
        why: "stages the instance file under a private mode (Unix) or owner ACL (Windows), then renames it",
    },
];

/// What [`files_thurbox_reads_back_are_written_atomically`] reports: every site
/// of a call no entry allows or that outnumbers its entry, and every entry that
/// outnumbers what is left of it. Files are compared with `/` separators, as
/// the entries spell them, so the rule holds on Windows too.
fn unlisted_in_place_writes(
    found: &[(PathBuf, usize, &str)],
    allowed: &[InPlaceWrite],
) -> Vec<String> {
    let mut sites: BTreeMap<(String, &str), Vec<usize>> = BTreeMap::new();
    for (file, line, call) in found {
        sites
            .entry((file.to_string_lossy().replace('\\', "/"), *call))
            .or_default()
            .push(*line);
    }
    let mut problems = Vec::new();
    for ((file, call), lines) in &sites {
        let listed = allowed
            .iter()
            .find(|a| a.file == file && a.call == *call)
            .map_or(0, |a| a.sites);
        if lines.len() > listed {
            for line in lines {
                problems.push(format!(
                    "{file}:{line} {call} ({} found, {listed} allowed)",
                    lines.len()
                ));
            }
        }
    }
    for a in allowed {
        let left = sites.get(&(a.file.to_string(), a.call)).map_or(0, Vec::len);
        if left < a.sites {
            problems.push(format!(
                "{} {} is allowed {} but {left} remain ({})",
                a.file, a.call, a.sites, a.why
            ));
        }
    }
    problems
}

/// Production code outside `paths` writes no file in place unless
/// [`IN_PLACE_ALLOWED`] says why (#1634): a truncated `agents.toml`, manifest
/// or plugin lock is a broken install, while a cut log line or a stale cache
/// is not. Every entry must still match, so a write that moves to the atomic
/// writer takes its exception with it.
#[test]
fn files_thurbox_reads_back_are_written_atomically() {
    let found: Vec<_> = src_tree()
        .production_calls(IN_PLACE_WRITES)
        .into_iter()
        .filter(|(file, _, _)| !file.starts_with("paths"))
        .collect();
    let problems = unlisted_in_place_writes(&found, IN_PLACE_ALLOWED);
    assert!(
        problems.is_empty(),
        "write these through paths::write_atomically, or correct IN_PLACE_ALLOWED with why:\n  {}",
        problems.join("\n  ")
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

/// The scan behind [`files_thurbox_reads_back_are_written_atomically`] sees the
/// production writes and none of their lookalikes: a comment, a string,
/// another module's `write`, a `#[cfg(test)]` module inline or in a file of its
/// own. An allowance covers exactly its count: one write past it fails, and one
/// short of it is stale.
#[test]
fn the_in_place_write_scan_sees_only_production_calls() {
    let found = fixture("in_place_writes").production_calls(IN_PLACE_WRITES);
    let sites: Vec<_> = found
        .iter()
        .map(|(file, line, call)| format!("{}:{line} {call}", file.display()))
        .collect();
    assert_eq!(sites, ["config.rs:5 fs::write", "config.rs:9 fs::write"]);

    let allow = |sites| {
        [InPlaceWrite {
            file: "config.rs",
            call: "fs::write",
            sites,
            why: "fixture",
        }]
    };
    assert_eq!(
        unlisted_in_place_writes(&found, &allow(2)),
        Vec::<String>::new()
    );
    assert_eq!(
        unlisted_in_place_writes(&found, &allow(1)),
        [
            "config.rs:5 fs::write (2 found, 1 allowed)",
            "config.rs:9 fs::write (2 found, 1 allowed)"
        ]
    );
    assert_eq!(
        unlisted_in_place_writes(&found, &allow(3)),
        ["config.rs fs::write is allowed 3 but 2 remain (fixture)"]
    );
}

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
