# Design

## Context

See `proposal.md` — Why. The mechanism this change alters is small and already
in place: `src/kernel/bundled.rs` holds the embedded interface as a
`&[(path, contents)]` table, writes it into the user's directory on every start,
and tells an edit from a stale copy with `.bundled.json`, a
`path → FNV-1a digest` map of what it last wrote.

That manifest already answers "did we write this, and has it changed since". It
does not answer "did the user delete it", because the loop only considers files
that exist: absent means write, unconditionally. Every requirement in
`specs/plugin-lifecycle/spec.md` about removal is that one missing branch, plus
the surface that makes the resulting state legible.

Two existing behaviours constrain the design:

- **Reload is all-or-nothing.** `LuaHost::build` fails on the first plugin that
  will not load, leaving the previous plugin set running with the error in
  `host.error`. Per-plugin load isolation is not on the table here.
- **The recovery floor triggers on `host.error`.** `build` currently errors when
  the plugin directory yields no `.lua` files, so "the user removed everything"
  and "the directory is broken" are today the same condition.

## Goals / Non-Goals

**Goals:**

- Deleting a shipped file is the removal, and it survives upgrades.
- Nothing the user wrote is ever written over or removed by delivery.
- Restoring a removed or edited bundled plugin is one action from inside the app.
- Which plugins exist, where they came from, and which are actually running is
  readable — by a plugin, from published data.

**Non-Goals:**

- Per-plugin load isolation (a bad file still costs the reload; D5).
- Any change to kernel-owned chrome, or to the plugin API surface plugins render
  with. This change adds one read (`thurbox.plugins`) and one command.
- Opening a file in `$EDITOR` from inside thurbox. The directory path is
  reported; the editor is the user's.
- A package manager. Installing a plugin is copying a file in, and this change
  is about making that reliable, not about fetching it for you.

## Decisions

### D1 — The manifest gains a third state, rather than a second file

`.bundled.json` becomes `path → { digest } | { removed }`. A file the manifest
knows about and that is **absent from disk** is a removal: record it, never
write it again. A file the manifest has **no entry for** is a first delivery:
write it.

That single distinction implements every removal scenario in the spec, and it
falls out of what the manifest already means:

| on disk | in manifest | action |
|---|---|---|
| absent | absent | **write** — first run, or a plugin new in this release |
| absent | digest or removed | **tombstone** — the user deleted it |
| present, matches bundled | any | skip |
| present, differs, digest matches | — | **update** — ours, untouched, superseded |
| present, differs, digest does not match | — | **preserve** — the user edited it |

Alternatives considered:

- **A `disabled = [...]` list in `settings.toml`.** Rejected: two sources of
  truth for one question, and it makes `rm` a lie — the file would still be on
  disk and still not running. It also puts interface composition in a file the
  interface does not otherwise use.
- **A separate `.removed` file.** Rejected as the same record split across two
  files that must agree.

Consequences worth stating, because they are the escape hatches:

- Deleting `.bundled.json` forgets every removal and re-delivers the whole
  interface. So does deleting the directory. This is the documented "give me the
  shipped interface back" move, and the spec requires it.
- The manifest stays hand-editable and predictable: dropping an entry
  re-delivers that file.
- Legacy manifests (a plain `path → "digest"` map) must still parse, so the
  entry type deserializes from either a bare string or an object.

### D2 — A shipped file that stops shipping is retired, on the same rules

A file the manifest records as ours, which the current binary no longer carries,
is deleted **if it is unmodified**, and preserved with a report if it is not.
Without this, renaming a bundled plugin between releases leaves the old copy on
disk and loading — the user gets two session lists from an upgrade they did
nothing to. v2's bundled set is actively changing (`v2-parity-gaps` re-adds
panes), so this is not hypothetical.

It is the same rule as D1 read from the other side: we clean up only what we
wrote and the user has not claimed.

### D3 — Restore and remove are commands, because Lua has no filesystem

The inventory pane is an ordinary plugin, so it cannot write files — a
capability it must not gain (`bundled-plugins`: bundled plugins hold no
privileged capability). Both verbs go through the existing command bus:

```lua
command("plugin", { action = "restore", file = "plugins/10_sessions.lua" })
command("plugin", { action = "remove",  file = "plugins/10_sessions.lua" })
```

`restore` covers both spec cases — a removed file and an edited one — because
both are "write the embedded copy and clear the record". Nothing else is needed
to make the change visible: writing the file changes its mtime, and the existing
`Watcher` reloads, so this reuses the reload path rather than adding one.

`remove` is not required by the spec — deleting the file is — but the view
already needs the plumbing for `restore`, and offering restore without remove in
a list of plugins is a strange half. It can be dropped without touching the
specs.

### D4 — The inventory is published state, not a kernel-drawn surface

`thurbox.plugins` joins the other snapshot reads: one entry per plugin with
`name`, `path`, `slot`, `source` (`bundled` / `edited` / `user` / `removed`),
`state`, and `error`. Removed plugins appear too — they have no loaded plugin
behind them, so they come from the manifest rather than from the host.

