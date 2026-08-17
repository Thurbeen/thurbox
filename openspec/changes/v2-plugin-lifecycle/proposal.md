## Why

v2's premise is that the interface is yours: every pane is a Lua file, and
`bundled-plugins` already makes the shipped ones readable and editable on disk,
preserved across upgrades, with the embedded copies as a recovery floor. What it
does not make them is **removable**.

`bundled::materialize` rewrites any bundled file that is absent, on every launch
— and `bundled.rs::a_deleted_file_comes_back` asserts that as intended
behaviour. So a user who does not want the session list cannot delete it, and a
user who writes `my_sessions.lua` to replace it gets *two* session lists, because
the original returns on the next start. "Replaceable by the user on the same
terms as any other plugin" (`bundled-plugins`) is true only while the replacement
keeps the bundled filename.

The rest of the lifecycle is undocumented rather than broken, which is its own
cost: adding a pane, adding a `lib/` module, or editing the arrangement all work
today, but no requirement says so and no test holds them, so they can regress
silently. And three of the ways they fail are invisible:

- a plugin whose `slot` the arrangement never places is dropped without a word
  (`src/kernel/layout.rs:66` — "a slot named nowhere");
- a plugin that fails to load leaves the last good version running, so the
  interface looks fine while the file on disk is not what is running;
- `resolve_ui_dir()` prefers a `./ui` directory in the working directory, so
  launching `thurbox2` from any repo that happens to have one silently shadows
  the user's real interface.

There is nowhere to look for any of this: the kernel knows every plugin's path,
slot, source and load state, and shows none of it.

## What Changes

- **Deleting a bundled file is how you remove it.** `materialize` records a
  tombstone in `.bundled.json` for a file it wrote and later finds gone, and
  never writes it again. **BREAKING** (v2-internal): reverses
  `a_deleted_file_comes_back`.
- A **removed bundled plugin can be brought back** — the embedded copy is still
  in the binary, so restoring it is clearing the tombstone and writing the file.
- A **plugins view** lists every plugin with its file path, slot, source
  (`bundled`, `bundled, edited`, `yours`, `removed`) and load state
  (`loaded`, `failed`, `not placed`). It is where reset lives: restore a removed
  plugin, or discard edits to a bundled one and take the shipped version back.
  It is a plugin itself, bundled like the rest — and therefore removable.
- The **whole lifecycle becomes specified and tested**, not just the parts being
  built: adding a pane, adding a `lib/` module, replacing a bundled plugin under
  a *different* filename, editing `layout.lua`, and what happens to each on
  upgrade.
- A **user-written file is never touched** by materialization — stated as a
  requirement rather than left as a property of the current loop.
- The **`./ui` shadow becomes deliberate**: a cwd checkout is still preferred
  (it is how this repo develops v2), but the interface reports which directory it
  is running from, so shadowing is visible rather than silent.
- **Out of scope, deliberately**: kernel-owned chrome. Help, settings, the theme
  picker and the header/status/footer bands stay kernel-owned per
  `v2-system-modals` D1 and `v2-chrome-bands`; this change governs pane plugins,
  decorators, float/modal plugins, `lib/*.lua` and `layout.lua`. Also excluded:
  opening a plugin in `$EDITOR` from inside thurbox.

## Capabilities

### New Capabilities

- `plugin-lifecycle`: how a user adds, removes, replaces and edits each kind of
  plugin, and what the system owes them in return — where the interface is read
  from, what materialization may and may not write, how a removal persists
  across upgrades, how it is undone, and how a plugin that is present but not
  running is made visible.

### Modified Capabilities

None. `bundled-plugins` is still change-local — `openspec/specs/` holds nothing
yet, so its requirements live in `v2-plugin-kernel` rather than in the main
specs. `plugin-lifecycle` states the delivery requirements it needs to change
(removal, tombstones, user-file safety) and `design.md` records which
`bundled-plugins` requirements it supersedes, so the two are reconciled when
either is archived.

## Impact

- **Kernel**: `src/kernel/bundled.rs` gains tombstones, a restore path, and a
  per-file source classification; `Report` grows the shape the plugins view
  reads. `src/kernel/host.rs` retains why each plugin is not running (load
  error, unplaced slot) instead of discarding it.
- **Snapshot**: a new read-only `thurbox.plugins` list, so the view is a plugin
  drawing published data like any other — no privileged capability
  (`bundled-plugins`: "Bundled plugins hold no privileged capability").
- **Commands**: `remove` and `reset` for a plugin file, since Lua cannot touch
  the filesystem and must not start.
- **Bundled interface**: one new plugin file, plus its slot — it occupies the
  existing `center` switch slot, so `ui/layout.lua` is unchanged.
- **Tests**: `bundled.rs::a_deleted_file_comes_back` is replaced by its
  opposite; `tests/v2_parity.rs` is untouched (this is not a v1 surface —
  v1 has no editable interface to be at parity with).
- **Docs**: `docs/PLUGINS.md` gains the lifecycle it currently only implies;
  `CLAUDE.md`'s v2 section names the plugins view.
