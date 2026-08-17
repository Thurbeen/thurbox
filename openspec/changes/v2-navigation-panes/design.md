## Context

See `proposal.md`. This change settles the question `v2-plugin-kernel`
deliberately deferred (its design.md D6): how a plugin restyles nodes another
plugin rendered. Global search is the first real consumer, which is exactly the
condition D6 said to wait for.

## Goals / Non-Goals

**Goals:**

- Session details, file browsing and cross-cutting search as plugins.
- File contents as a *read*, so browsing needs no filesystem capability.
- D6 resolved against a working consumer rather than an imagined one.

**Non-Goals:**

- Fuzzy matching. Substring matching is what v1's search actually did for
  metadata; ranking can come later without changing the mechanism.
- Searching terminal scrollback. v1 scanned each session's vt100 buffer; that
  is a surface, not a tree, and needs a different read.

## Decisions

### D1 — Decoration is a userland tree-walk, not a kernel selector engine

A plugin declares `decorates = "<slot>"` and receives that slot's rendered tree,
returning a modified one. Matching is ordinary Lua over the `id`/`class`/`role`
each node already carries.

*Why this over selectors.* D6 left both doors open and said the promotion
direction is userland → kernel. A selector engine is a matching language,
specificity rules and a resolution pass in Rust; the consumer needs none of it —
it wants "rows whose text contains the query". Building the engine first would
have been the largest instance of the mistake the prior attempt diagnosed.

*What this costs.* Every decorator walks the tree itself, which is O(nodes) per
decorated slot per frame. With one decorator over a session list that is
nothing. If a profile ever says otherwise, `lib/select.lua` becomes the shared
implementation and only then a candidate for Rust.

*What it buys.* Search stops being "a mode, not a pane" — the carve-out the
prior attempt recorded in `tests/global_search_pane_gap.rs` — and becomes an
ordinary plugin.

### D2 — File contents are a read, not a capability

The kernel exposes "entries at a path" and "text of a file", both rooted at the
session's working directory and refusing anything outside it. The plugin draws.

This is the case the prior attempt called the sharpest test of the capability
model, and it went the same way: the file viewer needed contents and an editor
launch, and both stayed kernel-side. Adding a filesystem capability to browse
files would have been the easy wrong answer.

### D3 — Search matches metadata only, for now

Sessions by name, branch and agent; tasks by title and description; automations
by name. v1 also scanned live terminal scrollback, which is a surface rather
than a tree and needs its own read — deliberately out of scope so the mechanism
lands first.

## Risks / Trade-offs

**A decorator sees a tree it did not build.** It can return anything, including
something structurally wrong. → It is a plugin: a malformed tree is reported and
the undecorated tree is drawn, exactly as a broken render is.

**Decoration runs every frame.** → The tree diff already skips unchanged frames,
and a decorator that changes nothing produces an identical tree, so it costs a
walk rather than a repaint.

## Findings from implementing

**D6 is settled, and the userland answer was the right size.** Decoration is
`decorates = "<slot>"` plus a `decorate(tree)` function, matching on the
identity nodes already carry. The whole kernel cost is one optional field, one
call site, and a node→Lua conversion so the tree can cross back. `lib/tree.lua`
is 60 lines. A selector engine would have been a matching language, specificity
rules and a resolution pass in Rust — for a consumer that wants "rows whose text
contains the query".

Search is now an ordinary plugin rather than the permanent carve-out the prior
attempt recorded as "a mode, not a pane".

**The capability tripwire earned itself immediately.** Adding `files` to the
plugin environment failed `the_granted_capability_set_matches_a_declared_list`
until it was added to the list with a comment saying why. That is exactly the
"capabilities are introduced with their consumer" rule enforced rather than
remembered — and the test named the new global without being told about it.

**`Ctrl+/` has three encodings, and folding them belongs in the kernel.** A
kitty-protocol terminal reports it literally; a legacy one sends the raw 0x1F
byte, which surfaces as `ctrl+7` or `ctrl+_`. v1 bound all three in three
places. `canonical_chord` folds them, so a plugin declares one chord and it
works everywhere — the same argument, and the same fix, as the capital-letter
case. **Both were found by running it, not by reading the spec.**

**Node→Lua conversion deliberately drops style.** A decorator receives structure
and identity, not the colours the original chose. Carrying ratatui styles back
would mean a second colour representation on the boundary, for no consumer: a
decorator sets the styles it wants. Worth knowing before someone expects to read
what they are overriding.

**Two things turned out to need kernel work, not plugin work**, and are marked
rather than faked: publishing git working-tree state (it shells out, so it needs
the background-refresh treatment v1 gave it) and *focusing* a pane from a search
result (focus lives in the binary and there is no command for it).
