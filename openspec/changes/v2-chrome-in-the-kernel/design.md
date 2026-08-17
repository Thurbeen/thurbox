# Design

## D1 — Why this is a kernel change and not a library

The obvious move is a `lib/chrome.lua` that the six plugins share, leaving the
kernel alone. It fixes the duplication and none of the cost: the helpers would
still build a table per cell and hand back a tree of the same shape, so the
frame would stay at ~9 us per node with the node count unchanged.

Drawing the frame in the kernel is what removes the nodes, because the border
stops being *content*. A pane becomes its rows; the border, title, overlays and
scrollbar are drawn around them in Rust from data the node already carries.

The library is still worth having for what remains in userland — measuring,
clipping, windowing — but it is the smaller half of the change.

## D2 — What `Frame` grows, and what it does not

```
title:        Vec<Run>        -- styled spans, not a String
title_align:  Left | Right
overlays:     Vec<Overlay>    -- { edge: Top|Bottom, anchor: Left|Right, runs }
```

That is the whole addition, and it is chosen to cover exactly the things the
bundled panes leave the vocabulary for today:

| What a pane does now | What it becomes |
|---|---|
| session list's status dots | a top overlay, right-anchored |
| `▲ N` / `▼ N` overflow counts | top and bottom overlays, right-anchored |
| centre pane's tab strip and chevron | a top overlay, left-anchored |
| agent pane's right-aligned title | `title_align = Right` |
| focused title badge | styled `title` runs |

**Not added: a scrollbar field.** It looks like it belongs, but a scrollbar
needs the content length and the viewport offset, which are properties of the
pane's *state* rather than its frame — putting them here would make `Frame` a
place where panes stash state. `widgets` keeps drawing it into the border column
it already reserves.

**Not added: arbitrary cell painting.** The point is to make the escape hatch
unnecessary, not to move it into the kernel. A pane that needs something these
four cannot express should be a reason to look at the list again, not a reason
to add a fifth.

## D3 — Overlays are clipped, never wrapped

An overlay is drawn onto a border row, so it has exactly one row and a hard
width. If it does not fit it is clipped from the anchored end and the corner is
never overwritten — v1's rule, and the reason `fit_right_title` exists in the
agent pane today. Wrapping would push it off the border entirely.

The corner glyphs are drawn last for the same reason. A pane that overruns loses
its overlay, not its frame.

## D4 — The rendering order this forces

Border, then overlays, then content, then the scrollbar column. It matters that
overlays come before content: an overlay is *on* the border, so a pane whose
content is flush against the frame must not paint over it. v1 gets this right by
painting the block first and the overlays into the block's own rect.

## D5 — Migration is per-pane and verifiable

Each pane converts independently, and the parity recordings decide whether it
worked: the output must be cell-identical before and after, since nothing about
the appearance is meant to change. That makes this safe to land incrementally
rather than as one rewrite — a pane that has not converted yet still draws its
own chrome and still looks right.

Order by payoff, which is row count: the session list and the file viewer first
(both windowed lists in a tall pane), then the task and automation panes, then
the two detail panes, then the agent pane last because its chrome is the most
elaborate and the least repeated.
