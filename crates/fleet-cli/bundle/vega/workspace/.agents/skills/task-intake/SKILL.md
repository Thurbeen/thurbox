---
name: task-intake
description: Dispatch every substantive requested deliverable as a bounded Task that reports back into Mission Control's inbox. Load whenever a user requests work or asks Mission Control to start a worker.
license: MIT
---

# Create a Task

VEGA is dispatch-only: every substantive requested deliverable belongs to a
worker Task, however short, read-only or urgent. A request without a repository
still gets one owner, a concrete result and an isolated workspace, which is a
`dir` only when the user explicitly asks for it. Acknowledgement, clarification,
dispatch itself and routine status need no Task.

This skill needs an engine that reports `version` 2.7.3 or later; check
`fleet version --json` once per session. An engine before 2.6.0 accepts the
parent labels below but relays nothing, so tell the user it must be upgraded
and report results only when asked. Commands
assume `FLEET_CONFIG` holds Mission Control's configuration; use
`fleet --config PATH` when inheritance is uncertain, never a guessed store, and
require that exact path for every Fleet operation in each child brief. Read
`fleet --help` or a subcommand's help only when a command is refused.

## Prepare

Confirm the repository when one exists, the intended result, the permissions
and the checks. Read only the repository instructions needed to choose and brief
the harness. Choose a unique lowercase DNS-like Task name.

Record the user's ordering and limits on the Tasks, not in your memory:

- Serial work: create every Task in the lane now. Dispatch the first; create
  each later one with `dispatch` false and label `vega.fleet.dev/after` naming
  its predecessor. Parallel lanes are Tasks without `after`. Skipped work gets
  no Task.
- Limits such as `draft-only no-merge`: label `vega.fleet.dev/limits` and state
  them in the brief.
- A decision only the user can make before the work starts: create the Task
  with `dispatch` false and label `vega.fleet.dev/needs`, then ask.
- Label `vega.fleet.dev/request` with the request in a few words.

status-digest shows these labels and how to change them.

Give each Task its own workspace: a list of absolute URLs, one per
repository or directory the worker needs.

- A `git+` URL for every repository, for example
  `git+https://github.com/OWNER/REPO.git#main`. The host's runtime owns the
  checkout: it makes a worktree of the repository for this Task alone, on a
  branch named after the session, and removes it when the Task is deleted.
  Never clone, add a worktree or prepare a checkout yourself. No mapping or
  config edit is needed: the URL is the whole request.
  - `#REF` is optional: the base of the worktree, such as `#main`. The
    runtime fetches the repository before it makes the worktree.
  - `?branch=NAME` names the worktree's branch, made from the base when it
    does not exist. Leave it out unless the user names a branch.
  - A local repository with no remote is `git+file:///absolute/path`.
  - The first URL is where the worker starts; the others are extra
    directories it spans. Say in the brief which repository each part of the
    work belongs to.
  - Deleting the Task removes its worktrees. Changes their branch does not
    hold are first committed onto a preserved ref, and branches are kept.
    Brief workers to push, declare or record anything that must survive
    before they record their result.
  - A URL whose scheme the host's runtime does not handle leaves the Task
    Pending with `NeedsAttention`/`WorkspaceUnsupported`. Ask the user and
    wait.
- `file:///PATH` only when the user explicitly asks for that exception, for
  example for work that has no repository: an existing absolute directory on
  the target host, outside Fleet's data directory and outside this
  workspace. The runtime uses it as it is and isolates nothing, so never
  share one between live Tasks. Put only the Task's inputs and outputs in
  it; the worker makes any checkout it needs. Without that request, ask the
  user for a repository or a directory exception.

Write the manifest with a JSON serializer to `dispatch/NAME.json` in this
workspace, replacing every example value, and never echo the brief in a reply.
The file is only input to `fleet apply`; Fleet keeps the Task, so the file is
safe to delete. The two `gateway.fleet.dev/parent` labels make Fleet relay the
worker's Questions, declared changes and result into VEGA's own inbox, which is
how Mission Control hears back without waiting. Replace `VEGA_UID` with:

