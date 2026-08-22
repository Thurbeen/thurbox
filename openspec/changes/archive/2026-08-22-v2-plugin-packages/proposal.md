## Why

Extensions have a package manager — a manifest, an official source pinned to the
binary's release tag, install/update/uninstall, self-heal, staleness. Panes have
`cp docs/examples/tasks.lua ~/.config/thurbox/ui/plugins/80_tasks.lua`. That
asymmetry is now the thing standing between "every pane is a file you can edit"
and "every pane is a file you can *get*", and the interface has just started
shipping runnable examples that say "copy these three files" in prose.

The forcing case is an agent. A session on the interface directory already works —
`THURBOX_CONFIG_DIR` is injected into every session thurbox spawns, so
`thurbox-cli plugin check` run *inside* that session resolves against the live
interface, and the watcher reloads on save. What that agent cannot do is acquire a
pane, know where one came from, or discover that the pane it just installed is
loaded but drawing nothing. Iteration works; acquisition and feedback do not.

## What Changes

- **A declarative spec, `ui/plugins.toml`.** Lists what the interface is made of:
  a source, a destination file, an optional pin. Hand-edited by a person or an
  agent; TOML because `docs/CONFIG.md`'s own rule is that hand-edited registries
  are TOML, and because a bad edit becomes a parse error with a line number
  instead of a nil three frames later.
- **`thurbox-cli plugin install | sync | update | remove`.** Headless, no TTY.
  `sync` converges the directory to the spec, which reduces the agent's job to
  *edit one file, run one command, read the exit status*.
- **A lockfile, `ui/plugins.lock`.** Records what each entry resolved to, so the
  same spec produces the same interface on another machine.
- **Bare names resolve to `ui-plugins/` in the thurbox repository**, pinned to the
  binary's release tag, exactly as extension names resolve to `extensions/`. URLs
  and local paths work as they do for extensions.
- **`plugin check` gains a failure mode: loaded but unplaced.** A pane whose slot
  no arrangement places loads cleanly and draws nothing — today's only signal is
  the word `no slot` in a listing nobody reads in CI. It becomes a non-zero exit
  that prints the line to add to `ui/layout.lua`.
- **The manager does not edit `ui/layout.lua`.** Arrangement stays hand-authored.
  Editing Lua is what an agent is good at; noticing that a pane silently is not
  drawing is what it cannot do, so the effort goes into the diagnosis.
- **Provenance for installed panes.** The inventory's origin gains an installed
  case carrying the source, so the Interface tab stops reporting a third-party
  pane as indistinguishable from one you wrote. This matters because `run` trust
  is granted per file, and "who shipped this" is the question to answer *before*
  granting it.
- **Trust for a managed pane is keyed to `(source, pin)`, not to the file's
  digest.** Trust already survives an update in the sense that the capability
  keeps working, but the row reads `trusted · modified` — so a package manager
  would make every release look like tampering. "I trust atlas v0.3.1" is a
  sentence a person can mean; "I trust this digest" is not. A first install still
  prompts, and a pin change prompts again.
- **`lib/` gains namespacing for third-party modules.** `lib/fuzzy.lua` is a
  namespace with one tenant; a second publisher shipping a helper has nowhere to
  put it.
- **Deliberately not included: lazy loading.** It is the headline feature of the
  Neovim managers this borrows from, and it solves a problem thurbox does not
  have — a disabled plugin is never read, an unplaced pane never renders, and the
  bundled set is nine files. Porting a load scheduler would add machinery with no
  measurable gain and a new class of "why is my pane not there".

## Capabilities

### New Capabilities

- `plugin-packages`: acquiring, pinning, converging and removing interface
  plugins from a declarative spec; the lockfile; how a source resolves; what
  trust means for a file the user did not write.

### Modified Capabilities

- `plugin-authoring`: two requirements change. Verification without a terminal
  must additionally fail on a pane that loaded but which no arrangement places,
  and say what to add. Listing without a terminal must additionally report an
  installed pane's source, which its current four origins (shipped, edited, the
  user's own, removed) cannot express.

## Impact

- **`src/cli/plugins.rs`** — four new subcommands; `check` gains the unplaced
  diagnosis; `list` reports the new origin.
- **`src/kernel/bundled.rs`** — `Source` gains an installed case. `materialize`'s
  preserve / tombstone / digest semantics are reused rather than reimplemented:
  "do not clobber my edits, remember what I deleted" is already written and
  tested, and an installed file wants the same treatment.
- **`src/agent/extension_config.rs`** — `resolve_source` and the official-base
  helpers are reused for panes. One resolver, two payload kinds; no second
  fetcher.
- **`src/kernel/registry.rs`** — trust records gain the `(source, pin)` key for
  managed files. `ui.json` stays the file of user decisions.
- **`src/kernel/modals/interface.rs`** — the Interface tab shows the new origin.
- **New files in the interface directory** — `plugins.toml` and `plugins.lock`
  join `.bundled.json` (delivery) and `ui.json` (decisions). Both are inventoried,
  so neither is a stray the tab cannot account for.
- **Docs** — `docs/PLUGINS.md`, `ui/README.md` (the copy shipped beside the panes,
  which currently teaches `cp`), `docs/CONFIG.md`'s file table, and the website's
  configuration page.
- **No schema change.** Nothing here touches SQLite: the spec and the lockfile are
  hand-edited and machine-written files in the interface directory, which is where
  the rest of the interface's own state already lives.
