## Context

The interface is a directory of Lua files the kernel reads at startup and on
change. Two files already describe that directory, and the split between them is
load-bearing: `.bundled.json` records what **delivery** did (per file: the digest
we last wrote, or a tombstone for one the user deleted), and `ui.json` records what
the **user decided** (rebound chords, plugins turned off, files trusted, plugin
settings). `bundled::materialize` reconciles the embedded set against the directory
with a six-way decision — write, settle, update, preserve, tombstone, leave — that
encodes "do not clobber my edits, remember what I deleted".

Acquisition has no equivalent. `docs/examples/` ships three runnable files whose
install instructions are three `cp` commands, and the only record afterwards is
`Source::User` — the same answer the inventory gives for a file the user wrote
themselves.

Extensions already solved the neighbouring problem. `extension_config::resolve_source`
turns a bare name into `{OFFICIAL_REPO_RAW}/{tag}/extensions/<name>`, pinned to the
running binary's release tag, and handles URLs and local paths including Windows
drive forms. `thurbox-cli extension` has install / uninstall / update / list /
available / activate / deactivate / status, and self-heals on startup and on the
headless tick.

Two constraints shape everything below.

**An agent is the primary operator.** A session on the interface directory already
iterates well: thurbox injects `THURBOX_CONFIG_DIR` into every session it spawns,
so `thurbox-cli plugin check` run inside that session resolves against the live
interface, and the watcher reloads on save with a 120 ms debounce. Every operation
here must therefore work with no TTY, take machine-readable input, and report
outcomes through the exit status.

**Providers and packages must stay out of the binary** (ADR-20). This change adds
no knowledge of any specific plugin — only of a shape and a resolver, both of which
already exist for extensions.

## Goals / Non-Goals

**Goals:**

- Composition of the interface is written down, hand-editable, and authoritative.
- A plugin can be acquired, pinned, advanced and removed without a terminal.
- The same spec plus lockfile reproduces the same interface on another machine.
- A pane installed from a source is distinguishable from one the user wrote,
  because a capability is granted per file and provenance is the question that
  should precede the grant.
- Trusting an installed plugin survives its legitimate updates without surviving a
  substitution of its contents.
- A pane that loaded but which nothing places is a reported failure with an
  instruction, not a silent blank.

**Non-Goals:**

- **Lazy loading.** The Neovim managers this borrows from make it the headline
  feature; here it optimises nothing. A disabled plugin is never read (`build` skips
  the file), an unplaced pane never renders, and the bundled set is nine files. A
  load scheduler would add machinery and a new class of "why is my pane missing".
- **Mutating `ui/layout.lua`.** Decided below (D6).
- **Sandboxing an installed plugin beyond what the VM already does.** The
  capability model is unchanged: the environment withholds `os`, `io`, `debug`,
  `package` and the loaders, and `run` is absent until the file is trusted. An
  installed plugin is exactly as constrained as one the user wrote.
- **A schema change.** Nothing here belongs in SQLite; see D1.
- **Publishing tooling.** How a third party builds a package is out of scope; the
  officially distributed set lives in this repository.

## Decisions

### D1 — The spec is TOML in the interface directory

`ui/plugins.toml`, hand-edited, listing one entry per plugin.

`docs/CONFIG.md` already states the rule this follows: hand-edited registries are
TOML, machine-written keybindings are JSON, and concurrently-written runtime state
lives in SQLite. Composition is a hand-edited registry. It also has to sit *in the
interface directory*, because that is where the agent stands.

*Alternatives considered.* A **Lua spec** reads naturally in a Lua directory and is
what lazy.nvim does — rejected because the CLI would need to boot a VM to read it,
and because a malformed edit becomes a runtime nil at frame time rather than a
parse error with a line number. **SQLite** — rejected twice over: an agent cannot
edit it with an ordinary edit, and CONFIG.md reserves it for state written
concurrently by several processes, which composition is not. **Extending
`ui.json`** — rejected because that file is user *decisions* and this is
*composition*; they have different lifecycles, and a lockfile written by a machine
does not belong in a file people hand-edit.

### D2 — Two files: `plugins.toml` hand-edited, `plugins.lock` machine-written

```toml
# ui/plugins.toml
[[plugin]]
src  = "atlas"                  # bare name, URL, or path
file = "plugins/75_atlas.lua"   # load order lives in the filename
pin  = "v0.3.1"                 # omit to take the newest at install time
```

The lock records, per entry, what the source resolved to and the digest of every
file delivered.

The split follows the precedent the repository already sets twice — `.bundled.json`
versus `ui.json` — and the one every package manager sets (`Cargo.toml` /
`Cargo.lock`). It is technically possible to keep one file: `settings.toml` is
already machine-written through a `toml_edit` `DocumentMut` that preserves its
comments, so the tooling exists. Rejected anyway: a merge conflict in the recorded
half would dirty the hand-edited half, and the two want opposite review treatment —
you read a spec diff and you skim a lock diff.

