## Why

Seven of the fifteen bundled plugins hand-draw their own borders with a cell
buffer. The same three helpers — `new_cells`, `place_text`, `cells_to_spans` —
are copy-pasted into **six** of them; `border_style` and `title_style` into
four; `visible_window` and `truncate_ellipsis` into four. Roughly a hundred
lines per pane, reimplemented each time.

They do it because the kernel's `Frame` cannot express what v1's panes look
like:

```rust
pub struct Frame {
    pub title: Option<String>,   // plain, unstyled, left-aligned
    pub borders: Borders,
    pub border_style: Style,
    pub style: Style,
    pub padding: u16,
}
```

A plain `String` title cannot be the focused badge (inverted text on the
accent), cannot be right-aligned as the agent pane's is, and leaves no way to
paint the things v1 puts **on** the border: the session list's status dots, the
centre pane's tab strip and collapse chevron, the `▲ N` / `▼ N` overflow counts,
the scrollbar column. So every pane that wants to look native drops out of the
node vocabulary and paints cells itself.

This is one fault with two costs.

**Modularity.** The extension point is worse than it looks. A plugin author who
wants a pane that matches the others must write a cell-buffer renderer, not
compose widgets — and if they get it subtly wrong, their pane looks foreign.
`widgets.panel` exists and is unusable for anything but the plainest frame,
which is why six plugins bypass it.

**Performance.** Cost tracks node count at roughly 9 us per node, and the
cell-buffer shape inflates it: a row drawn as a box with a border node either
side is four nodes, where the same row inside two full-height border *columns*
is one. Measured on a synthetic 30-row pane, borders-as-columns renders in
**0.80 ms against 2.08 ms — 2.6x faster** for identical output. At 60 rows it is
1.9x. The session list costs 1.2 ms with *zero sessions in it*, which is the
same finding from the other end.

## What Changes

- **`Frame` gains what the panes actually need**: a title of styled spans with
  an alignment, and border *overlays* — content placed on the top or bottom
  border, left- or right-anchored. The kernel draws it, in Rust, once.
- **A shared `lib/chrome.lua`** for what remains genuinely in userland, so the
  six copies of the cell-buffer helpers collapse to one implementation that is
  tested once.
- **The bundled panes stop hand-drawing.** Each drops its private chrome and
  composes the frame instead, which is also what takes their row shape from four
  nodes to one.
- **Closed modals stop being rendered.** Every floating plugin is drawn
  full-screen each frame purely to discover it is not floating — about 1.3 ms,
  a quarter of a frame at 150x30.

## Capabilities

### New Capabilities

- **Expressive frames**: styled, aligned titles and border overlays, so a pane
  gets v1's chrome without leaving the node vocabulary.
- **Shared chrome helpers**: one tested implementation of the measuring and
  clipping every pane was repeating.

### Unchanged, deliberately

- **The four node kinds.** `Frame` is a property of a node, not a fifth kind.
  This change makes the existing vocabulary sufficient; it does not extend it.

## Non-Goals

- Rewriting the panes' appearance. The output should be identical — this is a
  change of who draws it, not what is drawn, and the parity recordings are the
  check on that.