This keeps the surface a plugin, which the spec requires and which is also the
honest test of the API: a pane that lists panes should need nothing special. It
is the opposite choice from `v2-chrome-bands` D1 (bands are kernel-drawn), and
for the opposite reason — a band reports the application's own state and must
never depend on plugin code, whereas this is a view of the user's own files that
they should be able to restyle or replace.

### D5 — "Not running" has three states, reported honestly

The kernel knows, after each frame, which slots the arrangement placed. That
gives three distinguishable states without guessing:

- **visible** — placed, and (in a switch slot) the active occupant;
- **not shown** — its slot is placed but another occupant holds it, or the
  column is closed. Normal, not a fault.
- **slot not placed** — the slot appears nowhere in the arrangement at the
  current size. This is the silent-drop case from `layout.rs:66`, and the one
  worth a diagnostic.

**failed** is a fourth state and comes from `host.error`, which names the file
in its message (`load_plugin` prefixes the file stem). Since a failed reload
leaves the previous set running — per-plugin isolation is a non-goal — the
report is against the whole environment, attributed to that file. That is enough
to stop the running interface being mistaken for what is on disk.

Placement is per-frame and size-dependent, so the report is "as of the last
frame". Wording it "not shown" rather than "broken" is why that is acceptable.

### D6 — An empty plugin set is valid

`build` stops erroring on an empty directory. Otherwise removing the last plugin
trips `host.error`, the recovery floor fires, and the bundled interface returns
— the system silently undoing the removal it was just asked to make, which is
exactly the spec's "an empty interface is a choice, not a fault".

With chrome kernel-owned (help, settings, theme picker, bands), an interface
with zero plugins is still navigable and still repairable from inside, so this
costs nothing in recoverability. The reserved keys never belonged to plugins.

### D7 — The cwd checkout keeps winning, and says so

`resolve_ui_dir()` still prefers `THURBOX_UI_DIR`, then `./ui`, then the user's
copy: this repo develops v2 by running from the checkout, and taking that away
to fix a shadowing surprise would be the wrong trade. The fix is visibility —
the resolved directory is published alongside the inventory, so a user whose
edits "did nothing" can see which directory is actually loaded.

## Risks / Trade-offs

- **A user deletes a bundled plugin by accident and cannot find it** → the
  inventory lists removed plugins with a restore action, and deleting
  `.bundled.json` (or the directory) re-delivers everything. Both documented in
  `docs/PLUGINS.md`.
- **The manifest is lost or corrupted, and removals come back** → this is the
  same behaviour as a fresh install, which is the least surprising failure mode
  available; the alternative (a removal record that outlives the directory it
  describes) is worse.
- **A user edits a bundled file and then wants the shipped one, having lost
  theirs** → `restore` overwrites without a copy. Reversible only from their own
  version control. Accepted: the shipped copy is always recoverable, the user's
  is their own; a backup file would be a third state in a directory whose whole
  point is to be legible.
- **`thurbox.plugins` grows the snapshot** → one small vector rebuilt on reload,
  not per frame.
- **`remove` from inside the app deletes a file the user may have edited** → it
  is a confirmed destructive action or it is not offered. There turned out to be
  no shared confirmation surface to reuse — `60_confirm.lua` was cut with the
  other panes — so the confirmation is two presses in the pane itself, cancelled
  by moving off the row.

## Findings from implementing

- **A decorator could not be added by a file drop at all.** `load_plugin`
  required a `render` of every plugin, and a decorator that declared no `slot`
  took the default one — so the documented decorator example
  (`docs/PLUGINS.md`) failed to load, and adding a stub `render` to satisfy it
  made the decorator an occupant of `center`, competing with the pane it
  exists to decorate. Both fixed here: `render` is required unless the plugin
  decorates, and a decorator is never a slot occupant.
- **The existing "the difference is surfaced" requirement was not met.**
  `bundled-plugins` already required an upgrade that preserves a user's edit to
  say so; the `Report` was built and then discarded by `resolve_ui_dir`. It now
  becomes a startup notice, alongside the retirements from D2.
- **The footer could not take another entry.** `render_action` gives the entries
  the full width and the left cluster what remains, so a fourth pill pushed the
  focus badge out at 60 columns. The inventory pane therefore declares a key and
  no pill — the call `20_agent.lua` already documents for its shell tab.

## Migration Plan

1. `bundled.rs::a_deleted_file_comes_back` is replaced by its opposite
   (`a_deleted_file_stays_deleted`) plus the redelivery-after-directory-loss
   case. This is the **BREAKING** item in the proposal, v2-internal only —
   `thurbox2` is not the shipped binary and v1 is untouched.
2. Existing user directories need no migration: a legacy manifest parses, and a
   file the user deleted before this change is tombstoned on the next start
   instead of returning. That is the intended behaviour arriving late, not a
   regression.
3. Rollback is reverting the crate: an older binary reads the new manifest,
   ignores the entries it does not understand, and restores the removed files —
   i.e. it degrades to today's behaviour rather than failing.
4. `docs/PLUGINS.md` gains the lifecycle section; `CLAUDE.md`'s v2 section names
   the inventory pane in the bundled set.
