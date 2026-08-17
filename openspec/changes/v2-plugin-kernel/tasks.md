## 1. Capture the oracle — before anything else

Golden recordings can only be taken while `src/ui/` exists. No task in any later
group may start until group 1 is complete.

- [x] 1.1 Build a recording harness that renders a v1 pane against a headless backend **with an active session** and serializes what it drew, cell-exact
- [x] 1.2 Write the recording formatter so it destructures every field by name, making a dropped field a compile error rather than a silent omission
- [x] 1.3 Record the session list across its real states: repo grouping, nested children, every status glyph, remote and worktree marks, pending-spawn row, empty
- [x] 1.4 Record the central agent view: live output, no session selected, exited session, unreachable placeholder
- [x] 1.5 Record the status bar and footer across widths, including the responsive thresholds at 80 and 120 columns
- [x] 1.6 Record the arrangement itself: which regions exist at widths below 80, 80–119, and 120+
- [x] 1.7 Assert each recording against the live v1 renderer and commit them as the contract v2 must reproduce
- [x] 1.8 Verify no recording asserts against a builder that lives inside a module scheduled for deletion — a differential oracle cannot license a deletion

## 2. Plugin host

- [x] 2.1 Add the Lua 5.4 runtime with a deliberately chosen set of opened standard libraries, and record which are opened and why
- [x] 2.2 Confirm the VM is `!Send` (mlua's `send` feature disabled) and add a compile-fail test pinning it
- [x] 2.3 Load every `*.lua` in the plugin directory, reading each plugin's declared name, slot, order and focusability with documented defaults
- [x] 2.4 Implement whole-VM reload: build a fresh environment, swap only on success, keep the last good one on failure, drop the module cache
- [x] 2.5 Implement debounced filesystem watching plus an explicit reload-now action
- [x] 2.6 Implement private per-plugin state and one shared store, both surviving reload, preserving integer/float distinction
- [x] 2.7 Isolate render failures to the failing plugin's own region, leaving neighbours' state intact
- [x] 2.8 Isolate key-handling failures, keeping the application alive and dispatching subsequent keys
- [x] 2.9 Attribute every failure to a plugin and a phase (load, render, key)
- [x] 2.10 Bound execution with an instruction-count hook; abort an overrunning invocation as a plugin failure and prove an unterminated loop leaves the app responsive
- [x] 2.11 Bound memory with an allocator limit; prove exhaustion is a plugin error, not a process abort
- [x] 2.12 Measure the instruction-hook overhead against v1's frame timings and expose the budget as a setting

## 3. View tree

- [x] 3.1 Implement the four primitives — `text`, `box`, `input`, `surface` — and nothing else
- [x] 3.2 Implement child sizing: exact length, percentage, proportional share, min/max, and equal division of the remainder
- [x] 3.3 Make over-subscribed sizing deterministic and prove no child receives a negative size
- [x] 3.4 Invert the POC's order: resolve every plugin's rect **before** invoking it, and pass the resolved width and height to the invocation
- [x] 3.5 Prove a plugin nested inside a narrow region is told the region's width, not the screen's
- [x] 3.6 Add optional `id`, `class` and `role` to every node and preserve them through layout to the painted output
- [x] 3.7 Resolve colour by theme role, never by literal value, and prove a theme change repaints every plugin unedited
- [x] 3.8 Implement `surface` and paint it from a live session via the existing `vt100::Parser` + `tui_term` path
- [x] 3.9 Prove two surfaces naming two different sessions paint simultaneously, each in its own rect
- [x] 3.10 Implement plugin-supplied surface cells for geometry-first content
- [x] 3.11 Report a malformed tree against its plugin and keep rendering the rest of the screen
- [x] 3.12 Implement change-driven repaint: diff the tree, repaint on input or difference, hold the forced-redraw floor otherwise
- [x] 3.13 Add a perf counter test proving an idle application paints at the floor rather than the polling rate

## 4. Host API

- [x] 4.1 Build the snapshot: sessions with identity, name, agent, status, cwd, branch, backend, parent and manual order, plus repositories and hosts
- [x] 4.2 Refresh the snapshot on the kernel's own schedule, independent of plugin invocation, and stamp each with the instant it represents
- [x] 4.3 Expose reads that return immediately, and prove a read returns instantly while a configured remote host is unreachable
- [x] 4.4 Build the command bus: create, delete, restore, restart, fork, reorder, select, send input — each returning immediately
      <!-- create/fork/sync landed in v2-session-flows. `select` turned out to be
           plugin state, not a command — the list owns its cursor. -->
- [x] 4.5 Execute commands off the render path and surface their effects through a later snapshot
- [x] 4.6 Expose in-flight commands with their subject and phase, sufficient to render v1's pending-spawn row
- [x] 4.7 Report command failures through a subsequent snapshot rather than as an immediate error
- [x] 4.8 Audit the plugin environment for any filesystem, process or network binding and prove by test that none is reachable
- [x] 4.9 Add a test asserting the granted capability set matches a declared list exactly, so a new grant cannot be added silently

## 5. Registry

- [x] 5.1 Accept declarative key contributions carrying chord, stable action id, description and scope
- [x] 5.2 Route keys by scope: global chords anywhere, plugin-scoped chords only while their plugin holds focus
- [x] 5.3 Detect overlapping claims, resolve deterministically, and report naming both claimants
- [x] 5.4 Accept declarative setting contributions carrying id, type, default and description, and supply effective values to their plugin
- [x] 5.5 Persist overrides for bindings and settings; apply them in preference to defaults across restarts
- [x] 5.6 Retain an override whose action or setting no longer exists, without effect and without blocking other overrides
- [x] 5.7 Expose the whole registry as a read so help and settings surfaces can be plugins
- [x] 5.8 Reserve an un-overridable minimum — focus, reload, quit — and prove it survives a plugin that consumes every key

## 6. Composition

- [x] 6.1 Implement named slots and route each plugin's output to the slot it declared
- [x] 6.2 Implement the arrangement as a userland function of the available dimensions that positions slots without invoking plugins
- [x] 6.3 Isolate an arrangement failure from the plugins, and vice versa
- [x] 6.4 Implement `stack` mode: occupants visible together, sharing space by declared size
- [x] 6.5 Implement `switch` mode: one occupant visible, with the occupant set and active choice readable
- [x] 6.6 Persist the selected occupant of a switched slot
- [x] 6.7 Implement focus-claim: a plugin takes a switched slot while focused, restoring the prior selection when it loses focus
- [x] 6.8 Implement focus movement, report focus state to each plugin, and prevent focus resting on a hidden or removed plugin
- [x] 6.9 Implement key dispatch order: exclusive grab, then focused plugin, then non-focusable listeners, then system bindings
- [x] 6.10 Implement floating plugins with z-order and exclusive key grab, preserving the state of everything beneath
- [x] 6.11 Prove dismissing a float reveals the screen unchanged and returns input to the previously focused plugin

## 7. Userland foundation

The widget library is a deliverable, not a convenience — see design.md D1. Every
pane from group 8 onward is built on it.

- [x] 7.1 Write `lib/theme.lua`: the role palette, with no literal colour reachable from a plugin
- [x] 7.2 Write `lib/widgets.lua` — list, gauge, divider, titled panel, table — composed only from the four primitives
- [x] 7.3 Give widget-rendered rows node identity by default, so decoration and event targeting work without each pane opting in
- [x] 7.4 Write `layout.lua` reproducing v1's responsive thresholds, and prove editing a threshold takes effect on reload with no kernel change
- [x] 7.5 Add a test asserting the kernel still exposes exactly four node kinds — the guard against D1's failure mode

## 8. Vertical slice — the session list, end to end

The first pane runs to completion before any other is specified. This is where
D1, D2, D5 and the registry are tested by something real.

- [x] 8.1 Implement the session list as a plugin using only `lib/widgets.lua` and the host API
- [x] 8.2 Reproduce repo grouping, parent/child nesting and manual ordering
- [x] 8.3 Reproduce every status glyph, including the animated working spinner and the done/idle distinction
- [x] 8.4 Derive the scroll window, overflow markers and selection from the resolved rect — the D2 proof
- [x] 8.5 Render the pending-spawn row from in-flight command state — the D5 proof
- [x] 8.6 Wire every session action through declared keys and commands: select, create, delete, restore, restart, fork, reorder
- [x] 8.7 Assert the plugin against the group 1 recordings, cell-exact, in every recorded state
- [x] 8.8 Record what this slice taught before continuing — any primitive, capability or contract it needed that the specs did not anticipate

## 9. The rest of the bare core

- [x] 9.1 Central agent view as a plugin placing a session-fed surface, owning its own passthrough and scrollback rules
- [x] 9.2 Move the `Ctrl+<letter>` passthrough rule into plugin-owned Lua data and prove it is editable without recompiling
- [x] 9.3 Status line and footer plugin, asserted against its recordings across widths
- [x] 9.4 Help surface plugin, rendering the registry including plugins it does not know about
- [x] 9.5 Settings surface plugin, reading and recording overrides through the registry
- [x] 9.6 Session-creation flow as a floating plugin: repo, branch, agent and host selection
- [x] 9.7 Destructive-action confirmation as a floating plugin, preserving v1's risk assessment before deletion
- [ ] 9.8 Assert the whole bare core against the group 1 recordings — this is what licenses group 12
      <!-- The recordings exist and `tests/v2_parity.rs` measures against them.
           Every structural property holds — every pane exists, the layout
           thresholds match exactly, every session/branch/status reaches the
           screen. Cell-exact equality does NOT hold: six named divergences are
           asserted in KNOWN_DIVERGENCES so the gap can only shrink, and adding
           a seventh unnoticed is a test failure. `v2-retire-v1` stays blocked
           until that list is empty — which is the gate doing its job. -->

## 10. Delivery and recovery

- [x] 10.1 Embed the bundled plugins in the binary and render the default interface with no supporting file present
- [x] 10.2 Materialize the embedded plugins to the user's plugin directory on first run
- [x] 10.3 Prefer a user copy over the embedded copy where both exist
- [x] 10.4 Preserve a user-modified bundled plugin across upgrade and surface the difference; update unmodified ones
- [x] 10.5 Fall back to the embedded plugins when the user's directory is missing or fails to load, reporting the fallback and its cause
- [x] 10.6 Prove no plugin fault can leave the application with nothing rendered
- [x] 10.7 Prove the bundled plugins hold no capability unavailable to a user-written one

## 11. Performance and documentation

- [x] 11.1 Re-derive v1's perf counters against the new render path and restore their acceptance tests
- [x] 11.2 Re-establish the demand-driven redraw floor and confirm idle frame rate matches v1
- [x] 11.3 Re-derive the caching ADR-P6 provided, now as snapshot refresh policy rather than a UI-thread cache
- [x] 11.4 Confirm the non-blocking spawn behaviour ADR-P12 provided now falls out of the command bus, and test it
- [x] 11.5 Write the plugin-authoring guide, stating plainly that the API is public-but-unstable and that plugins are trusted code
- [x] 11.6 Rewrite `docs/ARCHITECTURE.md`, `docs/FEATURES.md`, `docs/PERFORMANCE.md` and the affected sections of `CLAUDE.md`

## 12. Theme system

The coherence layer every plugin depends on. Not a feature pane — without it a
plugin cannot express a colour at all, and v2 looks nothing like the thurbox the
user configured.

- [x] 12.1 Resolve the active theme at startup: built-in presets, user themes from `themes.toml`, and the persisted choice, falling back to the default when the recorded one is missing
- [x] 12.2 Publish the resolved palette to plugins as named roles, covering every distinction the bundled panes draw
- [x] 12.3 Report and skip a malformed user theme without preventing the rest — or the interface — from loading
- [x] 12.4 Rewrite `ui/lib/theme.lua` to read the published roles instead of carrying its own palette, and prove no bundled plugin names a literal colour
- [x] 12.5 Publish the selectable theme list (identifier, display name, light/dark) so a picker can be a plugin
- [x] 12.6 Add a `theme` command that persists the choice and takes effect without a restart
- [x] 12.7 Prove a theme change restyles every pane, including a plugin the theme's author never saw
- [x] 12.8 Assert every built-in identifier still resolves, so a persisted v1 choice stays valid

## 13. Carrying v1 settings forward

- [x] 13.1 Migrate existing `keybindings.json` overrides into the registry, so a user's bindings survive the move
- [x] 13.2 Report any override that no longer names a real action, without discarding the rest
- [x] 13.3 Add a test asserting every `SessionRow` field reaches Lua — `display_order` was in the snapshot and the database but never published, and every reorder silently did nothing

## Deletion is not in this change

Retiring v1 (`src/app/`, `src/ui/`, the v1 event loop) moves to **`v2-retire-v1`**,
which is gated on **full feature parity** — every v1 surface having a plugin
equivalent proven against its recording — not on the bare core alone. The bare
core proving itself licenses nothing but the next change.
