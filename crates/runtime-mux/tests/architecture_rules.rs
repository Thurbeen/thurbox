//! The shape the runtime's boundary depends on, over the module rules in
//! `crates/architecture.toml` (which `arch-rules` checks): the adapters are
//! peers, each is exactly one multiplexer, only the factory names them, and
//! the route and the contract know no launcher, adapter or build OS
//! (docs/runtime/runtime-mux.md, ADR-31).

#[allow(dead_code)]
#[path = "../../thurbox/tests/architecture/resolver.rs"]
mod resolver;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use resolver::{strip_comments_and_strings, Edge, Reference, Tree};

/// The runtime's module rules, as `crates/architecture.toml` declares them:
/// each module and the modules it may use.
fn module_rules() -> &'static BTreeMap<String, Vec<String>> {
    static RULES: OnceLock<BTreeMap<String, Vec<String>>> = OnceLock::new();
    RULES.get_or_init(|| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../architecture.toml");
        let file: toml::Table = fs::read_to_string(&path)
            .expect("read crates/architecture.toml")
            .parse()
            .expect("parse crates/architecture.toml");
        let modules = file["crates"]["runtime-mux"]["modules"]
            .as_table()
            .expect("runtime-mux declares its modules");
        modules
            .iter()
            .map(|(name, rule)| {
                let uses = rule["use"]
                    .as_array()
                    .expect("a module rule lists what it uses")
                    .iter()
                    .map(|u| u.as_str().expect("a module name").to_string())
                    .collect();
                (name.clone(), uses)
            })
            .collect()
    })
}

/// The node that builds the registry, naming every concrete adapter.
const FACTORY: &str = "wiring";

/// The concrete adapters, each serving one multiplexer of its own.
const ADAPTERS: &[&str] = &["tmux", "psmux", "rmux"];

/// The protocol helper the adapters share, which uses none of them.
const PROTOCOL_HELPER: &str = "tmux_compat";

fn in_protocol_helper(node: &str) -> bool {
    node == PROTOCOL_HELPER || node.starts_with(&format!("{PROTOCOL_HELPER}::"))
}

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn src_tree() -> &'static Tree {
    static TREE: OnceLock<Tree> = OnceLock::new();
    TREE.get_or_init(|| Tree::load(&src_root()))
}

fn node_names() -> Vec<&'static str> {
    module_rules().keys().map(String::as_str).collect()
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

fn declared_edges() -> BTreeSet<(String, String)> {
    module_rules()
        .iter()
        .flat_map(|(from, uses)| uses.iter().map(|to| (from.clone(), to.clone())))
        .collect()
}

/// Only the factory may reach an adapter, and nothing may reach the factory:
/// an embedder's composition root calls it, and every other module is handed
/// the registry it builds.
#[test]
fn only_the_factory_names_an_adapter() {
    for (name, uses) in module_rules() {
        assert!(
            !uses.iter().any(|u| u == FACTORY),
            "`{name}` may reference {FACTORY}; only an embedder's composition root may"
        );
        if name != FACTORY {
            for adapter in ADAPTERS {
                assert!(
                    !uses.iter().any(|u| u == adapter),
                    "`{name}` may reference the adapter {adapter}; only {FACTORY} may"
                );
            }
        }
    }
}

/// The adapters are peers: neither reaches the other, and the protocol helper
/// they share reaches neither — in code, test code included, or in a grant. A
/// quirk of one multiplexer is then a body in its own adapter, never a branch
/// in code the other runs.
#[test]
fn the_adapters_are_peers() {
    let tree = src_tree();
    let mut found = Vec::new();
    for adapter in ADAPTERS {
        if !tree.has_module(adapter) {
            found.push(format!("{adapter} does not exist"));
        }
    }
    let crosses = |from: &str, to: &str| {
        ADAPTERS.contains(&to)
            && from != to
            && (ADAPTERS.contains(&from) || in_protocol_helper(from))
    };
    for edge in tree.edges(&node_names()) {
        if crosses(&edge.from, &edge.to) {
            found.push(describe(tree, &edge));
        }
    }
    for (from, to) in declared_edges() {
        if crosses(&from, &to) {
            found.push(format!("the module rules let {from} reach {to}"));
        }
    }
    assert!(
        found.is_empty(),
        "\nthe adapters are not peers:\n  {}\n",
        found.join("\n  ")
    );
}

/// Where `Multiplexer` lives, as a resolved path.
const MULTIPLEXER: &[&str] = &["host", "multiplexer", "Multiplexer"];

/// The multiplexers a route can name.
const MULTIPLEXER_VARIANTS: &[&str] = &["Tmux", "Psmux", "Rmux", "Herdr"];

/// The multiplexer variant a resolved path names, if it names one.
fn multiplexer_variant(reference: &Reference) -> Option<&str> {
    let path = &reference.path;
    (path.len() == MULTIPLEXER.len() + 1
        && path[..MULTIPLEXER.len()].iter().eq(MULTIPLEXER.iter())
        && MULTIPLEXER_VARIANTS.contains(&path[MULTIPLEXER.len()].as_str()))
    .then(|| path[MULTIPLEXER.len()].as_str())
}

