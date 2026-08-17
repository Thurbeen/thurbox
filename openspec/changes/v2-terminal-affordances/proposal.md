## Why

A set of small v1 behaviours live in the code `v2-retire-v1` deletes, and each
would be missed: dragging to select and copy, Ctrl+clicking a URL, desktop
notifications when an agent needs you, the shell pane, and the perf HUD. None is
a pane; together they are the difference between a terminal you can use and one
you can only look at.

## What Changes

- **Mouse**: drag-selection inside a terminal surface, and clicks resolved to the
  node under them via node identity — replacing v1's hand-built per-frame
  click-target registry rather than reproducing it.
- **Clickable URLs**, including OSC 8 rich-text links and the copy-instead-of-open
  fallback on a host with no browser.
- **OS notifications** on the block edge, reusing `src/notifications.rs` unchanged.
- **Shell pane** as a second contributor to the centre slot.
- **Perf HUD** as a plugin over published counters.

## Capabilities

### New Capabilities

- `terminal-affordances`: selection, copy, link activation and notification
  behaviour around a live terminal.

### Modified Capabilities

- `view-tree`: node identity gains event targeting.
- `plugin-host-api`: clipboard and link-opening as commands, keeping plugins
  free of process access.

## Impact

Depends on `v2-plugin-kernel`. `notifications`, `clipboard` and `ui::links`'
detection logic carry over. Blocks `v2-retire-v1`.