```sh
fleet get task vega | jq -r .metadata.uid
```

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "Task",
  "metadata": {
    "name": "inspect-project",
    "labels": {
      "gateway.fleet.dev/parent": "vega",
      "gateway.fleet.dev/parent-uid": "VEGA_UID",
      "vega.fleet.dev/request": "inspect the project",
      "vega.fleet.dev/limits": "no-publish"
    }
  },
  "spec": {
    "harness": "codex",
    "host": "local",
    "brief": "Inspect the repository instructions and report the requested finding. Run its required checks. Do not publish without authorization. Ask Mission Control by declaring a Question and wait for the answer in your inbox. Record the result through fleet result using the generation of the delivered brief.",
    "workspace": ["git+https://forge.example/acme/app.git#main"],
    "dispatch": false
  }
}
```

Every brief names the exact Fleet config path, the concrete result, the checks,
the limits, and how to ask: `fleet declare NAME --json
'{"kind":"Question","id":"q1","text":"..."}'`, then wait for the answer in the
inbox. For a reported issue, the brief requires a failing end-to-end reproducer
first, and an implementation brief requires the repository's tests and
publishing gate.

## Forge worker brief

When opening a change is authorized, put these rules in the brief:
open every change as a draft; run `fleet declare TASK --change-request URL`
immediately after opening it; end every forge post with `<!-- fleet:worker <task uid> -->`
on its own line, the uid from `fleet get task TASK`. Name the repository's
publishing skill, or the installed publish skill if it has none. A Task does not
authorize publication, merging or deployment, and workers do not merge.

A pull or merge request deliverable is done only once the real change URL is
declared and the base, checks, CI and review required by the user or repository
for that delivery stage pass; a local commit or a blocked required gate is not
completion. A draft-only deliverable may conclude after its declared URL and
draft-stage gates while Fleet keeps following the change. Do not impose
publication on Tasks whose requested result does not include it. Declaring
concludes nothing; feedback reaches the worker's inbox, and Fleet follows only
declared changes.

## Optional execution defaults

Keep the simple Task above unless reusable defaults are useful. TaskConfig and
EnvProvider are passive JSON resources, applied into the same store, separate
from native TOML. There is no `[taskConfig]` or `[envProvider]` TOML section.
Apply a provider, then its config, then the Task, for example:

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "EnvProvider",
  "metadata": {"name": "project-env"},
  "spec": {"data": {"PROJECT_MODE": "review"}}
}
```

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "TaskConfig",
  "metadata": {"name": "reviewer"},
  "spec": {
    "harness": "codex",
    "envFrom": [{"name": "project-env"}],
    "env": {"PROJECT_MODE": "careful"}
  }
}
```

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "Task",
  "metadata": {"name": "configured-review"},
  "spec": {
    "configRef": {"name": "reviewer"},
    "env": {"PROJECT_MODE": "focused"},
    "brief": "Review the repository instructions and report findings. Do not publish. Record your result with the delivered brief generation.",
    "workspace": ["git+https://forge.example/acme/app.git#main"],
    "dispatch": false
  }
}
```

TaskConfig supports harness, host, model, ordered `envFrom` and string `env`;
explicit Task fields override it. No runtime passes a model to its harness
yet, so leave `model` unset. EnvProvider supports string `data` and ordered file
or command `sources`, read on the manager. Resolution freezes privately before
launch, so later edits affect only future launches. Literal values are visible
in resource reads and snapshots are plaintext: keep credentials in external
private sources. Read Fleet's TaskConfig and EnvProvider resource documentation
for sources, precedence and limits before writing anything beyond these examples.

## Apply and dispatch

1. Check `fleet get task NAME` first. If it exists, inspect it; never reuse a
   name for a different request. A failed read is not evidence that the Task is
   absent unless it explicitly reports not found.
2. Run `fleet apply -f dispatch/NAME.json --json` to create the suspended Task.
3. When its prerequisites are met, the work is authorized and it is not held,
   set only `spec.dispatch` to `true` in that file and apply it again.
4. Reply at once with the `Dispatched:` line. Never wait for the spawn, poll
   `fleet list` or read the result in the same turn: the running `fleet up`
   reconciles it and Fleet delivers the worker's messages into your session.
   Run `fleet reconcile` only when the user asks for it while `fleet up` is
   down, in the harness's background facility.

If the engine, authentication, resources or another prerequisite blocks dispatch,
report that exact blocker and create a bounded recovery Task when possible. If no
dispatch is possible, say so plainly. Never take over the requested assignment
inline or claim that it started.

Only `dispatch`, `forge` and labels change on an existing Task. A new request
needs a new Task. Do not supply status, uid or generation on create. Setting
dispatch false does not stop an already spawned session. Do not delete another
worker's Task as a retry strategy or treat an ambiguous runtime probe as
permission to spawn a replacement. A held Task that never spawned may be deleted
and recreated when its brief must change.
