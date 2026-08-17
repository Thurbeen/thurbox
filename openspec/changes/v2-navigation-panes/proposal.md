## Why

The info panel, file viewer and global search are how you find your way around
thurbox. Search is also the change that finally forces the cross-plugin
decoration question `v2-plugin-kernel` deliberately deferred (design.md D6):
it restyles rows in three panes it does not own.

## What Changes

- **Info panel** and **file viewer** become plugins, with file contents supplied
  by the kernel as a read (the plugin gets no filesystem access).
- **Global search** becomes a plugin spanning sessions, tasks, automations and
  files, with live in-place highlighting.
- **D6 is resolved**: decoration lands either as a userland tree-walk over node
  identity or as a promoted selector engine — decided against a working system
  and a real consumer, as planned.

## Capabilities

### New Capabilities

- `navigation-panes`: what these surfaces show and how a result is activated.
- `decoration`: how a plugin restyles nodes another plugin rendered.

### Modified Capabilities

- `view-tree`: node identity gains whatever matching D6 settles on.

## Impact

Depends on `v2-plugin-kernel` and `v2-workflow-panes` (search covers tasks and
automations). Blocks `v2-retire-v1`.
