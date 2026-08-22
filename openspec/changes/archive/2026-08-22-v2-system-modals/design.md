# Design

## D1 — Where the modularity lives

**Decision: plugins contribute *data* to system modals; the kernel owns the
rendering.**

The kernel already collects two declarations from every plugin
(`LuaHost::declarations`): `Binding` and `Setting`, each stamped with the plugin
that declared it. That is the whole extension point, and it is enough:

| Modal | Contributed by | Contribution |
|---|---|---|
| Help | every plugin | `keys = { { key, action, desc, group, scope } }` |
| Settings | every plugin | `settings = { { id, description, default } }` |
| Theme | nobody | presets + `themes.toml`, resolved by the kernel |

A plugin therefore gets a help row and a settings row **by declaring one table
field**, with no knowledge of how either modal draws. That is stronger
modularity than today's arrangement, where the help *plugin* is replaceable but
no plugin can contribute to it in a way it did not anticipate.

The theme picker is deliberately asymmetric. There is nothing a plugin could
usefully add to a list of palettes, so it declares no extension point at all
rather than an unused one.

**Rejected: keep them as plugins and fix the focus ring instead.** That treats
the symptom. The ring is crowded *because* system chrome is competing for a
layout slot; a pane that overlays and is modal does not want a slot.

## D2 — What "kernel-owned modal" means concretely

A system modal is **not** a `Node` tree returned by Lua. It is drawn by the
kernel in Rust, above the arrangement and above plugin floats, in the same place
the perf HUD and the error panel already draw.

Consequences, each of them a reason for the choice:

- **Not in the layout.** No slot, so it cannot shrink a pane or be shrunk.
- **Not in the focus ring.** `focusable()` never contains it, so `Tab` keeps
  v1's short ring: session list → terminal → visible side panes.
- **Captures input.** While open, keys go to the modal first and are not offered
  to any plugin. `Esc` closes; the chord that opened it toggles.
- **One at a time.** Opening one closes another. There is no z-order question
  because there is no stack.

## D3 — Why help must be an editor, not a viewer

v1's F1 panel rebinds keys live: select an action, press a chord, it is captured
and persisted. v2 has the whole mechanism already (`Registry::rebind` with
conflict detection, scope-overlap checks and JSON persistence) and no way to
reach it. Shipping a read-only list would leave that dead and lose a v1 feature.

Capture is the one piece of genuinely modal input in the product: while
capturing, **every** chord is data, including `Ctrl+Q`. That is exactly why it
belongs to the kernel — a plugin cannot be allowed to swallow the quit chord,
but the kernel can, because it knows it is capturing and for how long.

## D4 — The centre slot collapses to one pane

Agent, shell and (later) review are three *views of one pane* in v1, selected by
a tab strip drawn on that pane's border. v2 modelled them as three plugins
racing for one `switch` slot, which produced three problems:

1. the tab strip lives in the agent plugin, so it vanishes on the other tabs;
2. each is a separate focus stop, inflating the ring;
3. `slot_selection` exists only to arbitrate between them.

Folding agent and shell into one `terminal` plugin with a `tab` in its own state
fixes all three and matches v1. Review stays out of this change, but the pane is
shaped to take it as a third tab.

**Rejected: keep the switch slot and move the strip to the kernel.** The strip
is a property of the pane that owns the tabs, not of the screen; drawing it in
the kernel would mean the kernel deciding what a tab is.

## D5 — Settings values stay a knob, not a document

`Registry::Value` is `Bool | Number | Text`. It stays that way. A plugin wanting
structured configuration should read a file through its own capability, not
smuggle a document through the settings modal — the modal's job is to render a
row and write a scalar back.

Open question deferred: v1's `settings.toml` has feature flags that gate whole
panes (`tasks`, `code_review`, …). Whether those become plugin-declared settings
or stay a kernel concern is decided when the flags are ported, not here.
