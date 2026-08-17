## Context

See `proposal.md`. The command bus from `v2-plugin-kernel` already runs work off
the render path and publishes what is in flight; this change adds the commands
that need it most, because creation is the slowest thing thurbox does.

`session_ops::spawn::spawn_session_headless` already performs the whole
pipeline — repo resolution, worktree creation, multi-repo workspaces, agent
launch — and is used by `thurbox-cli`. It is reused unchanged.

## Goals / Non-Goals

**Goals:**

- Creation, fork, sync and restore as non-blocking commands.
- Progress renderable from acceptance to appearance, including a placeholder
  row where the session will land.
- The creation flow as a bundled plugin over choices the kernel exposes.

**Non-Goals:**

- Multi-repo creation from the flow UI. The command carries it (the headless
  path already supports `--add-repo`), but the bundled flow offers one repo;
  a plugin can offer more without a kernel change.
- Branch *listing* from a remote. Reading refs is a read, and reads are served
  from a snapshot — so the branch list arrives via the snapshot's repo entry
  rather than being fetched inside the flow.

## Decisions

### D1 — Creation phases are published, not inferred

The command bus publishes a phase string per in-flight command. Creation
publishes the phases the pipeline actually passes through (resolving, fetching,
creating the worktree, readying the backend, spawning) rather than a spinner,
because v1 learned that a flow which can run for tens of seconds needs to say
*which* part is slow — a stalled fetch and a stalled ssh connect look identical
otherwise.

*Alternative considered.* A generic "working" phase. Rejected: it is exactly the
information the user wants when creation is slow, and the pipeline already knows
it.

### D2 — The placeholder is a row the session list draws, not a kernel concept

v1 needed `PendingSpawn` plus a bespoke slot in the ordering code. Here the
in-flight command already carries the repository it concerns, so the session
list groups it into that repo like any other row. The kernel adds nothing.

*Consequence.* A replacement session list gets pending rows for free if it reads
`thurbox.commands`, and loses nothing else if it does not.

### D3 — Choices are reads, actions are commands

Repositories, agents and hosts are published in the snapshot; picking among them
is plugin state; committing is a command. That keeps the flow plugin free of
blocking calls even though every choice it offers came from disk or config.

## Risks / Trade-offs

**Creation touches more failure modes than any other command.** A bad path, a
branch that exists, an unreachable host, a missing agent binary. → Every one
surfaces through the same in-flight error channel rather than a bespoke path,
and the flow renders it in place.

**A worktree left behind by a failed creation.** → `spawn_session_headless`
already cleans up on failure; this change does not add a second cleanup path.

**Sync is destructive-adjacent.** A rebase or reset can lose work. → Sync
refuses when the worktree has changes that would be lost, and reports why,
rather than asking the user to be careful.

## Findings from implementing

**D1 did not survive contact.** The design said creation phases would be
published "as the pipeline passes them". `spawn_session_headless` is one opaque
call — it resolves the repo, fetches, checks out a worktree, readies the backend
and launches the agent without reporting between stages. Publishing real phases
means threading a progress channel through `session_ops`, which `thurbox-cli`
shares, so it is a change to the headless path rather than something to bolt on
here. The bus publishes queued/running/failed, which is honest but coarser than
promised. Recorded against task 1.3 rather than quietly redefining what "phase"
meant.

**The pending row needed one field, not a subsystem.** v1 carried a whole
`PendingSpawn` type plus a bespoke slot in its ordering code, because the
placeholder had to be positioned before the session existed. Here the in-flight
command carries a `subject` — the repo name — and the session list groups it
like any other row, including bringing its own header when the repo has none
yet. The kernel gained one optional string.

**Restore is where "best effort" has to be a decision, not a discovery.** A
force-deleted session lost its worktree directory, so restoring reattaches
committed work and nothing else. The command refuses unless the caller says it
knows, and the restore surface marks the lossy rows *before* the choice. That
mirrors what v1 landed on after the fact, and is why the flag is on the command
rather than inferred from the row.

**Sync did not need the guard the spec implied.** The spec said sync must not
proceed when changes would be lost. `git::sync_worktree` already stashes,
rebases and pops — and on conflict aborts and restores the stash — so work is
never lost and pre-refusing a dirty worktree would have been strictly worse than
v1. What is reported instead is that the sync *stopped* and nothing moved.
