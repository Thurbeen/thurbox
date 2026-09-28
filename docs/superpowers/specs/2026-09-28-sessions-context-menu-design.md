# Sessions context menu — design

Date: 2026-09-28 · Status: proposed

## Goal

A right-click on a session row in the sessions column opens a menu of that
session's actions, drawn at the pointer. Every entry runs an action the pane
already has, so the menu adds a way in, not new behaviour.

## Decisions taken

| Question | Answer |
|---|---|
| Where the menu opens | At the pointer (not a centred modal, not inline in the column) |
| How the kernel places it | A generic float anchor — `float.at = { x, y }` — with flip/clamp. No kernel-drawn menu widget, no anchoring to a node id |
| Who draws it | A generic Lua float, `ui/plugins/64_menu.lua`, fed through `store.menu` like `confirm` and `rename` |
| What a right-click does to the row | Selects it (the cursor moves); focus does not move |
| Entries | See [Entries](#entries) |

## Entries

Three groups, separated by a rule. The chord column is read from
`thurbox.registry.keys` for the action, so a rebind shows up; an action with no
chord shows none.

```text
 Open                 ⏎      sessions.open
 Rename                      sessions.rename
 Fork                        sessions.fork
 Open in editor              sessions.editor
 ─────────────────────
 Restart         ^R / r      sessions.restart
 Sync                        sessions.sync
 Move up              K      sessions.move_up
 Move down            J      sessions.move_down
 ─────────────────────
 Delete              ^D      sessions.delete
 Delete + worktree    D      sessions.force_delete
```

`sessions.sort`, `sessions.toggle_panel` and `sessions.undo` are left out: they
do not target a session.

### Off the rows: the pane's menu

A right press on empty space or on a repo header (neither carries an `id`)
opens the column's general menu at the pointer, with no `target`:

```text
 New session          ctrl+n     new_session.open
 Restore deleted…     ctrl+u     restore.open
 ─────────────────────
 Sort by name              S     sessions.sort      (only with sessions)
 Undo delete          ctrl+z     sessions.undo      (only after a delete)
 ─────────────────────
 Hide panel               F9     sessions.toggle_panel
```

It is built when it opens, so an entry that would do nothing is left out, with
its rule when its group empties. A header opens the same menu rather than a
per-repo one: "new session in this repo" needs the creation flow to accept a
prefilled repo, which it does not today.

## Kernel changes

### 1. Screen coordinates on `hit`

`Click` (`src/kernel/host/mod.rs`) gains `screen_x: u16` and `screen_y: u16`, the
absolute cell of the press. `hit.x`/`hit.y` stay node-relative. Every click
hook gets them (`on_click`, `on_context`, and the new `on_outside`), since
`click_at` builds one `Click` for all of them.

A pane cannot compute the absolute position itself: it knows neither where its
slot sits nor where the node landed in it.

### 2. `float.at`: an anchored float

`thurbox.Float` gains `at = { x = integer, y = integer }` (screen cells, as
`hit.screen_x`/`screen_y` report them). `read_float` reads it. `float_rect`
(`src/coordinator/draw.rs`) keeps the size it computes today, then:

- with no `at`: centres, as now;
- with `at`: puts the top-left corner at `(x, y)`; if the rect would pass the
  right edge it opens to the left of the point (`x + 1 - width`), and if it
  would pass the bottom it opens above (`y + 1 - height`); the result is then
  clamped inside the area, so a menu taller or wider than the screen still
  stays on it.

Flip-then-clamp is what desktop menus do: a menu opened near the bottom-right
corner grows up and to the left rather than being cut off.

### 3. `on_outside`: telling a float that a press missed it

Today a press outside the float that holds the pointer is swallowed
(`float_grab` → `Grab::Held(None)`), and the float never hears of it — so a menu
could never close on a click elsewhere. A float may now declare
`on_outside(hit)`; when a press of either button is swallowed by that float,
the kernel calls it with the press's `hit` (no `id`, `screen_x`/`screen_y` set).

Its own hook rather than an `on_click` with an "outside" flag, for the reason
`on_context` got one: every existing float's `on_click` would suddenly hear
presses it was never written for. A float that declares no `on_outside` behaves
exactly as today. The press stays swallowed either way: it does not also reach
the pane below, so closing a menu cannot also select a row or focus a terminal.

## Interface changes

### `ui/plugins/64_menu.lua` — the menu float

Knows nothing about sessions. A pane opens it with:

```lua
store.menu = {
  at = { x = hit.screen_x, y = hit.screen_y },
  items = {
    { label = "Open", action = "sessions.open" },
    "sep",
    ...
  },
}
```

- Renders a bordered list with `float = { at = ..., cols = ..., rows = ... }`,
  width fitted to the longest label plus chord, height to the entries.
- Keys (it takes every key while up): `up`/`k` and `down`/`j` move over the
  entries, skipping rules; `enter` runs the highlighted one; `esc` closes.
- `on_click` on an entry runs it; on a rule does nothing.
- `on_outside` closes it.
- Running an entry is `store.menu = nil` then
  `command("action", { text = item.action })`. Closing first means an action
  that opens its own float (rename, fork, confirm) is not stacked under the
  menu.
- Chord hints come from `thurbox.registry.keys`, first binding per action, in
  the display form help already uses.

### `ui/plugins/10_sessions.lua`

Adds `on_context(hit)`: returns `false` with no `hit.id`, otherwise
`select_by_id(hit.id)` on the same cursor `on_click` uses and writes
`store.menu` with the entries above. Nothing else changes: the actions run
through the existing `on_action`, so delete and force-delete still go through
`confirm` and the soft-delete/undo path.

### Types and lint

- `ui/lib/thurbox.d.lua`: `screen_x`/`screen_y` on `thurbox.Hit`; `at` on
  `thurbox.Float` (with a `thurbox.FloatAt` class); `on_outside` on the plugin
  declaration.
- `thurbox.yml`: unchanged — it describes `thurbox.*`, and none of this is
  published there.

## The pressed session is pinned

An action carries no argument, so the menu names what it was opened on:
`store.menu.target`, handed back on a choice as
`store["menu.chosen"] = { action, target }`. The sessions pane re-selects that
session before acting, and refuses with a message when it is gone — otherwise a
session deleted by another instance under an open menu would leave the cursor on
a neighbour, and Delete + worktree would act on it instead. (Raised in review;
the first version of this spec accepted that risk.)

A screen shorter than the menu shows a window of the entries, slid to keep the
highlight in view, so an entry `enter` would run is always visible.

## Testing (test-first)

Rust:

- `float_rect` unit tests: `at` inside the area; flipping at the right edge;
  flipping at the bottom; a float larger than the area stays inside it; no `at`
  still centres (the existing `float_rect(...) escaped` sweep gains anchored
  cases).
- `read_float`: `at` read, malformed `at` reported as `float.at: …`.
- `tests/mouse.rs`: `on_context` receives `screen_x`/`screen_y`; a press
  outside a held float calls its `on_outside` and reaches nothing below; a
  float without `on_outside` behaves as today.

Interface (the existing kernel/interface test files, per `thurbox-testing`):

- Right press on a session row selects it and opens the menu at the pointer;
  on a header it opens nothing.
- `enter` on an entry closes the menu and runs its action (rename opens the
  rename float; delete goes through confirm when soft delete is off).
- `esc` and an outside press close it; `j`/`k` skip rules.
- Chord hints follow a rebind.

Lint gates: selene, stylua, luals `--check ui`, `check-lua-types.sh`.

## Docs updated in the same PR

- `docs/PLUGINS.md`: *The right button* shows a menu opened at
  `hit.screen_x`/`screen_y`; *Floating panes and modals* documents `at` and
  `on_outside`.
- `ui/README.md`: the menu float and `store.menu`.
- `thurbox-kernel` skill (plugin API section) and `thurbox-ui-surfaces` skill
  if they list the hooks or float fields.
- `docs/FEATURES.md`: the sessions context menu.
