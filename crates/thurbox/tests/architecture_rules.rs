//! Architecture rules enforced as tests (allowlist model over resolved edges).
//!
//! Every module under `src/` must appear in [`MODULE_RULES`] (or in
//! [`EXEMPT`]) and may only reference the nodes its entry allows —
//! `every_module_is_governed` fails when a new module is added without a rule,
//! so the architecture is an explicit decision per module.
//!
//! A rule's name is a **node**: a top-level module (`kernel`) or a governed
//! submodule (`agent::fetch`). A file belongs to the deepest node containing it,
//! and a reference is judged by the node it *resolves to* (see `resolver`):
//! `super::`, `self::`, bare child-module paths, nested brace groups, `as`
//! renames, imported names, `pub use` re-exports and `type` aliases are all
//! followed, so no import shape and no alias carries a crossing past a rule. A
//! grant names exactly one node and never its children: allowing `agent`
//! admits nothing in a governed `agent::fetch`.
//!
//! The graph is checked as a whole too: the actual production edges and the
//! declared allowlist must both be acyclic, and every allowance must be used
//! by production code. A crossing that is known and scheduled for removal is
//! listed in [`TRANSITIONAL`], which must equal the violations found — both
//! ways, so a new crossing fails and so does a stale entry.
//!
//! The layering mirrors AGENTS.md ("Module Dependency Rules") and
//! docs/constitution.md T1. If a rule change is intentional, update those
//! docs in the same PR.

#[path = "architecture/resolver.rs"]
mod resolver;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use resolver::{cycles, strip_comments_and_strings, Edge, Reference, Tree};

/// Per-node dependency allowlist.
struct ModuleRules {
    /// A top-level module (`src/<name>/` or `src/<name>.rs`) or a governed
    /// submodule path (`agent::fetch`).
    name: &'static str,
    /// Nodes this node may reference in any form.
    allowed: &'static [&'static str],
    /// Nodes additionally reachable via fully-qualified paths
    /// (`crate::module::item(…)`) but **not** importable with `use` —
    /// keeps the dependency visible at every call site.
    allowed_path_only: &'static [&'static str],
}