Both files are inventoried, so neither is a stray the Interface tab cannot account
for. `Kind` gains a `Manifest` case beside `Doc` for the same reason `Doc` was added
when the README shipped: `Kind::of` falls through to `Pane`, and a non-Lua file
reported as a pane that failed to load is a false alarm.

### D3 — The install/preserve decision is extracted, not duplicated

`materialize` decides between write / settle / update / preserve / tombstone /
leave from three inputs: what is on disk, what the manifest last recorded, and what
we are about to write. An installed plugin wants that decision unchanged — an edit
is the user's to keep, a deletion is remembered, an untouched file may be advanced.

Extract it as a pure function over those three inputs and have both delivery and
installation call it. Reimplementing the matrix for packages would be the second
copy of the only logic here that is genuinely subtle, and the first copy is the one
with tests.

*Alternative.* Parameterise `materialize` itself over a payload source (embedded
versus fetched). Rejected as the larger change: `materialize` also owns manifest
reading, directory creation and error collection, none of which the package path
wants to inherit wholesale.

### D4 — Provenance is a new `Source` case, not a parallel registry

`Source` becomes Bundled / Edited / User / Removed / **Installed { src }**.

The inventory is already the single answer to "where did this file come from", and
the Interface tab already renders that answer as a word in the row's tail
(`edited`, `yours`). This is a third word. A separate installed-files registry
would mean two answers to one question and a tab that has to reconcile them.

Note this is orthogonal to `State`: `state_of`'s rank ordering is about whether a
file is drawing, not where it came from, so no sort order changes.

### D5 — Trust is keyed to `(src, pin)` **and** the digest the lock recorded

Today `Trust::resolve(trusted_digest, current_digest)` returns Trusted when the two
match and Drifted when they do not. For a managed file that is the wrong question:
every legitimate update changes the contents, so a manager keyed on contents reports
every release as `trusted · modified` and trains the user to dismiss the warning.

The grant therefore records `src@pin` *plus* the digest the lockfile recorded for
that version, and a managed file is trusted when **both** agree:

| Situation | `src@pin` | digest vs lock | Result |
|---|---|---|---|
| granted, untouched | match | match | trusted |
| reinstalled, same version | match | match | trusted, silently |
| pin advanced | differs | — | not granted; ask again |
| user edited it locally | match | differs | installed · modified |
| upstream re-tagged the same pin | match | differs | installed · modified |

That last row is why the digest cannot be dropped. Keying on `(src, pin)` alone
would let a source replace the contents of a tag the user had already trusted and
keep the capability — a supply-chain hole opened by the very change meant to reduce
warning fatigue. Keeping both closes it, and costs nothing: the lock already has
the digest.

A first install still prompts. Capabilities-by-absence is unchanged: until the
grant exists, `run` is not in the plugin's environment.

### D6 — The manager does not touch `ui/layout.lua`

Adding a pane is two edits, the plugin and the slot. The manager makes the first
and diagnoses the second.

Three options existed. **Programmatic insertion** into a managed region — rejected:
`layout.lua` is the one file whose mistakes cannot break a plugin, and it is the
file whose correctness the whole arrangement rests on; rewriting it from a package
manager trades a clear instruction for a class of merge and formatting bugs.
**Slots from data**, so placement needs no Lua edit — rejected as gutting the second
rule of the kernel (layout resolves before render, *in a file you can edit*).
**Leave it to the operator and make the feedback loud** — chosen, and chosen
*because* the operator is an agent: editing Lua is what a coding agent is reliably
good at, and noticing that a pane loaded successfully while drawing nothing is what
it cannot do.

So the effort goes into D7 rather than into an editor for `layout.lua`.

### D7 — "Unplaced" is decided at a stated reference size

Placement is size-dependent: below `two_panel_min_cols` the arrangement returns the
centre alone, so *every* side pane is legitimately unplaced on a narrow terminal.
"Unplaced" must therefore mean unplaced at a size where it should have been placed.

`check` resolves the arrangement once at a generous reference size, and reports the
size it used. The comparison is already available: `LuaHost::occupied_slots()`
returns the slots loaded plugins claim (floats and decorators excluded, since
neither fills a rect), and `layout::resolve` returns the slots the arrangement
placed. A slot in the first set and not the second is the failure.

*Alternative.* Probe several widths and report the widest at which the pane is
missing. Rejected as noise: the actionable case is a slot no arrangement mentions at
any size, and that is visible at one size.

Floats are excluded because they occupy no slot by design, and disabled plugins
because nothing was asked to place them — both are in the spec as scenarios, since
both would otherwise be false positives in exactly the situation the check exists
to serve.

