//! Architecture rules enforced as tests (allowlist model over resolved edges).
//!
//! Every module under `src/` must appear in [`MODULE_RULES`] (or in
//! [`EXEMPT`]) and may only reference the nodes its entry allows —
//! `every_module_is_governed` fails when a new module is added without a rule,
//! so the architecture is an explicit decision per module.
//!
//! A rule's name is a **node**: a top-level module (`kernel`) or a governed
//! submodule (`agent::tmux`). A file belongs to the deepest node containing it,
//! and a reference is judged by the node it *resolves to* (see `resolver`):
//! `super::`, `self::`, bare child-module paths, nested brace groups, `as`
//! renames, imported names, `pub use` re-exports and `type` aliases are all
//! followed, so no import shape and no alias carries a crossing past a rule. A
//! grant names exactly one node and never its children: allowing `agent`
//! admits nothing in a governed `agent::tmux`.
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
    /// submodule path (`agent::tmux`).
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
    ModuleRules {
        name: "agent",
        allowed: &["session", "paths", "shell"],
        allowed_path_only: &[],
    },
    // hosts.toml, and the cached registry of it every process shares. Its own
    // node so that reading global host config is a visible decision: the
    // backend contract and the pure registry must not.
    ModuleRules {
        name: "agent::host_config",
        allowed: &["session", "paths", "agent"],
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
            "agent::host_config",
            "backend::contract",
            "backend::registry",
            "backend::tmux",
        ],
        allowed_path_only: &[],
    },
    // The tmux command and control-mode protocol. Shared grammar, not an
    // adapter: it may know the contract, never an adapter using it. Its root
    // only declares the two below.
    ModuleRules {
        name: "backend::tmux_compat",
        allowed: &[],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "backend::tmux_compat::control_mode",
        allowed: &[
            "session",
            "shell",
            "backend::contract",
            "backend::tmux_compat::transport",
        ],
        allowed_path_only: &[],
    },
    // How the multiplexer is launched: locally, over ssh, or in a WSL distro.
    // `session` for the one local-multiplexer default (`Multiplexer`).
    ModuleRules {
        name: "backend::tmux_compat::transport",
        allowed: &["session", "shell", "agent"],
        allowed_path_only: &[],
    },
    // The tmux adapter. Reaches the contract, the identity rule and the
    // protocol helper; nothing reaches it but the factory.
    ModuleRules {
        name: "backend::tmux",
        allowed: &[
            "session",
            "paths",
            "shell",
            "agent",
            "backend::contract",
            "backend::identity",
            "backend::tmux_compat::control_mode",
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
            "agent",
            "agent::host_config",
            "backend::contract",
            "backend::identity",
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
        // Path-only, like `agent`, so the crossing stays visible at each call
        // site.
        allowed_path_only: &["agent", "agent::host_config", "backend::contract", "kernel"],
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
            "agent",
            "agent::host_config",
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
            "agent",
            "backend::output_wake",
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
const SUBMODULE_GOVERNED: &[&str] = &["backend"];

/// The task in the backend-boundary sequence that removes a transitional
/// crossing. F7 is the last, and ends with [`TRANSITIONAL`] empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Remover {
    /// Lifecycle through the contract, one registry injected at the roots.
    F5a,
    /// Pane I/O through `locate` and the contract's pane verbs.
    F5b,
    /// Host platform and launcher separated from the multiplexer. Removed
    /// its crossings; kept so the sequence reads in order.
    #[allow(dead_code)]
    F6,
    /// psmux extracted into its own adapter. It owns no crossing today; the
    /// variant is here so an entry can name it once one exists.
    #[allow(dead_code)]
    F6b,
    /// Status delivery and the heartbeat owned by the backend.
    F7,
}

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
    remover: Remover,
    why: &'static str,
}

