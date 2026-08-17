## Why

`v2-plugin-kernel` lands a command bus that can delete, restore, restart,
reorder and send — but not **create**. So v2 can manage sessions it inherits and
cannot make one, which is the single largest gap between it and v1. Fork and
worktree sync are the same shape: they need the spawn pipeline, not just a row
update.

## What Changes

- The command bus gains `create` and `fork`, both non-blocking: the repo, branch,
  worktree and agent work runs off the render path and progress is readable.
- In-flight creation is renderable end to end — v1 needed `PendingSpawn` and a
  placeholder row precisely because this takes tens of seconds on a large repo.
- The **new-session flow** becomes a floating plugin: repo picker, branch
  selection, agent picker, host picker.
- **Worktree sync** and **restore-deleted** become commands with plugin surfaces.
- Multi-repo sessions (the symlink workspace) are reachable from the flow.

## Capabilities

### New Capabilities

- `session-creation`: what creating, forking and syncing a session does, how
  progress and failure are observable, and what a plugin must be able to render
  while it is happening.

### Modified Capabilities

None yet. `plugin-host-api` has no archived main spec to delta against — it is
still a delta in `v2-plugin-kernel` — so the command surface this change adds is
specified under `session-creation` and folds in at archive time.

## Impact

Depends on `v2-plugin-kernel`. Touches the command bus and adds bundled plugins;
reuses `session_ops::spawn` unchanged, which is where the repo/branch/worktree
logic already lives. Blocks `v2-retire-v1`: without creation, v2 cannot replace v1.
