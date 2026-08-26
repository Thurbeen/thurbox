## Context

See `proposal.md` — Why. The constraints that shape the approach:

- The loop is demand-driven (`docs/PERFORMANCE.md`): `republish` runs once per
  painted frame and once per input batch, every published group is gated on a
  change signal, and a `pure` pane's tree is reused until `state`/`store` or a
  published source moves. Anything added to an iteration must settle to zero
  when nothing happens.
- One iteration is `poll_reload → apply_commands → poll_command_bus →
  sync_terminals_and_agents → serve_worker_stores → apply_external_requests →
  paint_if_due → drain_input` (`src/coordinator/mod.rs`). The command bus
  reports finished commands (`report_finished_commands`) and triggers a snapshot
  refresh; the snapshot's `version` moves only when a row actually changed.
- A plugin has five entry points today (`render`, `on_key`, `on_click`,
  `on_action`, `decorate`), each armed with `Budget` and each failing into a
  `PluginError { plugin, phase }` that costs only that plugin.
- The registry holds `Binding`s and `Setting`s collected from declaration
  tables at load; the help and settings modals render it; `Registry::resolve`
  turns a press into `(plugin index, action)` and `coordinator::input` hands
  that to `host.on_action`. Kernel chords live in `modals::bindings()`.
- `kernel` may not `use agent`; `session_ops` runs on workers. The lifecycle
  hooks from `add-hook-mechanism` run *inside* `session_ops` on the worker and
  are invisible to the kernel by design.
- `ctrl+p` is asserted unbound in `tests/v2_keymap.rs::CHORDS_AWAITING_THEIR_PANE`
  for v1's automations pane; every other `Ctrl+<letter>` a palette could
  plausibly take is spent, and every F-key but `f11` (fullscreen in most
  emulators) is bound or awaiting.

## Goals / Non-Goals

**Goals:**

- Events are **derived from what plugins can already read**, so a session made
  by any process looks the same, and no mutation site anywhere has to remember
  to fire one.
- One dispatch point per iteration, gated on the same signals `republish` is,
  so an idle interface runs no handler and the settle test still passes.
- The palette **reuses the registry and the `on_action` path** whole: it adds
  a declaration (`commands`) and a modal, not a second dispatch mechanism.

**Non-Goals:**

- Vetoing (`pre_*`) from Lua — a handler cannot answer, so it cannot refuse.
- Events for agent *output* (`output_generation`) — a per-line event would
  defeat the output frame floor; a pane that wants output reads the surface.
- Timers, `init.lua`, key sequences, band slots, persistent `state` — each its
  own change.
- Palette entries for the agent's own commands or for `thurbox-cli` verbs; it
  lists actions the *interface* can run.

## Decisions

**D1 — Events are derived by diffing published state, not raised by mutators.**
`kernel::events::Deriver` keeps, per session id, the fields events are defined
over (`status`, `name`, `branch`, `repos`, `parent`) and re-diffs only when
`SnapshotStore::version` moved (plus the per-tick quiescence override, which
changes a published status without a version bump — the deriver reads the
post-override rows, which is what the pane reads too). *Alternative*: firing
from `session_ops`. Rejected: it misses every other process (`thurbox-cli`,
the cron tick, a second TUI), it puts a kernel concern inside the side-effect
layer the kernel may not `use`, and it is the model that leaves a mutation
site forgotten. The first snapshot after a load **seeds** the deriver silently
— there is no `session.created` burst for rows that existed before the plugin
did.

**D2 — `session.post_*` comes from the command bus, not from the shell hooks.**
When a `Create`/`Fork`/`Delete`/`Restore`/`Restart` command finishes without
error, the coordinator queues the matching `session.post_*` with the facts
from the finished `InFlight` plus the row. The shell hook already ran inside
the operation on the worker, before the command could finish, so the ordering
"shell post-hook, then Lua post-event" holds without the kernel knowing hooks
exist. Names are shared with `hooks.toml` so a user learns one vocabulary; the
snapshot-derived `session.created`/`deleted` remain the events that fire for
every process.

**D3 — One dispatch point, after the stores and before the paint.**
`dispatch_events()` runs after `apply_external_requests()` and before
`paint_if_due()`. Handlers therefore see the iteration's fresh state and their
`state`/`store` writes land in the frame about to be painted, with no extra
frame. Before calling any handler it runs the same `republish` an input batch
does (tables current, once per batch, never per event). It is a no-op — a
`VecDeque::is_empty` check — on every iteration with nothing queued, which is
what keeps the settle test true. *Alternative*: dispatching inside `render`
(a `ctx.events` list). Rejected: it ties delivery to whether the pane is drawn
this frame, and a switch-slot alternate would miss every event while hidden.

**D4 — Delivery order and the cascade bound.** Kernel events go on the queue in
the order the deriver produced them (rows in published order, `deleted` before
`created` before `status` before `changed` within one refresh), then
`focus.*`, then `command.*`. User events emitted by a handler are appended and
delivered in the same call, up to **depth 4**: a fifth generation is dropped
and reported once, so a ping-pong between two plugins cannot pin the loop. A
reload clears the queue and enqueues `interface.reloaded` first — the plugins
that would have received the rest no longer exist.

