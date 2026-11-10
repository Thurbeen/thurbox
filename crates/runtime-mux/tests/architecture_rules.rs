//! The runtime's module rules, enforced as tests: an allowlist over resolved
//! edges, with the resolver thurbox's `architecture_rules.rs` uses.
//!
//! Every module under `src/` has an entry naming the modules it may reference
//! in any form. On top of the allowlist, the shape the boundary depends on:
//! the adapters are peers, each is exactly one multiplexer, only the factory
//! names them, and the route and the contract know no launcher, adapter or
//! build OS (ADR-13, ADR-31).
//!
//! The layering mirrors the "Module Dependency Rules" in AGENTS.md and
//! docs/constitution.md. If a rule change is intentional, update those docs in
//! the same change.

#[allow(dead_code)]
#[path = "../../thurbox/tests/architecture/resolver.rs"]
mod resolver;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use resolver::{cycles, strip_comments_and_strings, Edge, Reference, Tree};

/// Per-node dependency allowlist.
struct ModuleRules {
    /// A top-level module, or a submodule of a [`SUBMODULE_GOVERNED`] one.
    name: &'static str,
    /// Nodes this node may reference in any form.
    allowed: &'static [&'static str],
}

/// Which nodes each node may touch.
const MODULE_RULES: &[ModuleRules] = &[
    // The host model a route names: pure data, the sink.
    ModuleRules {
        name: "host",
        allowed: &[],
    },
    // Pure values: the identity a window is stamped with, the hyperlink runs a
    // pane records, what the embedder injects, and the TOML parse.
    ModuleRules {
        name: "session_id",
        allowed: &[],
    },
    ModuleRules {
        name: "hyperlink",
        allowed: &[],
    },
    ModuleRules {
        name: "settings",
        allowed: &[],
    },
    ModuleRules {
        name: "toml_file",
        allowed: &[],
    },
    // Process lookups: `PATH` and the WSL distro this process runs in.
    ModuleRules {
        name: "path_lookup",
        allowed: &[],
    },
    // How a command reaches a machine: quoting, ssh and `wsl.exe` launchers.
    // `host` for the one conversion from a host entry to its launcher.
    ModuleRules {
        name: "shell",
        allowed: &["host"],
    },
    // `hosts.toml`: loading, WSL discovery and the loopback repair plan.
    ModuleRules {
        name: "hosts",
        allowed: &["host", "path_lookup", "shell", "toml_file"],
    },
    // A host's `PATH` and the multiplexers on it, probed once and cached.
    ModuleRules {
        name: "host_path",
        allowed: &["host", "shell"],
    },
    // Whether the binaries a session needs are installed.
    ModuleRules {
        name: "preflight",
        allowed: &["host", "path_lookup", "shell"],
    },
    // The contract every adapter implements and the values that cross it.
    // Names no adapter, no protocol helper and no host config: the bottom of
    // the boundary.
    ModuleRules {
        name: "contract",
        allowed: &[],
    },
    // Which window is whose (ADR-25), over the listing the contract defines.
    ModuleRules {
        name: "identity",
        allowed: &["contract"],
    },
    // Which server an instance's sessions live on (ADR-12).
    ModuleRules {
        name: "instance",
        allowed: &["host", "settings"],
    },
    // The pane machinery every backend's stream is wired into: the reader
    // loop, the vt100 parser, the signals it raises.
    ModuleRules {
        name: "pane",
        allowed: &[
            "contract",
            "hyperlink",
            "identity",
            "osc8",
            "output_wake",
            "session_id",
        ],
    },
    ModuleRules {
        name: "osc8",
        allowed: &["hyperlink"],
    },
    ModuleRules {
        name: "output_wake",
        allowed: &[],
    },
    // A container of backends. Knows the contract and nothing that builds one.
    ModuleRules {
        name: "registry",
        allowed: &["host", "contract"],
    },
    // The runtime protocol over one backend it is handed: the contract and
    // nothing that builds one.
    ModuleRules {
        name: "protocol",
        allowed: &["contract", "identity", "workspace"],
    },
    // The `workspace` verb's side: the runtime's config, the workspace-git
    // program it runs, and its setup and teardown steps.
    ModuleRules {
        name: "workspace",
        allowed: &[],
    },
    // One pane as a terminal stream, over one backend it is handed.
    ModuleRules {
        name: "view",
        allowed: &["contract"],
    },
    // What fills the registry: the only node that names an adapter.
    ModuleRules {
        name: "wiring",
        allowed: &[
            "host", "shell", "settings", "contract", "registry", "tmux", "psmux", "rmux",
        ],
    },
    // The tmux command and control-mode protocol the adapters speak. Shared
    // grammar, not an adapter: its root only declares the modules below.
    ModuleRules {
        name: "tmux_compat",
        allowed: &[],
    },
    ModuleRules {
        name: "tmux_compat::server",
        allowed: &[
            "host",
            "shell",
            "settings",
            "path_lookup",
            "host_path",
            "preflight",
            "contract",
            "identity",
            "instance",
            "session_id",
            "tmux_compat::control_mode",
            "tmux_compat::transport",
        ],
    },
    ModuleRules {
        name: "tmux_compat::control_mode",
        allowed: &["shell", "contract", "tmux_compat::transport"],
    },
    ModuleRules {
        name: "tmux_compat::transport",
        allowed: &["shell", "preflight"],
    },
    // The adapters are peers: each reaches the protocol helper and never the
    // other; nothing reaches one but the factory.
    ModuleRules {
        name: "tmux",
        allowed: &[
            "host",
            "shell",
            "settings",
            "contract",
            "tmux_compat::control_mode",
            "tmux_compat::server",
            "tmux_compat::transport",
        ],
    },
    ModuleRules {
        name: "psmux",
        allowed: &[
            "host",
            "shell",
            "settings",
            "contract",
            "instance",
            "tmux_compat::control_mode",
            "tmux_compat::server",
            "tmux_compat::transport",
        ],
    },
    ModuleRules {
        name: "rmux",
        allowed: &[
            "host",
            "shell",
            "settings",
            "contract",
            "tmux_compat::control_mode",
            "tmux_compat::server",
            "tmux_compat::transport",
        ],
    },
];