/// The nodes that may decide something by naming *one* multiplexer: the
/// route grammar and its defaults (`host`), whether a binary is installed
/// (`preflight`, which names the local default), the factory and the adapters.
fn may_name_a_multiplexer(node: &str) -> bool {
    node == "host" || node == "preflight" || node == FACTORY || ADAPTERS.contains(&node)
}

/// A specific multiplexer is named only where one is chosen, and each adapter
/// names exactly the one it is, no two the same, with every multiplexer the
/// factory serves one an adapter is. Test code is left out: a test names a
/// multiplexer to pin what happens for it.
#[test]
fn each_multiplexer_is_chosen_only_by_its_adapter_and_the_factory() {
    let tree = src_tree();
    let mut found = Vec::new();
    let mut owned: BTreeMap<&str, BTreeSet<String>> =
        ADAPTERS.iter().map(|a| (*a, BTreeSet::new())).collect();
    let mut served = BTreeSet::new();
    for reference in tree.references(&node_names()) {
        if reference.test {
            continue;
        }
        let Some(variant) = multiplexer_variant(&reference) else {
            continue;
        };
        if !may_name_a_multiplexer(&reference.from) {
            found.push(format!(
                "{} names Multiplexer::{variant} @ {}:{}",
                reference.from,
                reference.file.display(),
                reference.line
            ));
        }
        if let Some(names) = owned.get_mut(reference.from.as_str()) {
            names.insert(variant.to_string());
        }
        if reference.from == FACTORY {
            served.insert(variant.to_string());
        }
    }
    let mut owner: BTreeMap<String, &str> = BTreeMap::new();
    for (adapter, names) in &owned {
        if names.len() != 1 {
            found.push(format!(
                "{adapter} names {names:?}; an adapter is exactly one"
            ));
        }
        for name in names {
            if let Some(other) = owner.insert(name.clone(), adapter) {
                found.push(format!(
                    "{other} and {adapter} both are Multiplexer::{name}"
                ));
            }
        }
    }
    for name in &served {
        if !owner.contains_key(name) {
            found.push(format!(
                "{FACTORY} serves Multiplexer::{name}, which no adapter is"
            ));
        }
    }
    assert!(found.is_empty(), "\n  {}\n", found.join("\n  "));
}

/// The files that say where a session runs and what a backend is, for every
/// host and every multiplexer alike.
const NEUTRAL_FILES: &[&str] = &["host/route.rs", "contract.rs"];

/// What a neutral file may not reach: how one kind of host is launched or
/// configured, or how one multiplexer is driven.
const HOST_OR_MUX_SPECIFIC: &[&str] = &[
    "shell",
    "host::host_def",
    "hosts",
    "tmux",
    "psmux",
    "rmux",
    "tmux_compat",
];

/// The route and the contract are the same for every host OS, launcher and
/// multiplexer (docs/runtime/runtime-mux.md): they reach no launcher and no adapter, and decide
/// nothing by the OS this build was compiled for.
#[test]
fn the_route_and_the_contract_know_no_launcher_adapter_or_build_os() {
    let tree = src_tree();
    let root = src_root();
    let mut found = Vec::new();
    for reference in tree.references(&node_names()) {
        let Some(file) = NEUTRAL_FILES
            .iter()
            .find(|f| reference.file.ends_with(Path::new(f)))
        else {
            continue;
        };
        if reference.test {
            continue;
        }
        let path = reference.path.join("::");
        if let Some(banned) = HOST_OR_MUX_SPECIFIC
            .iter()
            .find(|b| path == **b || path.starts_with(&format!("{b}::")))
        {
            found.push(format!(
                "{file}:{} reaches {path} ({banned})",
                reference.line
            ));
        }
    }
    for file in NEUTRAL_FILES {
        let source = fs::read_to_string(root.join(file)).expect("read a neutral file");
        let stripped = strip_comments_and_strings(&source);
        let production = stripped.split("#[cfg(test)]").next().unwrap_or_default();
        for (n, line) in production.lines().enumerate() {
            if line.contains("cfg(windows)")
                || line.contains("cfg!(windows)")
                || line.contains("cfg(not(windows))")
                || line.contains("cfg(unix)")
            {
                found.push(format!("{file}:{} decides by build OS", n + 1));
            }
        }
    }
    assert!(
        found.is_empty(),
        "a neutral file depends on one host or multiplexer:\n  {}",
        found.join("\n  ")
    );
}

/// Every persisted proptest seed still names a source file: a moved source
/// silently stops replaying its saved cases.
#[test]
fn every_proptest_seed_still_names_a_live_source_file() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let seeds = root.join("proptest-regressions");
    let mut orphans = Vec::new();
    let mut stack = vec![seeds.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("readable entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "txt") {
                let rel = path.strip_prefix(&seeds).unwrap().with_extension("rs");
                if !root.join("src").join(&rel).exists() {
                    orphans.push(rel.display().to_string());
                }
            }
        }
    }
    assert!(
        orphans.is_empty(),
        "orphaned proptest seeds for src/{orphans:?}"
    );
}
