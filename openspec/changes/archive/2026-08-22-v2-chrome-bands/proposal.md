## Why

v2 has no top bar and no bottom bar. v1 has three chrome rows — a brand/version
header, a leveled status row for info and errors, and a footer carrying the focus
label, live counts and the clickable action pills — and all three were lost when
the interface was cut back to two panes.

They were plugins (`05_header.lua`, `90_footer.lua`), and that was the wrong
shape twice over:

- **The footer was not the only owner of its band.** The plugin drew key hints
  while the *kernel* painted status toasts onto the centre pane's bottom border,
  because there was nowhere else to put them. One band, two owners, and the
  status message had no level, no badge and no room.
- **They were panes that are not panes.** They never hold focus, take no input,
  and show the application's own state rather than the user's work — so each had
  to be hand-excluded from the focus ring, and deleting them forced an edit to
  the arrangement to remove slots nothing could fill.

`v2-system-modals` already settled this question for help, settings and the theme
picker: **system chrome is kernel-owned, and plugins contribute data to it**
(design D1). It explicitly rejected "keep them as plugins and fix the focus ring
instead" as treating the symptom. The bars are the same category of thing —
chrome about thurbox itself — differing only in being always-visible rather than
overlaid. This change extends that decision from modals to bands.

## What Changes

- Three **chrome bands** are rendered by the kernel: `header`, `status` and
  `footer`, reproducing v1's content.
- Bands are placed by `ui/layout.lua`, which names them as slots exactly as it
  names pane slots. The kernel decides what a band *contains*; the arrangement
  decides whether and where it appears. A band the arrangement omits does not
  draw.
- **BREAKING** (v2-internal): a slot may be occupied by the kernel rather than by
  a plugin. `ui-composition` currently assumes every occupant is a plugin.
- Plugins contribute footer pills by declaring them as data
  (`pills = { { action, label, priority } }`), alongside today's `keys` and
  `settings`. Adding a pane gets it a pill by declaring one table field.
- Live values in the bands — session counts, sync and creation progress, the
  focused pane's name, the version, the active theme — are derived by the kernel
  from state it already holds. **No plugin code runs while a band paints**, so a
  throwing plugin cannot break the chrome.
- The status band replaces the kernel's current toast-on-a-pane-border hack, and
  gains v1's levels (`INFO`, `✓ SYNC`, `ERROR`) and its "only carved when there
  is something to say" behaviour.

## Capabilities

### New Capabilities

- `chrome-bands`: persistent, kernel-rendered bands that report the
  application's own state — what each contains, how they are placed and sized,
  how a message is surfaced and retired, and how a plugin contributes an entry
  without being able to break or draw into a band.

### Modified Capabilities

None. The capabilities this interacts with (`ui-composition`, `plugin-registry`)
are still change-local — `openspec/specs/` holds nothing yet — so their deltas
belong to the changes that introduce them, not here. `chrome-bands` states the
slot-occupancy and declaration requirements it needs, and `design.md` records
where they touch.

## Impact

- **Kernel**: a new band renderer, plus a third declaration kind collected by
  `LuaHost::declarations`. Removes the status-message special case from the paint
  path in `src/bin/thurbox2.rs`.
- **Arrangement**: `ui/layout.lua` regains `header` / `status` / `footer`, and
  the layout resolver must accept a slot the kernel occupies.
- **Plugins**: additive only. A plugin that declares no `pills` is unaffected.
- **Parity**: closes the `status bar` and `header band` rows of
  `v2-parity-gaps`, and lets `tests/v2_parity.rs` move both out of its
  "awaiting their plugin" list — where they will need a different home, since
  neither returns as a plugin.