Implementation found a **third** exclusion of the same kind, and it is the one
that matters most: a pane behind a **closed column**. `search` starts closed
(`lib/panels.lua`'s `OPEN_AT_START`), so the bundled arrangement legitimately
names no `search` slot until something opens it — the interface we ship would fail
its own check. So the check opens every occupied slot's panel flag before
resolving. That is a `store` write from the kernel, which is less of an
intrusion than it looks: `shared_bool` already documents that these flags live at
`panels.<name>` *because* the arrangement must read them before any plugin runs,
and the kernel already reads them from outside Lua. This writes the same key.

Two smaller findings, both fixed rather than designed around. `plugin check` never
applied the registry's disabled set at all — it built the host with an empty one —
so a turned-off plugin was reported as loaded, and would have faulted the slot the
user correctly removed alongside it. And a `layout.lua` that fails to resolve was
previously not checked in any way; it is now the reported failure, since it is
both a real defect and the reason the unplaced verdict cannot be reached.

### D8 — `lib/<vendor>/…` needs no kernel change

`require` splits the module name on every dot and resolves under the interface root,
refusing `..` and absolute paths — so `require("lib.atlas.util")` already resolves
to `lib/atlas/util.lua`. Namespacing is therefore a packaging convention plus a
refusal: a package may deliver under `lib/<its own name>/` and nowhere else, so it
cannot replace `lib/theme.lua`.

The convention is enforced at install time rather than at load time, because the
loader deliberately knows nothing about packages.

### D9 — Bare names resolve to `ui-plugins/` in this repository

`{OFFICIAL_REPO_RAW}/{tag}/ui-plugins/<name>`, mirroring `extensions/`, and pinned
to the running binary's release tag by the same `official_ref()`.

Pinning to the tag is the property worth keeping: a pane reads `thurbox.*`, a
contract that moves, and resolving against the tag means the officially distributed
set always matches the binary asking for it. A separate `thurbox-panes` repository
was considered and rejected for losing exactly that, in exchange for release
independence this set does not need yet.

A package is a directory containing a `plugin.toml` (name, description, the files it
delivers, and a `requires_thurbox` range) plus its Lua. A single `.lua` URL is the
degenerate case with no manifest: one file, no libs, no declared compatibility.

## Risks / Trade-offs

- **Installing a pane is a supply-chain step, and a pane can ask for `run`** →
  Three things already stand between a package and the user's shell, and none is
  weakened here: the capability is absent until granted per file, the grant is
  prompted rather than implied, and D5 makes a content substitution visible even
  within a trusted version. What this change adds is the provenance needed to make
  that decision — D4 — which the user does not have today for a copied file.
- **Fetching happens in the CLI over the network, with no checksum from a third
  party** → Unchanged from the extension installer, which has the same posture; the
  lockfile improves on it by recording the digest actually installed, so a later
  divergence is detectable even where the source offers no integrity of its own.
  Worth stating plainly rather than implying packages are verified.
- **The lock can disagree with the spec** → Convergence reconciles toward the spec
  and reports what it changed; the spec is authoritative by D1. The failure mode to
  avoid is a silent reconciliation, so it is a reported outcome per entry.
- **`Source` gaining a case touches every match over it** → That is the point of it
  being an enum; the compiler enumerates the sites. The Interface tab, the CLI's
  JSON, and `sources()` are the three that render it.
- **A reference size in `check` is a judgement, and a pane could be placed at the
  reference size and unplaced at the user's** → Accepted, and mitigated by reporting
  the size used, so a surprising verdict is explicable rather than mysterious. The
  alternative is a multi-size report nobody reads.
- **Two more files in the interface directory** → Both inventoried (D2), so the tab
  accounts for them, and `Kind::Manifest` keeps them from being reported as broken
  panes. The cost is one more concept in a directory whose whole appeal is that it
  is legible.
- **An agent could install a pane and never place it** → This is the failure the
  change exists to make loud (D6/D7), so it is a risk only if `check` is not run.
  Mitigation: `install` prints the layout line at the moment of installing, so the
  instruction arrives before the check is thought of.

## Migration Plan

Nothing to migrate: no schema change, and an interface with no `plugins.toml` is a
valid interface with nothing installed (a spec scenario). Existing directories keep
working untouched, including ones where a pane was copied in by hand — those remain
`Source::User`, which stays truthful, since the manager did not put them there.

Adopting the manager for a hand-copied pane is `plugin install <src> --as <file>`
against the same destination, which by D3's decision refuses to overwrite an
unmanaged file — so adoption is deliberate rather than accidental.

Rollback is deleting `plugins.toml` and `plugins.lock`: the panes stay where they
are and become ordinary files again.

## Open Questions

- **Does `plugin sync` belong in the startup self-heal?** Extensions re-ensure
  themselves at TUI startup and on the headless tick. Doing the same for panes would
  make the interface converge on launch, but it would also mean a network fetch on a
  path that currently touches nothing outside the binary and the disk — and the
  startup path is the one place the interface must not become slow or fallible.
  Leaning no; worth deciding before implementing.
- **Should `requires_thurbox` be enforced at load, refusing a pane that declares a
  newer range than the running binary?** It turns a future `attempt to index a nil
  value` into a sentence, which is the same trade `check` makes elsewhere. The cost
  is a second place the kernel knows about versions.
- **Does the official set need `plugin available`, mirroring `extension
  available`?** The mechanism is identical and the list would be short at first.