**D5 — `on_event` is a sixth entry point, with `Phase::Event`.** Same shape as
`on_action`: `enter(plugin)`, `Budget::arm`, call, `clean_error`. A failure is
recorded against the plugin as a render failure is, keyed `(plugin, event)` so
a handler that throws on every `session.status` reports once per event, not
once per frame. Subscriptions are read from `events = { … }` at load
(`load.rs`, beside `keys`) and validated against `events::KERNEL_EVENTS`, a
`const` table of `(name, &[payload field])` that is also what the help page and
`thurbox-cli plugin events` render — one list, three readers. `user.<name>`
subscriptions are validated for shape only (`user.` + a non-empty identifier).

**D6 — `emit` is a `command` kind, not a new global.** `command("emit", { text
= name, … })` follows the existing convention (`text` is the subject, other
fields the payload), needs no `thurbox.yml` change, and is refused for a
kernel name at parse time like any malformed command. The emitting plugin's
name is stamped from `enter`'s current plugin, the way a program pane's owner
is — a plugin cannot forge `source`.

**D7 — The palette is a modal over the registry.** `Registry` gains
`commands: Vec<CommandDecl { plugin, action, description }>` collected at load;
`Registry::palette_rows()` returns bindings ∪ commands ∪ `modals::bindings()`
de-duplicated on `(plugin, action)`, chord attached when one exists. The modal
(`modals/palette.rs`) copies the theme picker's shape — a query, `lib.fuzzy`'s
matcher ported to Rust once (or, cheaper, the existing Rust subsequence
matcher the theme picker uses), a `matched/total` count, cursor addressed in
match space so refining keeps the selection. `Enter` returns a `Dispatch {
plugin: Option<String>, action }`; `Modals` closes itself, then the coordinator
routes it through the same `host.on_action(index, action)` a key press uses
(`coordinator/input.rs`), or the kernel handler for a kernel action. The
palette never calls `resolve` — there is no chord — and never bypasses
`Modals::toggle`'s one-open rule.

**D8 — `ctrl+p`, non-passthrough, no F-key alternate.** Alternatives: `ctrl+;`
(not deliverable on legacy terminals — only the kitty protocol separates it),
`ctrl+space` (tmux prefix for some users, IME toggle on others), any F-key
(all bound or awaiting except `f11`). Cost: a focused agent loses readline's
previous-history on `ctrl+p`; up-arrow is the same thing. The reassignment is
made in `tests/v2_keymap.rs` — `ctrl+p` leaves `CHORDS_AWAITING_THEIR_PANE`
and joins `GLOBAL_CHORDS` as `palette.open` — so it is on the record rather
than quiet, and the automations pane, when it returns, is reachable through
the palette rather than by that chord.

**D9 — Frame-cost contract.** Dispatch never touches `dirty`. A handler's
`state`/`store` write bumps `StateVersion`, which already invalidates `pure`
panes and marks the frame; `command(...)` sets dirty through the queue as it
does from a key. So the `frame-cost` delta is satisfied by construction, and
the existing settle test (`v2_frames`/`kernel_mvp`) gains one case: an event
delivered to a read-only handler paints no frame.

**D10 — `focus.*` from the coordinator's own two fields.** `focused_session`
and the focus ring's current plugin are compared at the end of
`drain_input` and after `apply_commands` (focus can move from a command);
a change enqueues one event. No new state.

## Risks / Trade-offs

- [A handler on the loop thread costs a frame's worth of Lua per event] →
  the `Budget` that bounds `render` bounds it; a storm is bounded by the
  snapshot's own cadence (one refresh per `data_version` move, ≥ the
  400 ms interval when nothing forces it) and by the cascade depth.
- [A plugin reacts to `session.status` by focusing, and fights the user] →
  documented as the pattern to guard with `store`/a setting, and the bundled
  example (`docs/examples/events.lua`) checks `focus.session` recency before
  moving focus. The kernel cannot police intent.
- [Palette runs a plugin-scoped action that assumed its pane was focused] →
  the action reads `state` the same way it does under a rebinding to a global
  chord today; the doc says so, and the bundled panes' actions are audited
  once for the assumption.
- [`ctrl+p` breaks a v1 user's muscle memory for automations] → the chord
  opens a palette in which `automations` is one query away once that pane
  returns; recorded in the keymap ledger, not silent.
- [Two event vocabularies for creation (`created` vs `post_create`)] →
  deliberate and documented in one table: `post_*` = *this* interface did it
  and has the operation's facts; `created` = it exists now, whoever did it.
- [The deriver diffs every row on every version bump] → it is O(rows) on a
  signal that already triggers a full republish; measured in the perf HUD
  under `tick`, and the bar is "not visible" at 50 sessions.

## Migration Plan

Additive. No schema, config or capability-grant change. A plugin without
`events`/`commands` is unaffected; `ui.json` rebindings are untouched except
that a user who had rebound something *onto* `ctrl+p` keeps their binding (a
user rebinding beats a default, as it does for every chord). Rollback is a
revert; nothing persists that a previous binary would misread.