const TRANSITIONAL: &[Transitional] = &[
    // Registry construction outside the composition roots: one registry, built
    // at `coordinator::boot` and `bin/thurbox-cli`, injected everywhere else.
    Transitional {
        from: "kernel",
        to: "backend::wiring",
        items: &["configured"],
        remover: Remover::F5a,
        why: "Terminals::new and the create-flow snapshot build their own registry",
    },
    Transitional {
        from: "session_ops",
        to: "backend::wiring",
        items: &["configured", "implements"],
        remover: Remover::F5a,
        why: "spawn builds a registry to ask whether it supports a route, then drops it; \
              lifecycle asks the factory whether a row's multiplexer has an adapter",
    },
    // Lifecycle — spawn, restart, restore, stop, delete, reap, owed teardown,
    // rename, register — through the tmux adapter's free functions.
    Transitional {
        from: "session_ops",
        to: "backend::tmux",
        items: &[
            "SessionPanes",
            "agent_window",
            "agent_window_alive",
            "kill_remote_windows",
            "kill_shell_window",
            "kill_window",
            "kill_window_at",
            "known_host_socket",
            "local_window_index",
            "remote_window_index",
            "rename_session_windows",
            "spawn_window",
            "spawn_window_remote",
            "stamp_local_window",
            "window_pane_pid",
        ],
        remover: Remover::F5a,
        why: "every lifecycle verb calls the tmux adapter instead of the row's backend",
    },
    Transitional {
        from: "cli",
        to: "backend::tmux",
        items: &["agent_window", "stamp_local_window"],
        remover: Remover::F5a,
        why: "`session register` locates and stamps a local tmux window directly",
    },
    // Pane I/O addressed by (id, name) on the local tmux server.
    Transitional {
        from: "session_ops",
        to: "backend::tmux",
        items: &["send_text_now"],
        remover: Remover::F5b,
        why: "text reaches a pane through the tmux adapter, not locate + the contract",
    },
    Transitional {
        from: "cli",
        to: "backend::tmux",
        items: &[
            "NAMED_KEYS",
            "PanePath",
            "agent_pane_path",
            "capture_pane_text",
            "pane_state",
            "resolve_key",
            "send_key_now",
            "send_prompt_after_delay",
            "send_prompt_now",
            "window_exists",
        ],
        remover: Remover::F5b,
        why: "send, key, capture, watch, automations, tasks and doctor read panes via tmux",
    },
    Transitional {
        from: "kernel",
        to: "backend::tmux",
        items: &["pane_state", "send_prompt_after_delay"],
        remover: Remover::F5b,
        why: "dispatch_task and the snapshot's pane state go around the contract",
    },
    // Status delivery, the heartbeat, and the instance socket (ADR-12) they
    // are addressed by — backend-owned once status is.
    Transitional {
        from: "session_ops",
        to: "backend::tmux",
        items: &[
            "SOCKET_OVERRIDE_ENV",
            "SOCKET_OWNER_ENV",
            "TMUX_SOCKET",
            "host_socket",
            "learn_host_socket",
            "list_remote_hook_states",
            "local_socket_name",
        ],
        remover: Remover::F7,
        why: "hook provisioning and the remote status poll name tmux's socket and options",
    },
    Transitional {
        from: "cli",
        to: "backend::tmux",
        items: &[
            "automation_heartbeat_running",
            "ensure_automation_heartbeat",
            "list_local_hook_states",
            "local_socket_name",
            "set_own_pane_state",
            "stop_automation_heartbeat",
        ],
        remover: Remover::F7,
        why: "session signal, the headless status poll, the heartbeat and the socket report",
    },
    Transitional {
        from: "coordinator",
        to: "backend::tmux",
        items: &["ensure_automation_heartbeat"],
        remover: Remover::F7,
        why: "the interface arms the heartbeat window on the local tmux server",
    },
];

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

/// The concrete adapters. Each is reached only through [`FACTORY`].
const ADAPTERS: &[&str] = &["backend::tmux"];

/// Only the composition roots may build the registry — `coordinator` here, and
/// the exempt crate roots (`main`, `bin/`) — and only the factory may name an
/// adapter. A consumer that builds its own registry sees a different set of
/// backends from the one the process was wired with, and one that names an
/// adapter has stopped using the contract. Either kind of crossing that exists
/// today is transitional, and a factory call outside the roots is F5a's to
/// remove.
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
    for entry in TRANSITIONAL.iter().filter(|t| t.to == FACTORY) {
        assert_eq!(
            entry.remover,
            Remover::F5a,
            "{} → {FACTORY} is removed by injecting the registry (F5a)",
            entry.from
        );
    }
}

/// The files that say where a session runs and what a backend is, for every
/// host and every multiplexer alike: the route grammar and the contract.
const NEUTRAL_FILES: &[&str] = &["session/route.rs", "backend/contract.rs"];

/// What a neutral file may not reach: how one kind of host is launched
/// (`shell`'s ssh/wsl launchers, a host's own `hosts.toml` entry and the
/// loader of it) or how one multiplexer is driven (the tmux adapter and its
/// command grammar).
const HOST_OR_MUX_SPECIFIC: &[&str] = &[
    "shell",
    "session::host_def",
    "agent::host_config",
    "backend::tmux",
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
        remover: Remover::F5a,
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
