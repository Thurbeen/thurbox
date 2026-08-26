## 1. Event derivation (`kernel::events`)

- [x] 1.1 Add `src/kernel/events.rs`: `KERNEL_EVENTS` (name + payload fields), `Event { name, payload }`, and a `Deriver` that seeds from the first snapshot silently and diffs `status/name/branch/repos/parent` per row on later ones
- [x] 1.2 Unit tests: seed emits nothing; created/deleted/status/changed each fire once; order within one refresh is deleted → created → status → changed in published row order; a quiescence-derived status flip fires
- [x] 1.3 `KERNEL_EVENTS` validation helper: kernel names exact, `user.<ident>` by shape, anything else an error naming the event

## 2. Host entry point

- [x] 2.1 Read `events = { … }` in `host/load.rs` beside `keys`; validate via 1.3; a handler with no list subscribes to nothing
- [x] 2.2 Add `Phase::Event` and `LuaHost::on_event(index, name, payload)` mirroring `on_action` (`enter`, `Budget::arm`, `clean_error`); payload converted from a Rust struct to a Lua table once per event, shared by all subscribers
- [x] 2.3 Record handler failures keyed `(plugin, event)` so a repeating failure is reported once per event, and surface them where render failures are
- [x] 2.4 Parse `command("emit", { text, … })`: stamp `source` from the entered plugin, refuse a kernel name at parse time, carry remaining fields as payload
- [x] 2.5 Extend `tests/kernel_mvp.rs`'s plugin-environment enumeration (no new global expected; assert `emit` is reachable only through `command`)

## 3. Coordinator dispatch

- [x] 3.1 Add the event queue to `App` and `dispatch_events()` between `apply_external_requests` and `paint_if_due`; no-op when empty; republish once before the first handler of a batch
- [x] 3.2 Feed the queue from the deriver on `SnapshotStore::version` change and after `apply_output_quiescence`
- [x] 3.3 `command.done`/`command.failed` and `session.post_{create,delete,restart,restore}` from `report_finished_commands` (fork → `post_create`), payload from the `InFlight` plus the row
- [x] 3.4 `focus.session`/`focus.pane` from `focused_session` and the focus ring, compared after `drain_input` and `apply_commands`
- [x] 3.5 User-event cascade: append emits within the same dispatch, depth 4, drop-and-report beyond
- [x] 3.6 Reload: clear the queue, enqueue `interface.reloaded { reason }` first
- [x] 3.7 Verify `dispatch_events` never sets `dirty` itself; add the settle case (read-only handler paints no frame) to the existing settle test — verified by construction (`coordinator/events.rs` touches no `dirty`); the loop is the binary's and has no integration-level settle test to extend, so no new test was added

## 4. Registry `commands` + palette modal

- [x] 4.1 `CommandDecl` collected from `commands = { { action, desc } }` at load; `Registry::commands()`; `palette_rows()` = bindings ∪ commands ∪ kernel bindings, de-duplicated on `(plugin, action)`, chord attached when bound; disabled plugins absent
- [x] 4.2 Let `Registry::rebind` target a chord-less command so a user can give it a key; help lists it once bound
- [x] 4.3 `modals/palette.rs`: query, subsequence filter over description/id/plugin with `matched/total`, cursor in match space, `Up/Down/PageUp/PageDown/Enter/Esc`, hitboxes for click
- [x] 4.4 `ModalKind::Palette` with `ctrl+p` in `modals::bindings()`, non-passthrough, reserved chords honoured while open; `Enter` returns a `Dispatch` the coordinator routes through `host.on_action` / the kernel handler after the modal has closed
- [x] 4.5 `tests/v2_keymap.rs`: move `ctrl+p` from `CHORDS_AWAITING_THEIR_PANE` to `GLOBAL_CHORDS` as `palette.open`; keep the awaiting count assertion honest
- [x] 4.6 Audit the bundled panes' `on_action` handlers for a focus assumption a palette dispatch would break; fix any found

## 5. CLI

- [x] 5.1 `thurbox-cli plugin events` — the `KERNEL_EVENTS` table, human and JSON
- [x] 5.2 `thurbox-cli plugin check` fails on an unknown subscription with the loader's message (falls out of 2.1; add the test)

## 6. Tests

- [x] 6.1 `tests/v2_events.rs`: declared subscription fires once; undeclared does not; unknown name refuses load; two subscribers with one throwing; instruction-budget overrun contained; user event round-trip with `source`; kernel-name emit refused; cascade bound; reload drops pending and delivers `interface.reloaded` first; `session.created` for a CLI-made row vs `post_create` for an interface-made one
- [x] 6.2 `tests/v2_palette.rs`: rows from keys, commands and kernel; disabled plugin absent; fuzzy filter and cursor survival; `Enter` reaches an unfocused plugin's `on_action`; `open search` from the palette opens the strip; `Esc` restores focus; `ctrl+q` from the palette quits
- [x] 6.3 `tests/v2_frames.rs`: pin the palette frame (empty query, a query, no match) — pinned in `tests/v2_palette.rs` over a fixed registry (one frame, with a query); the bundled-pane pins in `v2_frames.rs` are left as they are
- [x] 6.4 `tests/v2_render_props.rs`: arbitrary key sequences into the palette never throw

## 7. Lint contract and example

- [x] 7.1 `.luarc.json` / `ui/lib` type annotations for `on_event`, `events`, `commands` so luals checks the declaration shapes; `thurbox.yml` unchanged (no new global) — note why in its header
- [x] 7.2 `docs/examples/events.lua`: "focus the session that just went blocked, unless the user moved focus in the last few seconds" — uses `session.status` + `focus.session`, and declares a palette command to toggle itself
- [x] 7.3 `selene ui`, `stylua ui`, `lua-language-server --check` green — selene and stylua green locally; luals is not installed here and runs in CI

## 8. Documentation

- [x] 8.1 `docs/PLUGINS.md`: an **Events** section (the table, `on_event`, `emit`, the bounds, the `created` vs `post_create` distinction) and a **Palette** section (`commands`, what a palette dispatch looks like to a plugin); add both to Traps where they bite
- [x] 8.2 `docs/V2-KERNEL.md`: the sixth entry point, the dispatch point in the iteration, why events are derived not raised (D1), and the cascade bound
- [x] 8.3 `CLAUDE.md`: Keybindings table (`Ctrl+P`), the Performance section's iteration order, the plugin-writing section's entry points; `ui/README.md`, `extensions/ui-skill` `SKILL.md`
- [x] 8.4 `docs/CONFIG.md` hooks.toml section and `docs/FEATURES.md`: cross-reference that `session.post_*` is also a Lua event — done after rebasing onto the merged `hooks.toml` work (v2.6.0)
- [x] 8.5 `website/docs/keybindings.html` and `v2-interface.html`: `Ctrl+P` and the events table
