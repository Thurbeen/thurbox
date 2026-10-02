//! Architecture rules enforced as tests (allowlist model over resolved edges).
//!
//! Every module under `src/` must appear in [`MODULE_RULES`] (or in
//! [`EXEMPT`]) and may only reference the nodes its entry allows —
//! `every_module_is_governed` fails when a new module is added without a rule,
//! so the architecture is an explicit decision per module.
//!
//! A rule's name is a **node**: a top-level module (`kernel`) or a governed
//! submodule (`backend::tmux`). A file belongs to the deepest node containing it,
//! and a reference is judged by the node it *resolves to* (see `resolver`):
//! `super::`, `self::`, bare child-module paths, nested brace groups, `as`
//! renames, imported names, `pub use` re-exports and `type` aliases are all
//! followed, so no import shape and no alias carries a crossing past a rule. A
//! grant names exactly one node and never its children: allowing `backend`
//! admits nothing in a governed `backend::tmux`.
//!
//! The graph is checked as a whole too: the actual production edges and the
//! declared allowlist must both be acyclic, and every allowance must be used
//! by production code. A crossing that is known and scheduled for removal is
//! listed in [`TRANSITIONAL`], which must equal the violations found — both
//! ways, so a new crossing fails and so does a stale entry.
//!
//! The layering mirrors AGENTS.md ("Module Dependency Rules") and
//! docs/CONSTITUTION.md §2. If a rule change is intentional, update those
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
    /// submodule path (`backend::tmux`).
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
    // Coding-agent definitions and their config: agents.toml, extensions,
    // hooks, settings, themes, preflight, self-update. Never a session
    // backend: those live in their own nodes below, and a backend may read an
    // agent's config, so the reverse would be a cycle.
    //
    // Every file is a node of its own (`SUBMODULE_GOVERNED`), so a grant names
    // the config it reads and a file added here is reachable by nobody until
    // that is decided. The root only re-exports the provider types.
    ModuleRules {
        name: "agent",
        allowed: &["agent::generic", "agent::provider"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "agent::agent_config",
        allowed: &["session", "paths"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "agent::extension_config",
        allowed: &["session", "paths", "agent::agent_config"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "agent::generic",
        allowed: &["session", "agent::provider"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "agent::hooks_config",
        allowed: &["session", "paths", "agent::agent_config"],
        allowed_path_only: &[],
    },
    // hosts.toml, and the cached registry of it every process shares. Its own
    // node so that reading global host config is a visible decision: the
    // backend contract and the pure registry must not. `shell` for the one
    // `wsl.exe` constructor every launch goes through (`shell::wsl_exe`).
    ModuleRules {
        name: "agent::host_config",
        allowed: &["session", "paths", "shell", "agent::agent_config"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "agent::host_path",
        allowed: &["session", "shell"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "agent::input",
        allowed: &[],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "agent::json_merge",
        allowed: &[],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "agent::preflight",
        allowed: &["session", "paths", "agent::agent_config"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "agent::provider",
        allowed: &["session"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "agent::self_update",
        allowed: &[
            "session",
            "paths",
            "shell",
            "agent::extension_config",
            "agent::version_check",
        ],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "agent::settings_config",
        allowed: &["session", "paths", "agent::agent_config"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "agent::themes_config",
        allowed: &["session", "paths", "agent::agent_config"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "agent::toml_merge",
        allowed: &[],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "agent::version_check",
        allowed: &["session", "paths", "agent::extension_config"],
        allowed_path_only: &[],
    },
    // The boundary's root: re-exports of the contract, nothing of its own —
    // and never an adapter, which would put one a `use` away from every
    // consumer.
    ModuleRules {
        name: "backend",
        allowed: &["backend::contract", "backend::pane", "backend::registry"],
        allowed_path_only: &[],
    },
    // The contract every adapter implements and the values that cross it.
    // Names no adapter, no protocol helper and no global config: the bottom of
    // the boundary.
    ModuleRules {
        name: "backend::contract",
        allowed: &[],
        allowed_path_only: &[],
    },
    // Which window is whose: thurbox's window-naming convention and the
    // resolution rule (ADR-25), over the listing the contract defines.
    ModuleRules {
        name: "backend::identity",
        allowed: &["backend::contract"],
        allowed_path_only: &[],
    },
    // The pane machinery every backend's stream is wired into: the reader
    // loop, the vt100 parser, the signals it raises.
    ModuleRules {
        name: "backend::pane",
        allowed: &[
            "session",
            "backend::contract",
            "backend::identity",
            "backend::osc8",
            "backend::output_wake",
        ],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "backend::osc8",
        allowed: &["session"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "backend::output_wake",
        allowed: &[],
        allowed_path_only: &[],
    },
    // A container of backends. Knows the contract and nothing that builds one.
    ModuleRules {
        name: "backend::registry",
        allowed: &["session", "backend::contract"],
        allowed_path_only: &[],
    },
    // What fills the registry: the only node that names an adapter, and the
    // only one that reads host config to do it. Referenced only by the
    // composition roots — see `only_the_composition_roots_name_the_factory`.
    ModuleRules {
        name: "backend::wiring",
        allowed: &[
            "session",
            "shell",
            "agent::host_config",
            "backend::contract",
            "backend::registry",
            "backend::tmux",
            "backend::psmux",
        ],
        allowed_path_only: &[],
    },
    // The tmux command and control-mode protocol tmux and psmux both speak.
    // Shared grammar, not an adapter: it may know the contract, never an
    // adapter using it (`the_adapters_are_peers`). Its root only declares the
    // modules below.
    ModuleRules {
        name: "backend::tmux_compat",
        allowed: &[],
        allowed_path_only: &[],
    },
    // A tmux-protocol server as a session backend, generic over the
    // multiplexer: everything the two adapters share, asking each what its
    // server can do and never which it is.
    ModuleRules {
        name: "backend::tmux_compat::server",
        allowed: &[
            "session",
            "paths",
            "shell",
            "agent::host_path",
            "agent::preflight",
            "backend::contract",
            "backend::identity",
            "backend::tmux_compat::control_mode",
            "backend::instance",
            "backend::tmux_compat::transport",
        ],
        allowed_path_only: &[],
    },
    // Which server an instance's sessions live on (ADR-12).
    ModuleRules {
        name: "backend::instance",
        allowed: &["session", "paths"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "backend::tmux_compat::control_mode",
        allowed: &[
            "shell",
            "backend::contract",
            "backend::tmux_compat::transport",
        ],
        allowed_path_only: &[],
    },
    // How the multiplexer is launched: locally, over ssh, or in a WSL distro.
    ModuleRules {
        name: "backend::tmux_compat::transport",
        allowed: &["shell", "agent::preflight"],
        allowed_path_only: &[],
    },
    // The two adapters, peers: each reaches the protocol helper and never the
    // other; nothing reaches either but the factory.
    ModuleRules {
        name: "backend::tmux",
        allowed: &[
            "session",
            "shell",
            "backend::contract",
            "backend::tmux_compat::control_mode",
            "backend::tmux_compat::server",
            "backend::tmux_compat::transport",
        ],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "backend::psmux",
        allowed: &[
            "session",
            "shell",
            "backend::contract",
            "backend::instance",
            "backend::tmux_compat::control_mode",
            "backend::tmux_compat::server",
            "backend::tmux_compat::transport",
        ],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "git",
        allowed: &["session", "paths", "shell"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "storage",
        allowed: &["session", "sync", "paths"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "sync",
        allowed: &["session"],
        allowed_path_only: &[],
    },
    // `shell` builds the host launchers (ssh/wsl) for reading a remote
    // session's credentials where the agent actually runs.
    ModuleRules {
        name: "usage",
        allowed: &["session", "shell"],
        // `paths::home_dir()` (fully-qualified, never `use`) to resolve agent
        // credential files cross-platform ($HOME / %USERPROFILE%).
        allowed_path_only: &["paths"],
    },
    // Headless session ops: no TUI state or PTY-attached backend. Reaches the
    // agent config and the backend contract via fully-qualified paths only
    // (never `use`), same pattern as the cli module. `shell` for the same
    // reason `agent` has it: `host_cli` spells a `thurbox-cli` invocation for a
    // host's `sh` or PowerShell, and the two quoting rules have exactly one
    // home (`shell::posix_quote` / `powershell_quote`).
    ModuleRules {
        name: "session_ops",
        allowed: &[
            "session",
            "storage",
            "git",
            "sync",
            "paths",
            "workspace",
            "shell",
        ],
        allowed_path_only: &[
            "agent::agent_config",
            "agent::extension_config",
            "agent::generic",
            "agent::hooks_config",
            "agent::host_config",
            "agent::host_path",
            "agent::json_merge",
            "agent::preflight",
            "agent::provider",
            "agent::self_update",
            "agent::settings_config",
            "agent::toml_merge",
            "agent::version_check",
            "backend::contract",
            "backend::identity",
            "backend::instance",
            "backend::registry",
        ],
    },
    // Thin headless dispatch — must not depend on TUI or the live backend.
    ModuleRules {
        name: "cli",
        allowed: &[
            "session",
            "storage",
            "session_ops",
            "sync",
            "paths",
            "notifications",
        ],
        // `kernel` for the two subcommands that drive the *interface's* own
        // files — `plugin` (`check` loads the real host: the failures worth
        // reporting are declaration-shaped — no `render`, an unplaced slot, a
        // clashing key — and a syntax check passes all of them) and `config`
        // (the interface directory and its `ui.json` overrides). Both are
        // kernel-owned surfaces asked about from outside, not session logic
        // duplicated here; the session engine the CLI shares with the loop is
        // `session_ops`, and that is where the reap sweep it drives lives.
        // Path-only, like the agent config, so the crossing stays visible at each call
        // site.
        allowed_path_only: &[
            "agent::agent_config",
            "agent::extension_config",
            "agent::hooks_config",
            "agent::host_config",
            "agent::preflight",
            "agent::self_update",
            "agent::settings_config",
            "agent::themes_config",
            "agent::version_check",
            "backend::contract",
            "backend::instance",
            "backend::registry",
            "kernel",
        ],
    },
    // The plugin kernel: hosts the Lua VM the whole UI is written in. Reads the
    // session engine to build the snapshot plugins render from (`storage` +
    // `sync` for the rows, `session` for the types, `paths` for the DB and
    // plugin directories).
    //
    // `git` IS allowed, and the rule is about *where*: the worker-backed stores
    // (`diff`, `repos`, `packages`, `command`) shell out to it off-thread, which
    // is rule 5 rather than an exception to it. What must not happen is a `git`
    // call from a render path — that is enforced by the loop's shape (a plugin
    // returns a tree; it cannot call Rust), not by this allowlist.
    //
    // `shell` for the reason `session_ops` has it: `runs` spells a `cd <dir> &&
    // <program>` script for a host, and POSIX quoting has exactly one home
    // (`shell::posix_quote`).
    ModuleRules {
        name: "kernel",
        allowed: &[
            "session",
            "storage",
            "sync",
            "paths",
            "session_ops",
            "git",
            "notifications",
            "shell",
        ],
        // Live agent terminals: `kernel::terminal` adopts a session's real pane
        // through the backend contract and paints its vt100 screen.
        // `kernel::metrics` fetches account usage through `usage`. All are
        // reachable by fully-qualified path only (never `use`), the same rule
        // `session_ops` and `cli` follow, so every crossing into the
        // side-effect layer is visible at its call site.
        allowed_path_only: &[
            "agent::agent_config",
            "agent::extension_config",
            "agent::host_config",
            "agent::preflight",
            "agent::self_update",
            "agent::settings_config",
            "agent::themes_config",
            "agent::version_check",
            "backend::contract",
            "backend::identity",
            "backend::pane",
            "backend::registry",
            "usage",
        ],
    },
    // Leaf utilities.
    ModuleRules {
        name: "paths",
        allowed: &[],
        allowed_path_only: &[],
    },
    // `session` for the one conversion from a host entry to its launcher
    // (`HostLauncher::for_host`), which every remote command shares.
    ModuleRules {
        name: "shell",
        allowed: &["session"],
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
            "agent::input",
            "agent::settings_config",
            "backend::output_wake",
            "backend::wiring",
            "clipboard",
            "kernel",
            "paths",
            "session",
            "session_ops",
            "shell",
            "storage",
        ],
        allowed_path_only: &[],
    },
    // Leaf side-effect module: OS desktop notifications. Knows about
    // `session` (for `SessionId`), `paths` (for the DB path the click callback
    // writes to), `shell` (the shared quoting rules — it grew a third copy of
    // the PowerShell one before this was allowed) and `storage`, through which
    // the click handler records its focus request. That last one used to be a
    // raw `rusqlite` statement here instead, which was a carve-out this
    // allowlist could describe but not enforce: the module that owns a table's
    // SQL is `storage`, and now this goes through it like every other write.
    ModuleRules {
        name: "notifications",
        allowed: &["session", "paths", "shell"],
        allowed_path_only: &["storage"],
    },
    // Leaf side-effect module: clipboard writes (native + OSC 52). Knows
    // `session` only for the `ClipboardProvider` setting; writes to the tty
    // and never reaches into agent / ui / app / storage.
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
const SUBMODULE_GOVERNED: &[&str] = &["backend", "agent"];

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

/// Where `Multiplexer` lives, as a resolved path: its variants are the one
/// kind of item a node may reach *through* a grant and still not name.
const MULTIPLEXER: &[&str] = &["session", "multiplexer", "Multiplexer"];

/// The multiplexers a route can name. A name here is not an implementation:
/// which of them work is what the registry says, at runtime.
const MULTIPLEXER_VARIANTS: &[&str] = &["Tmux", "Psmux", "Rmux", "Herdr"];

/// The nodes that may decide something by naming *one* multiplexer: the route
/// grammar and its defaults (`session`), the factory that picks an adapter per
/// multiplexer, and the adapters, each of which is one. Anywhere else, a
/// `Multiplexer::Psmux` is a consumer choosing behaviour — or an OS — by
/// multiplexer, which is what the route and the registry exist to decide.
fn may_name_a_multiplexer(node: &str) -> bool {
    node == "session" || node == FACTORY || ADAPTERS.contains(&node)
}

/// The multiplexer variant a resolved path names, if it names one.
fn multiplexer_variant(reference: &Reference) -> Option<&str> {
    let path = &reference.path;
    (path.len() == MULTIPLEXER.len() + 1
        && path[..MULTIPLEXER.len()].iter().eq(MULTIPLEXER.iter())
        && MULTIPLEXER_VARIANTS.contains(&path[MULTIPLEXER.len()].as_str()))
    .then(|| path[MULTIPLEXER.len()].as_str())
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
         docs/CONSTITUTION.md. A crossing scheduled for removal belongs in TRANSITIONAL, \
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

/// The node that builds the registry, naming every concrete adapter.
const FACTORY: &str = "backend::wiring";

/// The concrete adapters. Each is reached only through [`FACTORY`], and each
/// serves one multiplexer of its own.
const ADAPTERS: &[&str] = &["backend::tmux", "backend::psmux"];

/// The tmux command and control-mode protocol both adapters above speak: a
/// helper either may use, which uses neither.
const PROTOCOL_HELPER: &str = "backend::tmux_compat";

fn in_protocol_helper(node: &str) -> bool {
    node == PROTOCOL_HELPER || node.starts_with(&format!("{PROTOCOL_HELPER}::"))
}

/// Only the composition roots may build the registry — `coordinator` here, and
/// the exempt crate roots (`main`, `bin/`) — and only the factory may name an
/// adapter. A consumer that builds its own registry sees a different set of
/// backends from the one the process was wired with, and one that names an
/// adapter has stopped using the contract. No factory crossing is
/// transitional: the registry is built at the roots and injected.
#[test]
fn only_the_composition_roots_name_the_factory() {
    for rules in MODULE_RULES {
        let grants = || rules.allowed.iter().chain(rules.allowed_path_only);
        if rules.name != "coordinator" {
            assert!(
                !grants().any(|to| *to == FACTORY),
                "`{}` may reference {FACTORY}; only a composition root may",
                rules.name
            );
        }
        if rules.name != FACTORY {
            for adapter in ADAPTERS {
                assert!(
                    !grants().any(|to| to == adapter),
                    "`{}` may reference the adapter {adapter}; only {FACTORY} may",
                    rules.name
                );
            }
        }
    }
    for entry in TRANSITIONAL {
        assert_ne!(
            entry.to, FACTORY,
            "{} → {FACTORY} is not transitional: inject the registry the root built",
            entry.from
        );
    }
}

/// The nodes that consume backends: the session engine, the CLI and the
/// kernel. Each is handed a registry and reaches what is in it through the
/// contract.
const BACKEND_CONSUMERS: &[&str] = &["session_ops", "cli", "kernel"];

/// One multiplexer's code, or the grammar only some multiplexers speak, or
/// the factory that names them: what a consumer must never reach by name.
fn is_concrete_backend(node: &str) -> bool {
    node == FACTORY || ADAPTERS.contains(&node) || in_protocol_helper(node)
}

/// Every verb a consumer needs — lifecycle, pane I/O, status delivery and
/// polling, the heartbeat — goes through the contract, so no consumer can
/// reach a concrete backend at all.
///
/// Checked three ways, because each closes a door the others leave open:
/// - **references**, resolved through `use`, `super::`, brace groups,
///   re-exports and `type` aliases, test code included — what the code does;
/// - **grants**, followed transitively — what a consumer could start doing
///   without this file changing, including through a node it is allowed;
/// - **no grant of a whole parent** that holds files of its own, so a module
///   added under `agent` is not reachable until someone decides it is.
#[test]
fn consumers_reach_no_concrete_backend() {
    let tree = src_tree();
    let mut found = Vec::new();
    for edge in tree.edges(&node_names(MODULE_RULES)) {
        if BACKEND_CONSUMERS.contains(&edge.from.as_str()) && is_concrete_backend(&edge.to) {
            found.push(describe(tree, MODULE_RULES, &edge));
        }
    }
    let declared = declared_edges(MODULE_RULES);
    for consumer in BACKEND_CONSUMERS {
        let mut reach = vec![consumer.to_string()];
        let mut seen = BTreeSet::new();
        while let Some(node) = reach.pop() {
            if !seen.insert(node.clone()) {
                continue;
            }
            for (from, to) in &declared {
                if *from == node {
                    if is_concrete_backend(to) {
                        found.push(format!("{consumer} may reach {to} (granted to {from})"));
                    }
                    reach.push(to.clone());
                }
            }
        }
        let rules = rules_for(MODULE_RULES, consumer);
        for to in rules.allowed.iter().chain(rules.allowed_path_only) {
            if SUBMODULE_GOVERNED.contains(to) {
                found.push(format!(
                    "{consumer} is granted all of `{to}`; name the submodules it uses"
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
        "\na consumer reaches a concrete backend:\n  {}\n",
        found.join("\n  ")
    );
}

/// The adapters are peers: neither reaches the other, and the protocol
/// helper they share reaches neither — in code, test code included, or in a
/// grant. A quirk of one multiplexer is then a body in its own adapter, never a
/// branch in code the other runs; and an adapter that serves a second
/// multiplexer through flags is an adapter missing.
#[test]
fn the_adapters_are_peers() {
    let tree = src_tree();
    let mut found = Vec::new();
    for adapter in ADAPTERS {
        if !tree.has_module(adapter) {
            found.push(format!(
                "{adapter} does not exist, so another adapter serves its multiplexer"
            ));
        }
    }
    let crosses = |from: &str, to: &str| {
        ADAPTERS.contains(&to)
            && from != to
            && (ADAPTERS.contains(&from) || in_protocol_helper(from))
    };
    for edge in tree.edges(&node_names(MODULE_RULES)) {
        if crosses(&edge.from, &edge.to) {
            found.push(describe(tree, MODULE_RULES, &edge));
        }
    }
    for (from, to) in declared_edges(MODULE_RULES) {
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

/// The multiplexers each adapter's production code names, and the ones the
/// factory names — resolved references to `session::Multiplexer`'s variants.
fn multiplexers_named(tree: &Tree) -> (BTreeMap<&'static str, BTreeSet<String>>, BTreeSet<String>) {
    let mut owned: BTreeMap<&'static str, BTreeSet<String>> =
        ADAPTERS.iter().map(|a| (*a, BTreeSet::new())).collect();
    let mut served = BTreeSet::new();
    for reference in tree.references(&node_names(MODULE_RULES)) {
        if reference.test {
            continue;
        }
        let Some(variant) = multiplexer_variant(&reference) else {
            continue;
        };
        if let Some(names) = owned.get_mut(reference.from.as_str()) {
            names.insert(variant.to_string());
        }
        if reference.from == FACTORY {
            served.insert(variant.to_string());
        }
    }
    (owned, served)
}

/// One multiplexer, one adapter: each adapter names exactly the multiplexer it
/// is, no two adapters name the same one, and every multiplexer the factory
/// serves is one an adapter is. An adapter serving a multiplexer it does not
/// name is deciding by the binary's *name* instead — `if mux == "psmux"` —
/// which no resolved reference shows and every rule above would miss.
#[test]
fn every_multiplexer_the_factory_serves_has_an_adapter_of_its_own() {
    let (owned, served) = multiplexers_named(src_tree());
    let mut found = Vec::new();
    let mut owner: BTreeMap<String, &str> = BTreeMap::new();
    for (adapter, names) in &owned {
        if names.len() != 1 {
            found.push(format!(
                "{adapter} names {} multiplexer(s) {names:?}; an adapter is exactly one",
                names.len()
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
                "{FACTORY} serves Multiplexer::{name}, which no adapter is (adapters: {owned:?})"
            ));
        }
    }
    assert!(
        found.is_empty(),
        "\na multiplexer is served without an adapter of its own:\n  {}\n",
        found.join("\n  ")
    );
}

/// The files that say where a session runs and what a backend is, for every
/// host and every multiplexer alike: the route grammar and the contract.
const NEUTRAL_FILES: &[&str] = &["session/route.rs", "backend/contract.rs"];

/// What a neutral file may not reach: how one kind of host is launched
/// (`shell`'s ssh/wsl launchers, a host's own `hosts.toml` entry and the
/// loader of it) or how one multiplexer is driven (an adapter, or the tmux
/// command grammar two of them share).
const HOST_OR_MUX_SPECIFIC: &[&str] = &[
    "shell",
    "session::host_def",
    "agent::host_config",
    "backend::tmux",
    "backend::psmux",
    "backend::tmux_compat",
];

/// The route and the contract are the same for every host OS, launcher and
/// multiplexer (ADR-13): they reach no launcher and no adapter, and decide
/// nothing by the OS this build was compiled for — a Windows thurbox drives a
/// Linux host, and a Linux one a Windows host. A platform is a host's, read
/// from its configuration; a behaviour is a backend's, read from what it can
/// do.
#[test]
fn the_route_and_the_contract_know_no_launcher_adapter_or_build_os() {
    let tree = src_tree();
    let root = src_root();
    let mut found = Vec::new();
    for reference in tree.references(&node_names(MODULE_RULES)) {
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

/// `message send`/`reply` deliver a body through the recipient agent's own
/// inbox and never by typing into its pane. The keystroke nudge this replaced
/// had to guess from screen contents whether typing was safe, and could not;
/// so the rule is that the multiplexer is unreachable from the message path at
/// all, rather than that some gate around it holds.
///
/// Starts at the two files of the path and follows every resolved reference
/// into a sibling `cli` file, so a helper that later grows a pane write is
/// caught too. A reference is judged by the node it resolves to, so a
/// multiplexer item re-exported under another name still counts. `cli/mod.rs`
/// is not followed: the path reaches it only for `CommandError`, and it is the
/// dispatcher that names every subcommand.
#[test]
fn message_delivery_never_reaches_the_multiplexer() {
    const FORBIDDEN: &[&str] = &["agent", "backend", "session_ops", "kernel", "coordinator"];
    let tree = src_tree();
    let references = tree.references(&node_names(MODULE_RULES));
    let cli = src_root().join("cli");
    let mut queue = vec!["messages".to_string(), "delivery".to_string()];
    let mut seen: Vec<String> = Vec::new();
    let mut report = String::new();
    while let Some(name) = queue.pop() {
        if seen.contains(&name) {
            continue;
        }
        seen.push(name.clone());
        let file = cli.join(format!("{name}.rs"));
        for r in references.iter().filter(|r| r.file == file) {
            let root = r.path.first().map(String::as_str).unwrap_or_default();
            if FORBIDDEN.contains(&root) {
                writeln!(
                    report,
                    "  src/cli/{name}.rs:{}: {}",
                    r.line,
                    r.path.join("::")
                )
                .unwrap();
            }
            if let [first, next, ..] = r.path.as_slice() {
                if first == "cli" && cli.join(format!("{next}.rs")).is_file() {
                    queue.push(next.clone());
                }
            }
        }
        let content = fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()));
        let stripped = strip_comments_and_strings(&content);
        // Strings and comments are gone, so any hit is an identifier.
        for (at, _) in stripped.match_indices("tmux") {
            let line = stripped[..at].matches('\n').count() + 1;
            writeln!(report, "  src/cli/{name}.rs:{line}: names tmux").unwrap();
        }
    }
    assert!(
        seen.len() > 2,
        "the walk should reach the helpers the message path uses"
    );
    assert!(
        report.is_empty(),
        "the message path reaches the multiplexer (visited {seen:?}):\n{report}"
    );
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
/// `storage` and `sync` allowed to reach each other are a cycle.
#[test]
fn the_declared_check_sees_a_mutual_allowance() {
    let rules = [
        ModuleRules {
            name: "storage",
            allowed: &["sync"],
            allowed_path_only: &[],
        },
        ModuleRules {
            name: "sync",
            allowed: &[],
            allowed_path_only: &["storage"],
        },
    ];
    assert_eq!(
        cycles(&declared_edges(&rules)),
        vec![vec!["storage".to_string(), "sync".to_string()]]
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
