# Design

## Context

See `proposal.md` — Why. Three constraints shape the approach:

1. `v2-system-modals` D1 already decided the general question — plugins
   contribute data to system chrome, the kernel renders it — and rejected
   keeping chrome as plugins. This change applies that ruling to always-visible
   bands rather than re-litigating it.
2. The kernel already collects per-plugin declarations
   (`LuaHost::declarations` → `Binding`, `Setting`), each stamped with the
   plugin that made it. A third kind is an extension of a mechanism in place,
   not a new one.
3. `ui/layout.lua` is the arrangement, and the project's stated value is that
   it is *a file you can edit rather than a layout compiled into the binary*
   (`docs/PLUGINS.md`). A design that hides the bars from it would contradict
   that for the parts of the screen most people want to move or remove.

## Goals / Non-Goals

**Goals:**

- One owner per band, replacing today's split where the footer plugin drew hints
  and the kernel painted toasts onto a pane's border.
- Chrome that a failing plugin cannot break, because drawing it runs no Lua.
- A pane earns a footer entry by declaring one table field, knowing nothing about
  how the band draws.
- The arrangement keeps authority over placement, so the bars stay movable.

**Non-Goals:**

- Restyling. The bands reproduce v1's content and ordering; a redesign is a
  separate change.
- A general "chrome plugin" API. If a future band needs plugin-authored
  *rendering*, that is a new decision, not something to leave a hook for now.
- Per-band theming knobs. Bands use existing theme roles.

## Decisions

### D1 — Kernel renders, arrangement places

**Decision: the kernel owns each band's contents; `ui/layout.lua` names bands as
slots and decides whether, where and in what order they appear.**

This splits the two questions that were tangled together. *What a band contains*
is application state and belongs where that state lives. *Where a band sits* is
arrangement, and the project already treats arrangement as user-editable data.

The consequence worth naming: a slot may now be occupied by the kernel rather
than by a plugin. `ui-composition` currently reads as though every occupant is a
plugin. The resolver must therefore place a named region whose occupant supplies
no `Node` tree, and the paint step must route that region to the band renderer.
This is the one structural concept the change adds.

**Rejected: the kernel carves the top and bottom rows before the arrangement
runs.** Simpler, and it makes a band impossible to squeeze out — but the bars
become furniture that editing `layout.lua` cannot move or hide, in the one file
whose whole purpose is deciding what the screen looks like. Visibility would
have to migrate to settings, which is a worse home for it.

**Rejected: keep them as plugins.** D1 of `v2-system-modals` applies unchanged;
additionally, the pills are drawn from the *registry*, so a footer plugin renders
other plugins' actions — backwards, and impossible to make safe.

### D2 — Static shape from plugins, live values from the kernel

**Decision: a contribution is data collected at load and reload
(`pills = { { action, label, priority } }`); everything that changes while
running is read by the kernel from state it already holds.**

The rule this enforces is the specs' "a band cannot be broken by a plugin". If a
band asked a plugin for a value while painting, then a slow, throwing or
non-terminating plugin would take the chrome with it — the failure mode the
kernel exists to prevent. Collecting declarations up front means a band paints
from data that is already in hand.

It also removes a class of disagreement: a count that both a plugin and the
kernel could report is a count that can be reported two ways. Session counts,
in-flight work, the focused surface, the version and the theme are all already
known to the kernel.

**Rejected: allow plugins to publish live values into a reserved `store`
namespace.** More expressive — a pane could surface "3 due" — and it was
considered seriously, because the shared store already exists and the band would
still be reading *data*, not calling code. It is deferred rather than refused: it
is additive, and adding it later costs nothing that adding it now would save,
whereas shipping it now means specifying truncation, tone vocabulary, staleness
and precedence against kernel-derived values before anything needs them.

### D3 — An entry naming an unknown action does not render

**Decision: a pill whose action is declared nowhere is dropped, not drawn
disabled.**

Learned from the tab strip: while the review pane existed, its chip carried
`focus:review`; when the pane was deleted the chip remained, lighting on hover
and doing nothing. A dead affordance is worse than a missing one, because it
costs a press to discover.

This also makes removal self-healing — deleting a plugin retires its pill with
no other edit — which is the same property the declaration mechanism gives keys.

### D4 — The message band is a band, not a row stolen from a pane

**Decision: status messages get their own named band, occupying a row only while
there is a message.**

Today the kernel paints a message over the centre pane's bottom border, because
that was the only row a Lua-owned arrangement left spare. It works, and it costs
the band any severity, any badge, and any ability to be placed — the message
cannot be told from the pane it is defacing.

Giving it a band restores v1's `INFO` / `✓ SYNC` / `ERROR` levels and its
appear-only-when-needed behaviour, and lets the arrangement decide where a
message belongs.

Progress that outlives the retention window is deliberately a different thing
from a message (v1 learned this: creation phases routinely run past the 5 s
expiry, so a toast was the wrong carrier). The spec separates them; where
progress is *shown* — its own band region, or the action band's left cluster —
is left to implementation.

### D5 — Height pressure drops the identity band first

**Decision: under height pressure, bands are dropped in a fixed order —
identity first, then action, with the message band last.**

v1 drops its header below 20 rows for the same reason: the pane area is what the
user is working in, and the identity band is the least urgent row on screen. The
message band survives longest because a hidden error is the one failure this
chrome exists to prevent.

## Risks / Trade-offs

- **A slot with a non-plugin occupant complicates the resolver.** → The concept
  is confined to placement: the resolver learns that a named region may have no
  Lua occupant, and paint routes it. No new node kind, no new plugin kind.
- **The arrangement can now hide the error band.** A user editing `layout.lua`
  could remove the surface errors are reported on. → Errors continue to be
  logged; the bundled arrangement places all three; and the drop order in D5
  makes the message band the last to go automatically.
- **Declared-at-load pills cannot react to state.** A pill cannot appear only
  when it is relevant. → Accepted for now: v1's footer is likewise a fixed set
  filtered by feature flags. D2's deferred escape hatch is the answer if a real
  case appears.
- **Three bands plus panes is more layout arithmetic on every frame.** → Bands
  are fixed-height single rows resolved with the existing pass; the cost is a
  rect calculation, not a render. The message band costs nothing while empty
  because it is not placed.

## Migration Plan

Additive; no persisted state and no user-facing config changes.

1. Land the band renderer and the slot-occupancy support with the bundled
   arrangement still placing nothing — bands exist and are unreachable.
2. Place the three bands in `ui/layout.lua`; the status special case in the
   paint path is removed in the same step, so the message band never has two
   owners at once.
3. Add the `pills` declaration and move the bundled panes' entries onto it.

Rollback is removing the bands from `ui/layout.lua`, which leaves a working
two-pane interface — the state before this change.

## Open Questions

- Where in-flight progress is drawn once it is no longer a message: its own
  region of the message band, or the action band's left cluster. Both satisfy
  the spec; the choice can be made when the second consumer exists.
