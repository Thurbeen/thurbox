## Why

A v2 frame costs 7.7ms of CPU where v1's costs 3.1ms, and the difference is not
that v2 does the shared work worse — painting, the vt100 surface and the flush
come to ~3.1ms, which is v1's whole frame. The gap is a second frame's worth of
work laid on top: running each pane's Lua, converting the table it returns, and
rebuilding every `thurbox.*` table. Measured under load, almost all of it is
recomputing answers that did not change — the session list produced a
**byte-identical tree on 200 of 200 renders**, and the agent pane repaints
because its *surface* moved rather than because its tree did.

So the loop already proves the work was wasted, but only after paying for it:
it builds every tree, then diffs them to decide whether to paint. Making the
cost proportional to what actually changed is what closes the gap, and it is
worth doing now because the observability to prove each step landed
(ADR-P11/P15) exists as of this branch.

## What Changes

- Each published source gains a **change-signal** — a generation on the
  snapshot, a version on the theme set and the registry, and one for the
  terminal metadata that is mutated on read today. Nothing else can gate on
  "did this move" until these exist, so they are the shared prerequisite for
  both halves.
- `kernel::host::publish` rebuilds **only the `thurbox.*` groups whose inputs
  moved**, instead of all ~20 on every painted frame. What Lua observes is
  unchanged: a group that is not rebuilt is one that could not have differed.
- A pane may **declare its render pure** (`pure = true` in its declaration
  table). The kernel caches a pure pane's converted node tree and reuses it
  while the published epoch and the render context are unchanged, skipping both
  the Lua call and the table→node conversion.
- Purity is **opt-in**, so an undeclared pane — including every third-party one
  — behaves exactly as it does today. This is deliberate: a render may have side
  effects (`ui/plugins/65_search.lua` writes to `store` while rendering) and may
  animate from `ctx.elapsed`, and neither is knowable from outside the plugin.
- The bundled panes that qualify declare it; `65_search` does not, and stays
  uncached until its `store` writes move out of `render`.
- The perf snapshot gains counters for the two new skips, so "the loop is
  settling" stays provable rather than hoped for.

## Capabilities

### New Capabilities

- `frame-cost`: what a painted frame is allowed to recompute — that published
  data a plugin reads is never stale, that a reused tree is indistinguishable
  from a freshly built one, and that both are observable.

### Modified Capabilities

- `plugin-authoring`: a pane can declare its render pure, and the declaration
  carries obligations — no side effects, no per-frame clock — in exchange for
  not being called on every frame.

## Impact

- `src/kernel/host.rs` — `publish` split into gated groups; `render` consults a
  tree cache; the plugin declaration gains `pure`.
- `src/kernel/snapshot.rs`, `src/kernel/theme.rs`, `src/kernel/registry.rs`,
  `src/kernel/terminal.rs` — change-signals.
- `src/kernel/perf.rs`, `src/cli/perf.rs` — counters for skipped publishes and
  skipped renders.
- `ui/plugins/*.lua` — bundled panes that qualify declare `pure`.
- `thurbox.yml` — the sandbox's declared shape, so `pure` lints.
- `docs/PLUGINS.md`, `docs/V2-KERNEL.md`, `docs/PERFORMANCE.md` (ADR-P15's
  "still open"), `CLAUDE.md`.
- No change to the four node kinds, to the published `thurbox.*` shape, or to
  what an undeclared pane sees.
