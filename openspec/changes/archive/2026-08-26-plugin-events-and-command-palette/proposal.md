# plugin-events-and-command-palette

## Why

A v2 plugin can only learn that the world changed by being rendered: there is
no way to be told that a session was created, went `blocked`, finished a turn,
or that focus moved — so "when any session needs me, bring it forward", the
most thurbox-specific customisation there is, cannot be written as a pane. And
an action a plugin declares is reachable only by its chord, in a `Ctrl+<letter>`
namespace that readline, the agents and v1's muscle memory have already spent
(`tests/v2_keymap.rs` holds nine chords unbound on purpose); the help modal can
*list* an action but not run it. `hooks.toml` (session lifecycle hooks, in the
merge queue) answers the first gap for shell scripts and not for the interface:
nothing in the kernel or in Lua knows those hooks exist.

## What Changes

- **Plugins can react to events.** A plugin declares `on_event(name, payload)`
  and the kernel calls it — off the render path, under the same instruction
  budget as `render` — when something it already tracks changes: a session row
  appears, disappears, changes status, name or branch; the focused session or
  pane changes; a command the interface issued reaches a terminal phase; the
  interface reloads. The kernel derives every event from the change signals it
  already has (`SnapshotStore::version`, the command bus's phase transitions,
  the focus ring) — a plugin gets a push where it used to diff the snapshot
  itself, and gets it once per change rather than once per frame.
- **Plugins can emit events to each other.** `command("emit", { text = name,
  … })` publishes a user event (`user.<name>`) that every subscribed plugin
  receives next iteration — Neovim's `User` autocmd, and a real bus where
  `store` was a shared table nobody was told about.
- **The eight lifecycle hooks are also events.** `session.pre_*` cannot be —
  a plugin cannot veto, since it cannot answer — but each `session.post_*`
  the TUI's own operations fire is delivered to Lua as the same-named event,
  so shell and interface share one vocabulary. Operations another process
  performed (a `thurbox-cli` create, a cron tick) still arrive, as the
  snapshot-derived `session.created`/`session.deleted`; the two are
  distinguishable by name and a plugin subscribes to whichever it means.
- **A kernel-owned command palette.** `Ctrl+P` opens a modal listing every
  action in the registry — every plugin's declared keys, the kernel's own
  modals and reload, and a new declaration, `commands = { { id, desc } }`,
  for actions a plugin wants reachable **without** spending a chord — filtered
  by fuzzy match on description and id with the chord shown beside each,
  `Enter` runs the one selected. It is chrome, like help and settings:
  plugins contribute rows, the kernel renders and dispatches, and an action
  runs through the same `on_action` path a key press takes, so a plugin cannot
  tell the two apart.
- **Events are declared, so they are listable.** The set of kernel event names
  is a fixed enumeration the help modal shows on a page of its own, and
  `thurbox-cli plugin check` rejects a subscription to a name the kernel does
  not emit — a typo'd event is a lint failure, not a handler that never fires.
- **`Ctrl+P` leaves the awaiting list.** It is held there for v1's automations
  pane. This change reassigns it deliberately, recorded in the test rather
  than by silent reuse: a palette is the way *into* any pane that comes back,
  including that one.

Not in scope: timers, `init.lua`, key sequences, band slots, persistent
`state`. Each is its own change; the events bus is what they would build on.

## Capabilities

### New Capabilities

- `plugin-events`: what a plugin can subscribe to, what it is handed, when it
  runs, how it emits, and the bounds — a handler that throws or overruns costs
  its subscription for that event, never the frame or another plugin.
- `command-palette`: the palette modal — what it lists, how it filters, how it
  dispatches, what a plugin declares to appear in it, and what it must never do
  (run an action for a plugin that has no focus-independent handler, outrank a
  reserved chord).

### Modified Capabilities

- `frame-cost`: an event dispatch that writes nothing marks nothing dirty; a
  `pure` pane is invalidated by an event handler only through the state and
  store writes it already keys on. Handlers run at most once per changed
  signal, never per frame.

## Impact

- `src/kernel/host/` — a fifth plugin entry point (`on_event`), the `commands`
  declaration, `emit` in the command vocabulary, event-name validation at load.
- `src/kernel/` — a new `events` module deriving the kernel events from the
  snapshot, the command bus and focus; `registry` gains commands beside
  bindings; `modals/palette.rs` beside `help.rs`.
- `src/coordinator/` — one dispatch point per iteration, after workers and the
  command bus have published and before input is dispatched.
- `thurbox.yml`, `.luarc.json`, `tests/kernel_mvp.rs` (the environment
  enumeration), `tests/v2_keymap.rs` (`Ctrl+P` moves lists), new
  `tests/v2_events.rs` and `tests/v2_palette.rs`; `docs/PLUGINS.md`,
  `docs/V2-KERNEL.md`, `CLAUDE.md`, `ui/README.md`, the `ui-skill` `SKILL.md`.
- `session_ops::lifecycle_hooks` (from `add-hook-mechanism`): the TUI's own
  post-hook results are also queued as events. No behaviour change for the
  shell hooks.
- No schema change, no new config file, no new capability grant: events read
  what the snapshot already publishes, and the palette runs what keys already
  run.
