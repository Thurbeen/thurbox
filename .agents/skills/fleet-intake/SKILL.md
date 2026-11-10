---
name: fleet-intake
description: Use when handing authorized work to Fleet.
---

# Create Fleet resources

Read README.md and the current resource contracts before creating work.
Inspect the installed binary with `fleet version --json` and `fleet --help`.
Use the config and data directory the requester supplies; never probe a live
queue or create a fresh operational deployment by guessing a path.

Represent one session as a Task with `harness`, `host`, optional `model`,
`brief`, `workspace` (a list of absolute URLs the runtime makes for the
Task: a `git+` URL for a worktree of a repository, `file://` for an existing
directory as it is), optional `outputs` and explicit `dispatch`. The
[Task resource](../../../docs/fleet/resources/task.md) documents every field. Apply
it with `fleet apply -f FILE`. Record its name and returned generation.
Inspect it with `fleet get task NAME` and `fleet status task NAME --json`.
Check supported kinds with `fleet version --json`; do not invent unsupported
fields. To add to a brief later, use `fleet amend TASK`; a different brief is
a new Task. Do not push, merge or activate work unless the task authorizes it.