/// Which nodes each node may touch.
const MODULE_RULES: &[ModuleRules] = &[
    // Pure data: the dependency sink. No crate-internal references at all.
    ModuleRules {
        name: "session",
        allowed: &[],
        allowed_path_only: &[],
    },
    // Coding-agent definitions: the agents.toml registry, the downloads plugins
    // and the updater make, and keyboard input. Never a session backend: the
    // multiplexer runtime (`backend`) is reached through its own node.
    //
    // Every file is a node of its own (`SUBMODULE_GOVERNED`), so a grant names
    // what it reads and a file added here is reachable by nobody until that is
    // decided. The root only declares them.
    ModuleRules {
        name: "agent",
        allowed: &[],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "agent::fetch",
        allowed: &["paths"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "agent::input",
        allowed: &[],
        allowed_path_only: &[],
    },
    // thurbox's own config files beside config.toml. `config::file` is the one
    // loader: where a file lives, seeding it, parsing it with unknown keys
    // reported, and writing it back atomically. Each file is a node of its own
    // so a consumer names the config it reads; none reaches another, so a
    // file's degradation is its own.
    ModuleRules {
        name: "config",
        allowed: &[],
        allowed_path_only: &[],
    },
    // automations.json beside settings.toml: the schedule thurbox keeps
    // while Fleet has no schedule kind. The schedule math is `session`'s.
    ModuleRules {
        name: "config::automations",
        allowed: &["session", "config::file"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "config::file",
        allowed: &["paths"],
        allowed_path_only: &[],
    },
    // hosts.toml in thurbox's config directory: the runtime's loader bound to
    // `config::file`. Its own node so that reading global host config is a
    // visible decision.
    // What the interface remembers between runs (`interface.json`): data in,
    // the editor mode's type from `session`.
    ModuleRules {
        name: "config::interface_state",
        allowed: &["session", "config::file"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "config::hosts",
        allowed: &["session", "config::file"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "config::settings",
        allowed: &["session", "config::file"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "config::themes",
        allowed: &["theme", "config::file"],
        allowed_path_only: &[],
    },
    // Self-update: whether a newer release exists, and installing it.
    ModuleRules {
        name: "update",
        allowed: &[],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "update::self_update",
        allowed: &["paths", "shell", "agent::fetch", "update::version_check"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "update::version_check",
        allowed: &["paths", "agent::fetch"],
        allowed_path_only: &[],
    },
    // The gateway to the multiplexer runtime (`runtime_mux`, ADR-41): its
    // re-exports, and what thurbox injects into it — `session` for the settings
    // type, `paths` for the data directory, `config::hosts` for the hosts the
    // factory registers. Every crossing into the runtime is an edge to this
    // node, judged by the item it names (see `CONCRETE_BACKEND`).
    ModuleRules {
        name: "backend",
        allowed: &["session", "paths", "config::hosts"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "git",
        allowed: &["session", "paths", "shell"],
        allowed_path_only: &[],
    },
    // The interface's diagnostic-log bridge (ADR-38), built by
    // `coordinator`, which hands it the identity it exports under. The
    // settings come from `session`.
    // The theme presets and the user's overrides, in `ratatui` colours: data,
    // but rendering data, which `session_is_pure_data` keeps out of the sink.
    ModuleRules {
        name: "theme",
        allowed: &[],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "telemetry",
        allowed: &["session"],
        allowed_path_only: &[],
    },
    // Thin headless dispatch — must not depend on TUI or the live backend.
    ModuleRules {
        name: "cli",
        allowed: &["session", "paths", "notifications", "ui_control", "theme"],
        // `kernel` for the two subcommands that drive the *interface's* own
        // files — `plugin` (`check` loads the real host: the failures worth
        // reporting are declaration-shaped — no `render`, an unplaced slot, a
        // clashing key — and a syntax check passes all of them) and `config`
        // (the interface directory and its `ui.json` overrides). Both are
        // kernel-owned surfaces asked about from outside. Path-only, so the crossing stays visible at each call
        // site.
        allowed_path_only: &[
            "kernel",
            "backend",
            "config::file",
            "config::hosts",
            "config::automations",
            "config::interface_state",
            "config::settings",
            "config::themes",
            "update::self_update",
            "update::version_check",
        ],
    },
    // The plugin kernel: hosts the Lua VM the whole UI is written in. Builds
    // the snapshot plugins render from Fleet's rows (`session` for the types,
    // `paths` for the interface's files and plugin directories).
    //
    // `git` IS allowed, and the rule is about *where*: the worker-backed stores
    // (`diff`, `repos`, `packages`, `command`) shell out to it off-thread, which
    // is rule 5 rather than an exception to it. What must not happen is a `git`
    // call from a render path — that is enforced by the loop's shape (a plugin
    // returns a tree; it cannot call Rust), not by this allowlist.
    //
    // `shell`: `runs` spells a `cd <dir> && <program>` script for a host, and
    // POSIX quoting has exactly one home (`shell::posix_quote`).
    ModuleRules {
        name: "kernel",
        allowed: &["session", "paths", "git", "notifications", "shell", "theme"],
        // Live agent terminals: `kernel::terminal` adopts a session's real pane
        // through the backend contract and paints its vt100 screen. All are
        // reachable by fully-qualified path only (never `use`), the same rule
        // `cli` follows, so every crossing into the
        // side-effect layer is visible at its call site.
        allowed_path_only: &[
            "agent::fetch",
            "backend",
            "config::hosts",
            "config::automations",
            "config::interface_state",
            "config::settings",
            "config::themes",
            "update::self_update",
            "update::version_check",
        ],
    },
    // Leaf utilities.
    ModuleRules {
        name: "paths",
        allowed: &[],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "ui_control",
        allowed: &["paths"],
        allowed_path_only: &[],
    },
    // The runtime's launchers and quoting rules, re-exported: a node of its
    // own so a grant still says which modules may launch anything.
    ModuleRules {
        name: "shell",
        allowed: &[],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "workspace",
        allowed: &["paths"],
        allowed_path_only: &[],
    },
    // `main`'s own body, split across files: the loop, the workers and the
    // chrome. It is the one module whose job *is* to wire the layers together,
    // so its list is the widest — but it is a list, and a new layer reached
    // from the loop is a decision recorded here rather than an exemption.
    // Reaches the library by its crate name (`thurbox::`), which is the only
    // spelling available from inside the binary.
    ModuleRules {
        name: "coordinator",
        allowed: &[
            "clipboard",
            "kernel",
            "notifications",
            "paths",
            "session",
            "shell",
            "telemetry",
            "ui_control",
            "agent::input",
            "backend",
            "config::interface_state",
        ],
        allowed_path_only: &[],
    },
    // Leaf side-effect module: OS desktop notifications. Knows about
    // `session` (for `SessionId`), `paths` (for `PATH` lookups) and `shell`
    // (the shared quoting rules — it grew a third copy of the PowerShell one
    // before this was allowed).
    ModuleRules {
        name: "notifications",
        allowed: &["session", "paths", "shell"],
        allowed_path_only: &[],
    },
    // Leaf side-effect module: clipboard writes (native + OSC 52). Knows
    // `session` only for the `ClipboardProvider` setting; writes to the tty
    // and never reaches into agent / ui / app.
    ModuleRules {
        name: "clipboard",
        allowed: &["paths", "session"],
        allowed_path_only: &[],
    },
];

/// Modules exempt from the allowlist: `bin`, `lib`, and `main` are crate roots,
/// not architecture modules.
///
/// `coordinator` is **not** exempt. It is `main`'s own body split across
/// files, and it does wire every layer together — but "wires everything" was
/// never the same claim as "may reach anything". It has an entry above listing
/// what it actually reaches today.
const EXEMPT: &[&str] = &["bin", "lib", "main"];

/// Nodes whose every file module, at any depth, must be a governed node of
/// its own, so a new file there is a decision rather than something its
/// parent's rule silently covers. An inline `mod x { … }` belongs to the file
/// that holds it.
const SUBMODULE_GOVERNED: &[&str] = &["agent", "config", "update"];

/// A crossing that breaks a rule today, is known, and is scheduled to go.
///
/// Not an allowance: [`every_module_rule_holds`] fails on any violation this
/// table does not name, and [`transitional_table_names_only_live_crossings`]
/// fails on an entry naming one that no longer exists — so the table is
/// always exactly today's debt, item by item, and deleting an entry is how
/// the task that removes it proves it did.
struct Transitional {
    from: &'static str,
    to: &'static str,
    /// Items of `to` that `from` still reaches.
    items: &'static [&'static str],
    /// Why it is still there, and what removes it.
    why: &'static str,
}

/// Empty. A boundary change that cannot land in one step lists the crossings
/// it leaves here until the step that removes them.
const TRANSITIONAL: &[Transitional] = &[];

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// The real tree, parsed once for every test in this file.
fn src_tree() -> &'static Tree {
    static TREE: OnceLock<Tree> = OnceLock::new();
    TREE.get_or_init(|| Tree::load(&src_root()))
}

fn node_names(rules: &[ModuleRules]) -> Vec<&'static str> {
    rules.iter().map(|r| r.name).collect()
}

fn rules_for<'a>(rules: &'a [ModuleRules], node: &str) -> &'a ModuleRules {
    rules
        .iter()
        .find(|r| r.name == node)
        .unwrap_or_else(|| panic!("no rule for node `{node}`"))
}

/// Whether an edge is one its source node's rule forbids.
fn breaks_rules(rules: &ModuleRules, edge: &Edge) -> bool {
    let to = edge.to.as_str();
    if rules.allowed.contains(&to) {
        return false;
    }
    edge.in_use || !rules.allowed_path_only.contains(&to)
}

/// Every edge any rule forbids — test code included: a test may not reach
/// what its module may not.
fn violations(tree: &Tree, rules: &[ModuleRules]) -> Vec<Edge> {
    tree.edges(&node_names(rules))
        .into_iter()
        .filter(|e| breaks_rules(rules_for(rules, &e.from), e))
        .collect()
}

fn describe(tree: &Tree, rules: &[ModuleRules], edge: &Edge) -> String {
    let note = if edge.in_use
        && rules_for(rules, &edge.from)
            .allowed_path_only
            .contains(&edge.to.as_str())
    {
        " (allowed via fully-qualified path only, not `use`)"
    } else {
        ""
    };
    format!(
        "{} → {} @ {}{}{note}",
        edge.from,
        edge.target(),
        edge.site(&tree.root),
        if edge.test { " [test]" } else { "" },
    )
}

fn transitional_keys(table: &[Transitional]) -> BTreeSet<(String, String, String)> {
    table
        .iter()
        .flat_map(|t| {
            t.items
                .iter()
                .map(|item| (t.from.to_string(), t.to.to_string(), item.to_string()))
        })
        .collect()
}

/// Today's violations checked against a transitional table, both ways: the
/// violations it does not name, and the entries naming no violation.
fn reconcile(violations: &[Edge], table: &[Transitional]) -> (Vec<Edge>, Vec<String>) {
    let listed = transitional_keys(table);
    let live: BTreeSet<(String, String, String)> = violations
        .iter()
        .map(|e| (e.from.clone(), e.to.clone(), e.item.clone()))
        .collect();
    let unlisted = violations
        .iter()
        .filter(|e| !listed.contains(&(e.from.clone(), e.to.clone(), e.item.clone())))
        .cloned()
        .collect();
    let stale = listed
        .into_iter()
        .filter(|key| !live.contains(key))
        .map(|(from, to, item)| format!("  {from} → {to}::{item} no longer crosses — delete it"))
        .collect();
    (unlisted, stale)
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

/// The nodes that may decide something by naming *one* multiplexer: the route
/// grammar and its defaults (`session`) and, in a fixture, the factory. The
/// runtime's factory and adapters are the only other places one is chosen, and
/// `runtime-mux`'s own architecture test holds them. Anywhere else, a
/// `Multiplexer::Psmux` is a consumer choosing behaviour — or an OS — by
/// multiplexer, which is what the route and the registry exist to decide.
fn may_name_a_multiplexer(node: &str) -> bool {
    node == "session" || node == FACTORY
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
/// (`Multiplexer::Psmux`) — so [`TRANSITIONAL`] can list one exactly, the way
/// it lists any other crossing.
///
/// Test code is left out: a test names a multiplexer to pin what happens for
/// it, which is the opposite of deciding behaviour by one.
fn variant_violations(tree: &Tree, rules: &[ModuleRules]) -> Vec<Edge> {
    tree.references(&node_names(rules))
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

/// Everything the real tree is checked for: the node rules and the
/// multiplexer-variant rule.
fn all_violations(tree: &Tree) -> Vec<Edge> {
    let mut found = violations(tree, MODULE_RULES);
    found.extend(variant_violations(tree, MODULE_RULES));
    found
}

/// Every rule holds, except for the crossings [`TRANSITIONAL`] names.
#[test]
fn every_module_rule_holds() {
    let tree = src_tree();
    let (unlisted, _) = reconcile(&all_violations(tree), TRANSITIONAL);
    let report: String = unlisted
        .iter()
        .map(|edge| format!("  {}\n", describe(tree, MODULE_RULES, edge)))
        .collect();
    assert!(
        report.is_empty(),
        "\narchitecture violation(s), as `from → resolved item @ file:line`:\n{report}\
         Fix the reference, or — if the architecture is changing on purpose — update \
         MODULE_RULES in tests/architecture_rules.rs plus AGENTS.md and \
         docs/constitution.md. A crossing scheduled for removal belongs in TRANSITIONAL, \
         naming the task that removes it.\n"
    );
}

/// The other half of [`TRANSITIONAL`]'s contract: each entry names a crossing
/// that still exists, says why, and names each item once.
#[test]
fn transitional_table_names_only_live_crossings() {
    let tree = src_tree();
    let mut seen = BTreeSet::new();
    for entry in TRANSITIONAL {
        assert!(
            !entry.items.is_empty() && !entry.why.is_empty(),
            "TRANSITIONAL entry {} → {} names no items or no reason",
            entry.from,
            entry.to
        );
        for item in entry.items {
            assert!(
                seen.insert((entry.from, entry.to, *item)),
                "TRANSITIONAL names {} → {}::{item} twice",
                entry.from,
                entry.to
            );
        }
    }
    let (_, stale) = reconcile(&all_violations(tree), TRANSITIONAL);
    assert!(
        stale.is_empty(),
        "stale TRANSITIONAL entries:\n{}",
        stale.join("\n")
    );
}

/// The factory, as the fixture trees name it: the node that builds the
/// registry, naming every concrete adapter.
const FACTORY: &str = "backend::wiring";

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

/// Every verb a consumer needs — lifecycle, pane I/O, status delivery and
/// polling, the heartbeat — goes through the contract, so no module reaches a
/// concrete backend at all, and only the composition root reaches the factory.
///
/// Checked two ways, because each closes a door the other leaves open:
/// - **references**, resolved through `use`, `super::`, brace groups,
///   re-exports and `type` aliases, test code included — what the code does;
/// - **no grant of a whole parent** that holds files of its own, so a module
///   added under `agent` is not reachable until someone decides it is.
#[test]
fn consumers_reach_no_concrete_backend() {
    let tree = src_tree();
    let mut found = Vec::new();
    for edge in tree.edges(&node_names(MODULE_RULES)) {
        if edge.to != "backend" || !CONCRETE_BACKEND.contains(&edge.item.as_str()) {
            continue;
        }
        if edge.from == COMPOSITION_ROOT && edge.item == "wiring" {
            continue;
        }
        found.push(describe(tree, MODULE_RULES, &edge));
    }
    for rules in MODULE_RULES {
        for to in rules.allowed.iter().chain(rules.allowed_path_only) {
            if SUBMODULE_GOVERNED.contains(to) {
                found.push(format!(
                    "{} is granted all of `{to}`; name the submodules it uses",
                    rules.name
                ));
            }
        }
    }
    assert!(
        SUBMODULE_GOVERNED.contains(&"agent"),
        "`agent` must stay submodule-governed, so a grant names what it reaches"
    );
    assert!(
        found.is_empty(),
        "\na module reaches a concrete backend:\n  {}\n",
        found.join("\n  ")
    );
}

/// The modules that may name `runtime_mux` itself: the `backend` gateway, and
/// the ones re-exporting a runtime type as their own — the shell grammar
/// (`shell`, `session::agent_def`), the host model and `SessionId`
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

/// Production edges between distinct nodes, one representative site each.
fn production_graph(tree: &Tree, rules: &[ModuleRules]) -> BTreeMap<(String, String), String> {
    let mut graph = BTreeMap::new();
    for edge in tree.edges(&node_names(rules)) {
        if !edge.test {
            let site = format!("{} @ {}", edge.target(), edge.site(&tree.root));
            graph.entry((edge.from, edge.to)).or_insert(site);
        }
    }
    graph
}

fn format_cycles(cycles: &[Vec<String>], graph: &BTreeMap<(String, String), String>) -> String {
    let mut msg = String::new();
    for component in cycles {
        writeln!(msg, "  cycle {{{}}}:", component.join(", ")).unwrap();
        for ((from, to), site) in graph {
            if component.contains(from) && component.contains(to) {
                writeln!(msg, "    {from} → {site}").unwrap();
            }
        }
    }
    msg
}

/// The nodes as production code actually uses them form no cycle: a module
/// that reaches another and is reached back by it is one module in two files.
/// `#[cfg(test)]` code is left out — a test may exercise its caller.
#[test]
fn the_production_graph_is_acyclic() {
    let tree = src_tree();
    let graph = production_graph(tree, MODULE_RULES);
    let edges: BTreeSet<(String, String)> = graph.keys().cloned().collect();
    let found = cycles(&edges);
    assert!(
        found.is_empty(),
        "\ndependency cycle(s) between nodes:\n{}",
        format_cycles(&found, &graph)
    );
}

/// The declared allowlist forms no cycle either: two nodes allowed to reach
/// each other are a cycle waiting for its first reference.
#[test]
fn the_declared_graph_is_acyclic() {
    let found = cycles(&declared_edges(MODULE_RULES));
    assert!(
        found.is_empty(),
        "\nMODULE_RULES allow a cycle: {found:?} — drop one direction"
    );
}

fn declared_edges(rules: &[ModuleRules]) -> BTreeSet<(String, String)> {
    rules
        .iter()
        .flat_map(|r| {
            r.allowed
                .iter()
                .chain(r.allowed_path_only)
                .map(|to| (r.name.to_string(), to.to_string()))
        })
        .collect()
}

/// Every allowance is used by production code. An unused grant is a door
/// nobody decided to open, and test code alone does not keep one open.
#[test]
fn every_allowance_is_used() {
    let tree = src_tree();
    let used: BTreeSet<(String, String)> =
        production_graph(tree, MODULE_RULES).into_keys().collect();
    let unused: Vec<String> = declared_edges(MODULE_RULES)
        .into_iter()
        .filter(|edge| !used.contains(edge))
        .map(|(from, to)| format!("  {from} → {to}"))
        .collect();
    assert!(
        unused.is_empty(),
        "\nallowances no production code uses — delete them:\n{}",
        unused.join("\n")
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

/// Every module under `src/` must be governed: either a MODULE_RULES entry
/// or an explicit EXEMPT listing, and every module under a
/// [`SUBMODULE_GOVERNED`] node a rule of its own. Adding a module without
/// deciding its place in the architecture fails here. Also catches stale rule
/// entries.
#[test]
fn every_module_is_governed() {
    let tree = src_tree();
    let root = src_root();
    let entries =
        fs::read_dir(&root).unwrap_or_else(|e| panic!("cannot read {}: {e}", root.display()));
    for entry in entries {
        let path = entry.expect("readable directory entry").path();
        let name = if path.is_dir() {
            path.file_name()
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            path.file_stem()
        } else {
            continue;
        };
        let name = name
            .and_then(|n| n.to_str())
            .unwrap_or_else(|| panic!("non-UTF-8 path under src/: {}", path.display()))
            .to_string();
        let governed =
            MODULE_RULES.iter().any(|r| r.name == name) || EXEMPT.contains(&name.as_str());
        assert!(
            governed,
            "src/{name} has no architecture rules — add a MODULE_RULES entry \
             (or EXEMPT it) in tests/architecture_rules.rs"
        );
    }
    for parent in SUBMODULE_GOVERNED {
        for child in tree.file_descendants(parent) {
            assert!(
                MODULE_RULES.iter().any(|r| r.name == child),
                "`{child}` has no architecture rule — every module of `{parent}` is a \
                 node of its own; add a MODULE_RULES entry"
            );
        }
    }

    // Stale-entry checks: every rule and allowlist target must still exist.
    for rules in MODULE_RULES {
        assert!(
            tree.has_module(rules.name),
            "MODULE_RULES entry `{}` matches nothing under src/ — remove or rename it",
            rules.name
        );
        for target in rules.allowed.iter().chain(rules.allowed_path_only) {
            assert!(
                tree.has_module(target),
                "MODULE_RULES entry `{}` allows nonexistent module `{target}`",
                rules.name
            );
        }
    }
    for entry in TRANSITIONAL {
        for node in [entry.from, entry.to] {
            assert!(
                MODULE_RULES.iter().any(|r| r.name == node),
                "TRANSITIONAL names `{node}`, which is not a node"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The resolver on fixture trees: each is a tiny `src/` under
// `tests/fixtures/architecture/`, never compiled, holding one shape the
// resolver must see through.
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

/// The cycle this suite was written for: the contract names the tmux adapter's
/// role type, the adapter implements the contract, and the protocol helper
/// takes the contract's pane size while the contract takes the helper's
/// snapshot — reached through `super::`, a nested brace group with `self`, a
/// re-export and a bare child path, none of which the old first-segment check
/// followed.
#[test]
fn the_resolver_sees_the_three_node_backend_cycle() {
    let tree = fixture("backend_cycle");
    let nodes = [
        "agent",
        "agent::backend",
        "agent::tmux",
        "agent::control_mode",
    ];
    let edges = edge_set(&tree, &nodes, true);
    assert_eq!(
        cycles(&edges),
        vec![vec![
            "agent::backend".to_string(),
            "agent::control_mode".to_string(),
            "agent::tmux".to_string(),
        ]],
        "edges found: {edges:?}"
    );
}

/// PR #1272's shape: a `mux` core importing the registry's transport while the
/// registry builds every adapter, and the core and control mode importing each
/// other. Two cycles in one component.
#[test]
fn the_resolver_sees_the_mux_registry_cycles() {
    let tree = fixture("mux_registry_cycle");
    let nodes = [
        "agent",
        "agent::mux",
        "agent::registry",
        "agent::tmux",
        "agent::psmux",
        "agent::control_mode",
    ];
    let found = cycles(&edge_set(&tree, &nodes, true));
    assert_eq!(
        found,
        vec![vec![
            "agent::control_mode".to_string(),
            "agent::mux".to_string(),
            "agent::psmux".to_string(),
            "agent::registry".to_string(),
            "agent::tmux".to_string(),
        ]]
    );
}

/// Aliases and re-exports are edges: a `type` alias of an adapter's type is a
/// crossing where it is declared, a use of that alias from elsewhere resolves
/// to the adapter, and so does a `pub use` re-export read through its parent.
/// A grant of the parent (`agent`) admits none of it.
#[test]
fn aliases_and_reexports_launder_nothing() {
    let tree = fixture("laundering");
    let rules = [
        ModuleRules {
            name: "agent",
            allowed: &["agent::tmux"],
            allowed_path_only: &[],
        },
        ModuleRules {
            name: "agent::tmux",
            allowed: &[],
            allowed_path_only: &[],
        },
        ModuleRules {
            name: "kernel",
            allowed: &[],
            allowed_path_only: &["agent"],
        },
    ];
    let found: BTreeSet<String> = violations(&tree, &rules)
        .iter()
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
/// itself and its associated items (`Multiplexer::ALL`) stay free to use, the
/// factory may name what it builds, and a test may name the one it pins.
#[test]
fn a_multiplexer_variant_is_named_only_where_one_is_chosen() {
    let tree = fixture("mux_variants");
    let rules = [
        ModuleRules {
            name: "session",
            allowed: &[],
            allowed_path_only: &[],
        },
        ModuleRules {
            name: "kernel",
            allowed: &["session"],
            allowed_path_only: &[],
        },
        ModuleRules {
            name: FACTORY,
            allowed: &["session"],
            allowed_path_only: &[],
        },
    ];
    let found: BTreeSet<String> = variant_violations(&tree, &rules)
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
/// declared under `#[cfg(test)] mod x;` — so it cannot make a production cycle
/// (while still being checked against the rules, see [`violations`]).
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

/// The table is checked both ways: a crossing it does not name fails, and so
/// does an entry naming a crossing that is gone.
#[test]
fn the_transitional_table_fails_on_a_new_and_on_a_stale_crossing() {
    let tree = fixture("laundering");
    let rules = [
        ModuleRules {
            name: "agent",
            allowed: &["agent::tmux"],
            allowed_path_only: &[],
        },
        ModuleRules {
            name: "agent::tmux",
            allowed: &[],
            allowed_path_only: &[],
        },
        ModuleRules {
            name: "kernel",
            allowed: &[],
            allowed_path_only: &["agent"],
        },
    ];
    let table = [Transitional {
        from: "kernel",
        to: "agent::tmux",
        items: &["Index", "gone"],
        why: "fixture",
    }];
    let (unlisted, stale) = reconcile(&violations(&tree, &rules), &table);
    let unlisted: BTreeSet<String> = unlisted.iter().map(Edge::target).collect();
    assert_eq!(
        unlisted,
        ["agent::tmux", "agent::tmux::spawn"]
            .into_iter()
            .map(str::to_string)
            .collect()
    );
    assert_eq!(
        stale,
        vec!["  kernel → agent::tmux::gone no longer crosses — delete it".to_string()]
    );
}

/// The declared graph is checked for what it permits, not what is used:
/// `storage` and `telemetry` allowed to reach each other are a cycle.
#[test]
fn the_declared_check_sees_a_mutual_allowance() {
    let rules = [
        ModuleRules {
            name: "storage",
            allowed: &["telemetry"],
            allowed_path_only: &[],
        },
        ModuleRules {
            name: "telemetry",
            allowed: &[],
            allowed_path_only: &["storage"],
        },
    ];
    assert_eq!(
        cycles(&declared_edges(&rules)),
        vec![vec!["storage".to_string(), "telemetry".to_string()]]
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
    for reference in tree.references(&["kernel", "git", "storage"]) {
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
