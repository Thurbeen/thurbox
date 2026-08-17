## Why

Code review is v1's largest single surface — 1,844 lines of rendering plus 2,610
of state — and the prior v2 attempt could not express it as a view tree at all.
Its own tests record why: it windows a body by character count against a resolved
width, so side-by-side, wrapping, horizontal scrolling and syntax colouring are
all geometry-first.

That makes it the first real consumer of the `surface` primitive beyond the
terminal, and the test of whether design.md D3 holds.

## What Changes

- The review view becomes a plugin that produces **cells**, painted as a
  plugin-fed surface — not a tree.
- The changed-files list stays an ordinary tree-based pane beside it.
- Diffs are produced by the kernel off the render path and read from the
  snapshot; comments and marks keep their existing storage.
- Unified and side-by-side layouts, find-in-diff, and retargeting all live in
  the plugin.

## Capabilities

### New Capabilities

- `code-review-pane`: what the review surface shows, how it is navigated, and
  how comments are recorded and exported.

### Modified Capabilities

- `view-tree`: confirms — or corrects — that a plugin-fed surface can carry a
  pane this dense.

## Impact

Depends on `v2-plugin-kernel`. `session::review` and `storage::review` carry over
unchanged. Blocks `v2-retire-v1`.
