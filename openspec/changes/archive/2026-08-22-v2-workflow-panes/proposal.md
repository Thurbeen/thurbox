## Why

Tasks and automations are how thurbox schedules and tracks work. Both are fully
functional headlessly via `thurbox-cli` — only their TUI surfaces live in the
code `v2-retire-v1` will delete, so they must exist as plugins before v1 can go.

## What Changes

- The **tasks panel** becomes a plugin: list, status cycling, the full-screen
  preview with markdown rendering, the editor, and the trigger-time action picker.
- The **automations pane** becomes a plugin, together with its central-pane
  editor and run history — the first real second contributor to the centre slot,
  which is what finally exercises `switch` mode and focus-claim.
- Snapshot reads grow tasks and automations; commands grow their mutations.

## Capabilities

### New Capabilities

- `workflow-panes`: what the task and automation surfaces show, how they are
  edited, and how triggering one reaches an agent.

### Modified Capabilities

- `plugin-host-api`: reads and commands for tasks and automations.
- `ui-composition`: `switch` mode and focus-claim gain a second occupant, which
  is the first genuine test of both.

## Impact

Depends on `v2-plugin-kernel`. Markdown rendering must move to userland or
become a surface. Blocks `v2-retire-v1`.