/// The crate root, which only declares and re-exports, and the `runtime-mux`
/// program's composition root, which calls the factory as an embedder does.
const EXEMPT: &[&str] = &["lib", "main"];

/// Nodes whose every file module must be a node of its own.
const SUBMODULE_GOVERNED: &[&str] = &["tmux_compat"];

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
    MODULE_RULES.iter().map(|r| r.name).collect()
}

fn rules_for(node: &str) -> &'static ModuleRules {
    MODULE_RULES
        .iter()
        .find(|r| r.name == node)
        .unwrap_or_else(|| panic!("no rule for node `{node}`"))
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
    MODULE_RULES
        .iter()
        .flat_map(|r| {
            r.allowed
                .iter()
                .map(|to| (r.name.to_string(), to.to_string()))
        })
        .collect()
}

fn production_graph(tree: &Tree) -> BTreeSet<(String, String)> {
    tree.edges(&node_names())
        .into_iter()
        .filter(|e| !e.test)
        .map(|e| (e.from, e.to))
        .collect()
}

/// Every edge breaks no rule — test code included: a test may not reach what
/// its module may not.
#[test]
fn every_module_rule_holds() {
    let tree = src_tree();
    let report: Vec<String> = tree
        .edges(&node_names())
        .iter()
        .filter(|e| !rules_for(&e.from).allowed.contains(&e.to.as_str()))
        .map(|e| describe(tree, e))
        .collect();
    assert!(
        report.is_empty(),
        "\narchitecture violation(s), as `from → resolved item @ file:line`:\n  {}\n\
         Fix the reference, or update MODULE_RULES plus AGENTS.md and \
         docs/constitution.md if the architecture is changing on purpose.\n",
        report.join("\n  ")
    );
}

/// Every allowance is used by production code: an unused grant is a door
/// nobody decided to open.
#[test]
fn every_allowance_is_used() {
    let used = production_graph(src_tree());
    let unused: Vec<String> = declared_edges()
        .into_iter()
        .filter(|edge| !used.contains(edge))
        .map(|(from, to)| format!("{from} → {to}"))
        .collect();
    assert!(
        unused.is_empty(),
        "\nallowances no production code uses — delete them:\n  {}",
        unused.join("\n  ")
    );
}

/// Both the production graph and the declared one are acyclic.
#[test]
fn the_graphs_are_acyclic() {
    let production = cycles(&production_graph(src_tree()));
    assert!(production.is_empty(), "production cycle(s): {production:?}");
    let declared = cycles(&declared_edges());
    assert!(
        declared.is_empty(),
        "MODULE_RULES allow a cycle: {declared:?}"
    );
}

/// Every module under `src/` is a node or exempt, every file under a
/// submodule-governed node is a node of its own, and no rule names a module
/// that does not exist.
#[test]
fn every_module_is_governed() {
    let tree = src_tree();
    for entry in fs::read_dir(src_root()).expect("read src/") {
        let path = entry.expect("readable entry").path();
        let name = if path.is_dir() {
            path.file_name()
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            path.file_stem()
        } else {
            continue;
        };
        let name = name
            .and_then(|n| n.to_str())
            .expect("UTF-8 path")
            .to_string();
        assert!(
            MODULE_RULES.iter().any(|r| r.name == name) || EXEMPT.contains(&name.as_str()),
            "src/{name} has no architecture rule — add a MODULE_RULES entry"
        );
    }
    for parent in SUBMODULE_GOVERNED {
        for child in tree.file_descendants(parent) {
            assert!(
                MODULE_RULES.iter().any(|r| r.name == child),
                "`{child}` has no architecture rule"
            );
        }
    }
    for rules in MODULE_RULES {
        assert!(
            tree.has_module(rules.name),
            "`{}` matches nothing",
            rules.name
        );
        for target in rules.allowed {
            assert!(
                tree.has_module(target),
                "`{}` allows `{target}`",
                rules.name
            );
        }
    }
}

/// Only the factory may reach an adapter, and nothing may reach the factory:
/// an embedder's composition root calls it, and every other module is handed
/// the registry it builds.
#[test]
fn only_the_factory_names_an_adapter() {
    for rules in MODULE_RULES {
        assert!(
            !rules.allowed.contains(&FACTORY),
            "`{}` may reference {FACTORY}; only an embedder's composition root may",
            rules.name
        );
        if rules.name != FACTORY {
            for adapter in ADAPTERS {
                assert!(
                    !rules.allowed.contains(adapter),
                    "`{}` may reference the adapter {adapter}; only {FACTORY} may",
                    rules.name
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
            found.push(format!("MODULE_RULES lets {from} reach {to}"));
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
/// multiplexer (ADR-13): they reach no launcher and no adapter, and decide
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
