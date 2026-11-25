# Architecture rules

`crates/architecture.toml` states the workspace's architecture. The
`arch-rules` crate checks the source against it in its `architecture` test.
This page is the reference for the keys of that file and for what the checker
can and cannot see.

## What the checker reads

- **Manifests.** The checker reads every `[dependencies]`,
  `[build-dependencies]` and `[dev-dependencies]` table, target-specific ones
  included. It resolves `package =` renames. The declared graph and the actual
  graph must both be acyclic. Every actual edge must be declared.
- **Sources.** The checker parses every `src/**/*.rs` file with `syn`. It
  resolves each path the way the compiler starts to: through `use` aliases and
  renames, `extern crate … as …`, `crate::`, `self::` and `super::`, glob
  imports, and `pub use` re-exports inside the crate and across workspace
  crates, to a fixpoint. The checker judges the resolved path. So
  `use std::fs as f; f::read(..)` is caught, and so is a `pub use` chain that
  reaches a forbidden crate.

## What the checker cannot see

- The checker does not expand macros. It resolves a macro's own path. It judges
  macro arguments that parse as comma-separated expressions (`format!`,
  `vec![a, b]`, `assert!`). It does not judge other macro input.
- Method calls have no types. The checker matches a forbidden method by name on
  every receiver, and as a path whose segment before the method is a type or a
  trait (`Store::update_status(..)`). It does not catch the qualified-self form
  `<FileStore>::update_status(..)`.
- Glob imports resolve conservatively. A name that a forbidden glob could bring
  is judged as if the glob brought it. The exception is a name read as a module
  (a path's prefix, a glob's base) when the module declares a `mod` of that
  name: that `mod` shadows every glob.
- The checker does not know which namespaces an import fills. It judges a macro
  call through any import of its name, and also as the macro that a glob or the
  prelude brings. A macro imported under a module, type or value name hides the
  glob meaning of that name.
- The checker ignores `cfg`. It judges code behind any `cfg`. A module behind a
  `cfg` shadows no glob. An import behind a `cfg`, like an import in a function
  body, adds a meaning to its name and removes none.
- Each target (the library, `main.rs`, each `src/bin/` binary) is its own module
  tree, found through `mod` declarations, `#[path]` included. A file that two
  targets declare is a module of each. A file that no target declares is placed
  by its location.
- The source rules do not apply to `tests/`, `benches/` and examples. They do
  apply to `#[cfg(test)]` modules inside `src/`. The manifest rules apply to all
  targets.

The compiler and clippy cover part of the same ground at build time. An
undeclared crate does not compile, and the `compile_fail` probes in each pure
crate's `lib.rs` prove it for each forbidden edge. Each pure crate's
`clippy.toml` disallows the I/O types and functions. The checker proves that the
rules hold across re-exports, and the negative fixtures exercise it.

## Top-level keys

| Key | Meaning |
| --- | --- |
| `crates.<name>` | The rule for one workspace crate. Every member has one. |
| `pure.forbidden_paths` | Path prefixes that a pure crate cannot reach, for example `std::fs`. |
| `pure.clippy_guards` | The types and functions that each pure crate's `clippy.toml` must disallow. |
| `status_writers.<field manager>` | The crates whose sources can name that status-writing field manager. |
| `restricted` | Paths that the crates a rule applies to cannot reach, except from the crates it allows. |

Each `restricted` entry has these keys:

| Key | Meaning |
| --- | --- |
| `applies_to` | The roles and crates that the rule binds, by name. Required. Each name must be a role or a crate of the file, because a scope that names nothing binds nothing. |
| `path` | The path the rule restricts. |
| `allowed` | The crates that can reach the path anyway. |
| `why` | The reason, in one sentence. |

## Crate keys

| Key | Meaning |
| --- | --- |
| `role` | `pure`, `adapter`, `orchestration`, `edge`, `tooling` or `component`. |
| `deps` | The workspace crates the crate can depend on, as normal or build dependencies. |
| `dev_deps` | The workspace crates the crate can have as dev-dependencies only. |
| `external` | For a pure crate: the external crates it can use. |
| `forbidden_methods` | The methods the crate's sources cannot call, by name. |
| `probes` | The workspace crates that the `compile_fail` probes in the crate's `lib.rs` must name. |
| `modules.<path>` | One module's rule. A crate without module rules is held to the crate graph only. |
| `governed` | Modules in which every file module, at any depth, needs a rule of its own. A file added there is then a decision, not part of its parent. |
| `exempt` | Target roots without a module rule: `lib`, `main`, or a binary's name. |
| `restricted` | Paths of the crate that only some of its modules can reach. |

A `component` is a component of the repository that is not Fleet's: thurbox or a
runtime. The checker holds it to the crate graph from its manifest and its
sources. No rule written for Fleet's roles binds it, because components meet
only through processes.

## Module keys

A module rule lists the modules that the module can reference, in any form. A
grant names exactly one module and never its children.

| Key | Meaning |
| --- | --- |
| `uses` | The modules it can reference. |
| `path_only` | The modules it can reach through a fully-qualified path, but never import. Each crossing stays visible at its site. |
| `external` | When present, the external crates the module can name itself. `std`, `core` and `alloc` are always allowed. |

A crate-level `restricted` entry names paths of the crate that only the modules
in `allowed` can reach. The checker resolves those paths like any other
reference.

| Key | Meaning |
| --- | --- |
| `paths` | The restricted paths. |
| `allowed` | The modules that can reach them. |
| `applies_to` | The module paths that the rule binds, each with every module under it. Empty means every module. |
| `allowed_files` | Files, relative to `src/`, that the rule leaves free. |
| `direct` | Judge what a module names itself: a path through the crate's own module that re-exports another crate's item belongs to that module. |
| `production_only` | Judge production code only, so a test can name what it pins. |
| `exceptions` | Files, relative to `src/`, each allowed exactly `sites` reaches, with a `why`. |
| `why` | The reason, in one sentence. |
