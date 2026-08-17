## Why

Help, settings and the theme picker are currently ordinary plugins in the centre
slot. That was a faithful reading of "every pane is a plugin", but it is the
wrong shape for these three, and it shows:

- They **compete with the terminal**. Opening help replaces the agent view
  rather than covering it, so you lose sight of the thing you are working on.
- They **join the focus ring**. `Tab` visits every centre-slot occupant, so
  reaching the session list can take five presses where v1 takes one.
- Help is **read-only** where v1's is an editor, and settings renders an empty
  pane because **no plugin declares a setting** — the contribution API exists
  (`Registry::settings`) and nothing uses it.

The mistake was conflating two different things. A *pane* shows your work and
belongs in the layout. Help, settings and the theme picker are **system chrome**:
they are about thurbox itself, they overlay, they are modal, and they are the
same in every install. Making them plugins bought no extensibility — a user
replacing the help pane wholesale is not a use case — while costing the layout,
the focus ring, and a contribution mechanism nobody could reach.

Modularity for these belongs one level down: not *who draws the modal*, but
**what plugins declare into it**.

## What Changes

- **Help, settings and the theme picker become kernel-owned modals.** They are
  no longer plugins, no longer in any slot, and never in the focus ring. They
  overlay, capture input while open, and close on `Esc`.
- **Help renders the registry**, so a plugin's keys appear in it by declaring
  them and nothing else. It becomes an **editor**: capture a chord, reset one,
  reset all — v1's `Modal::Help` behaviour, against `Registry::rebind`, which
  already exists and persists.
- **Settings renders declared settings.** The bundled plugins gain real
  `settings` declarations, so the modal has content and a plugin author gets a
  settings row by declaring one field.
- **The theme picker takes no contribution.** Its list is the built-in presets
  plus `themes.toml`; there is nothing for a plugin to add, so it declares no
  extension point.
- **The centre slot collapses to one occupant.** Agent and shell become tabs of
  a single `terminal` plugin rather than two plugins racing for the slot, which
  is v1's shape and makes the tab strip belong to the pane that owns it. Code
  review stays a separate concern and is explicitly **not** part of this change.

## Capabilities

### New Capabilities

- **System modals**: kernel-owned, overlaying, input-capturing UI for help,
  settings and themes, above the arrangement and outside the focus ring.
- **Keybinding editing**: rebind, reset and reset-all from the help modal,
  persisted through the registry.
- **Plugin-declared settings**: a plugin contributes a settings row as data,
  and the modal edits it without knowing what it is for.

### Removed Capabilities

- `help`, `settings` and `themes` as plugins, and their centre-slot occupancy.
- The `shell` plugin as a separate centre-slot occupant (folded into `terminal`).

## Non-Goals

- Code review. It stays where it is; the centre pane keeps room for it as a
  third tab, but nothing about it changes here.
- User-replaceable help or settings UI. That is the capability being
  deliberately given up, in exchange for the layout and focus ring.
