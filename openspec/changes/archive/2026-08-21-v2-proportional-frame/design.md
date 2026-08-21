## Context

See `proposal.md` — Why, and ADR-P15 in `docs/PERFORMANCE.md` for the frame
budget this is working against.

Three facts from that budget shape the approach:

- The work to remove is **producing**, not painting. Painting, the vt100
  surface and the flush come to ~3.1ms, which is v1's entire frame. The Lua
  call, the table→node conversion and the `thurbox.*` rebuild are the extra
  ~4.6ms.
- The loop **already knows** the work was wasted — `draw` diffs each pane's tree
  against last frame's — but only after building it. This change moves that
  knowledge in front of the work.
- A render is **not provably pure**. `ui/plugins/65_search.lua` writes to
  `store` from inside `render`, and any pane may animate from `ctx.elapsed`.
  Nothing outside the plugin can see either.

One constraint from ADR-P11 carries over: deciding whether to skip must cost
nothing when perf timing is off, so the decision may not read a clock.

## Goals / Non-Goals

**Goals:**

- One place that answers "has anything published moved", cheap enough to ask
  per frame per pane.
- Gating that is **wrong-by-construction-proof**: a group that is not rebuilt
  is one that could not have differed, demonstrated by a test that compares a
  gated publish against a full one.
- An opt-in purity declaration whose absence changes nothing.

**Non-Goals:**

- Per-pane dependency granularity. A pure pane is invalidated when *any*
  published source moves, not only the ones it read. Coarse, and enough: the
  measured waste is frames where nothing moved at all.
- Changing the four node kinds, the published `thurbox.*` shape, or what an
  undeclared pane observes.
- Making `65_search` pure. Moving its `store` writes out of `render` is a
  separate change with its own behaviour to think about.
- Bounding the repaint *rate*. Rejected in ADR-P15 and unchanged here.

## Decisions

### Purity is declared by the pane, not inferred

**Chosen**: a pane declares `pure = true` in its declaration table.

The kernel cannot see a side effect or a clock read from outside the VM, so the
alternatives were to guess or to ask.

- *Opt-out (cache by default, declare `animated`)* — rejected: it silently
  breaks any existing pane that writes state while rendering, including a
  bundled one, and the failure is a pane that stops updating rather than an
  error. A performance change must not be able to do that to a third-party
  plugin that was never touched.
- *Read-tracking (proxy `thurbox.*`, key the cache on fields actually read)* —
  rejected for now: it is the most precise answer and needs no annotation, but
  it does not solve side effects either, and it is a much larger change. Opt-in
  purity is compatible with adding it later: the declaration would become the
  fallback for panes the tracker cannot prove.

The cost is that purity is an assertion. It is bounded by being opt-in, stated
in `docs/PLUGINS.md` next to the declaration, and lintable — `thurbox.yml`
learns the key, so a misspelling is a lint error rather than a pane that
quietly renders every frame.

### One epoch, not a dependency graph

**Chosen**: every published source carries a version; the **publish epoch** is
those versions taken together. A pure pane's tree is cached under
`(epoch, width, height, focused)`.

`ctx.frame` and `ctx.elapsed` are deliberately **not** in the key. They change
every frame, so including them would make the cache never hit; excluding them
is exactly what the purity declaration buys, and why a pane that animates may
not declare it.

A per-pane dependency set would hit more often, but the measurement says it
would not matter: the wasted frames are ones where *nothing* moved, not ones
where an unrelated source moved.

### A version lives with the mutation, not the caller

The failure mode this whole design has to avoid is a source that changes
without its version moving. So a version is bumped **inside** the code that
performs the mutation — `SnapshotStore` when it replaces the snapshot,
`Themes` when a palette is activated, `Registry` when a setting, binding or
disabled entry changes, `Terminals::meta` when it actually writes an entry
(it is mutated on read today, which is why it needs one at all).

Sources that are already small values — the focused pane, the hovered target,
the status row — are compared directly rather than versioned; there is nothing
cheaper about a counter for a `bool` and an `Option<String>`.

### Group gating is a map from group to the versions it reads

`publish` becomes a sequence of groups, each naming the versions it is built
from. A group is rebuilt when any of them moved since the last publish; the
`thurbox` table itself is still assembled fresh each frame from either a newly
built group or the one cached from last time, so the table a plugin sees is
never partially updated.

Keeping the outer table fresh is deliberate: it means a bug in gating can only
ever produce a *stale group*, never a torn table, and it keeps the change to
`publish` local to the group builders.

### The proof is a test, not a review

The one test that matters compares, for a range of mutations, what a gated
publish produces against what a full rebuild produces — mutate one source, take
both, assert equal, and assert the epoch moved. A gating bug is otherwise
invisible until a user reports a pane that stopped updating.

## Risks / Trade-offs

- **A source mutates without bumping its version** → the version lives inside
  the mutation path, and a test asserts that mutating each source moves the
  epoch. This is the one failure that produces silent staleness, so it gets the
  explicit test rather than a comment.
- **A pane declares purity and lies** (writes `store`, reads a clock, reads
  something it was not given) → it paints from a stale tree. Bounded by being
  opt-in; documented as an obligation where the declaration is described; the
  bundled panes that declare it are audited as part of this change, and
  `65_search` deliberately does not.
- **Cheaper frames become more frames, not less CPU** → known and unchanged
  from ADR-P15: repaints are output-driven, so part of the saving is spent
  painting more often. The saving is real but it is bounded by
  `MIN_FRAME_INTERVAL`; this change is measured on **CPU per frame** as well as
  on total CPU, so the win is not hidden by the rate moving.
- **The epoch is coarse** → a source that moves every frame (terminal metadata
  under a printing agent) would invalidate every pure pane and cost the whole
  benefit. If that shows up in measurement, the answer is to split that source
  out of the epoch rather than to add dependency tracking.
- **A cached tree outliving a reload** → the cache is dropped whenever the host
  is rebuilt, which is the same moment the plugins themselves are replaced.

## Migration Plan

No data, config or on-disk format changes, so there is nothing to migrate and
nothing to roll forward. Rollback is reverting the commit; an interface that
declared `pure` keeps loading afterwards, because an unknown declaration key is
ignored.

The two halves are independently landable and independently measurable, and
should land in that order: the change-signals plus gated publish first (no
contract change at all), then the purity declaration on top of them.
