# v2-plugin-switching

## Why

The interface can add a plugin and remove one. It cannot turn one **off** — and
that gap is not a missing convenience, it is a way to lose work.

`bundled::remove` deletes the file. For a plugin thurbox ships, that is safe: the
removal is recorded and `restore` writes the embedded copy back. For a plugin
**you wrote**, there is no tombstone and no embedded copy — `restore` refuses
anything not in `BUNDLED` — so the file is simply gone. Nothing warns you: the
confirmation reads *"remove X? press d again to confirm"* whether the file can be
restored from the binary or has just been destroyed.

So the one key that reads like "I do not want this pane right now" is, for the
plugins a user cares most about, permanent deletion. Anybody trying to
*deactivate* their own plugin — to see the interface without it, to bisect a
problem, to quiet a pane for an afternoon — loses it.

The absent state is the fix. A plugin that is present, intact, and not loaded is
a state the interface has no way to express, and it is the state people actually
want most of the time.

## What Changes

- **A plugin can be disabled and re-enabled.** The file stays exactly where it
  is; the interface does not load it. Reversible with the same key that turned
  it off, taking effect on the next frame rather than at the next launch.
- **Disabling is the prominent action; deleting is the deliberate one.** The
  Interface tab offers both, but the reversible one is what the obvious key
  does, and the destructive one is not reachable by the same reflex.
- **A destructive removal says so before it happens.** Removing a file thurbox
  ships and removing one you wrote are different acts with different
  consequences, and the confirmation must name which one is about to occur. The
  asymmetry is already known — `restore` reports *"X is yours; thurbox ships no
  version of it"* — but only after the file is gone.
- **Disabled is a state in the inventory**, beside `removed`, `no slot` and
  `on screen`, so "why is this pane not here" has one place to be answered and
  every reason reads the same way.
- **Disabled plugins are inert, not half-loaded.** A disabled plugin declares no
  keys, contributes no settings, occupies no slot and takes no capability — the
  same as if the file were not there, which is the point of the state.
- **Adding a plugin is discoverable from inside the interface.** Dropping a file
  into the directory already works; nothing on screen says so, so it is a
  feature of the guide rather than of the application. The surface that lists
  the interface's files is where that belongs.

## Capabilities

### New Capabilities

- `plugin-switching`: turning an interface file off and on again without
  deleting it, and telling a reversible removal apart from a permanent one.

### Modified Capabilities

None. The requirements this change adds sit beside the existing
`plugin-lifecycle` ones rather than replacing them: adding, removing and
restoring keep their current contracts, and disabling is a third thing a file can
have done to it. See Impact for why the delta is written as a new capability
rather than as changes to that one.

## Impact

- **Archive ordering.** `plugin-lifecycle` is defined by the complete but
  **unarchived** change `v2-plugin-lifecycle`, so `openspec/specs/plugin-lifecycle/`
  does not exist yet. A `MODIFIED` delta against it would have nothing to merge
  into. This change therefore adds a capability of its own and depends on
  nothing being archived first; the two can be archived in either order.
- **New**: a persisted disabled set (alongside trust, in the interface's own
  `ui.json`), a `disabled` state on `kernel::inventory::Row`, and a toggle in the
  settings modal's Interface tab.
- **Changed**: `src/kernel/host.rs` (skip a disabled file when building the VM,
  so it declares nothing and occupies nothing), `src/kernel/inventory.rs`,
  `src/kernel/modals/interface.rs` (the toggle, the key layout, and the
  confirmation wording), `src/kernel/registry.rs` (persistence),
  `src/cli/plugins.rs` (`plugin list` reports it, like every other state).
- **Unchanged on purpose**: `bundled::remove` and `bundled::restore` keep their
  contracts. Disabling is not a kind of removal and must not be recorded in the
  delivery manifest — a disabled bundled file is still one delivery may update.
- **Not in scope**: enabling or disabling a plugin from outside the running
  interface (a CLI verb), and any notion of plugin *packages* or dependencies. A
  file is still the unit.
