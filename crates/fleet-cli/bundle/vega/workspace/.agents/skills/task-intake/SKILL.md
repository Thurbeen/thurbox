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

Give each Task its own workspace, in exactly one of two forms:

- `{"repositories": [{"name", "url", "ref"}]}` for every Task. The runtime
  owns the checkout: it materializes the repositories for this Task alone, on a
  branch it names, and cleans them up when the Task's session is deleted. Never
  clone, add a worktree or prepare a checkout yourself, and never name a branch
  in the spec; Fleet has no branch, base, worktree or cleanup field.
  - Use the exact URL mapped in the Mission Control config's
    `runtime.repositories`, for example the canonical
    `https://github.com/OWNER/REPO`; Fleet matches it byte for byte, with no
    `.git` or SSH normalization. An unmapped URL leaves the Task Pending with
    `NeedsAttention`/`RepositoryUnmapped`, without spending a spawn attempt, so
    add the mapping yourself, never a checkout or a directory in its place:
    1. Find an existing source checkout on the target host whose `origin` is that
       URL (`git -C PATH remote get-url origin`) and which has no branch named
       after `runtime.keyPrefix`. If there is none, several, or the mapping needs
       anything else, ask the user and wait.
    2. Write a candidate copy of the config that adds
       `"URL" = "/absolute/source/path"` under `[runtime.repositories]` and
       changes nothing else. Check it against a new empty scratch directory
       outside Fleet's data, which opens the runtime the way `fleet up` does
       without touching the live store:
       `fleet --config CANDIDATE --data-dir EMPTY_DIR reconcile --max-passes 1 --json`.
       Only when it exits 0, keep the old config as a backup and put the
       candidate in its place; then remove the scratch directory. A
       `fleet list` passing proves nothing: it does not open the runtime.
    3. A running `fleet up` re-reads `runtime.repositories` before each sweep,
       so the mapping applies without a restart and a waiting Task then spawns.
       An invalid edit leaves the last valid mapping in force. Other config
       settings need a restart: leave those to the user, and never stop the
       engine yourself.
  - `ref` is optional and opaque to Fleet. Thurbox uses it as the base of a
    worktree created in the mapped source checkout, and it does not fetch, so
    name a remote-tracking ref such as `origin/main` and have the worker confirm
    its base against the forge before relying on it.
  - The first repository is the primary. With one repository the worker starts
    in its worktree; with several, Thurbox starts it in a directory holding one
    entry per repository, named after each source checkout's directory. Say in
    the brief which repository each part of the work belongs to.
  - The runtime's cleanup applies when the Task is deleted: with Thurbox's
    default `delete` it removes its worktrees, its branch and any unpushed
    commits. Brief workers to push, declare or record anything that must
    survive before they record their result.
- `{"dir": PATH}` only when the user explicitly asks for that exception, for
  example for work that has no repository: an existing absolute directory on the
  target host, outside Fleet's data directory and outside this workspace.
  Fleet neither creates nor isolates it, so never share one between live Tasks.
  Put only the Task's inputs and outputs in it; the worker makes any checkout it
  needs. Without that request, ask the user for a repository or a `dir`
  exception.
  Existing `dir` Tasks finish in place; their specs are immutable, so never
  rewrite or move them.

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
    "workspace": {"repositories": [
      {"name": "app", "url": "https://forge.example/acme/app", "ref": "origin/main"}
    ]},
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
    "workspace": {"repositories": [
      {"name": "app", "url": "https://forge.example/acme/app", "ref": "origin/main"}
    ]},
    "dispatch": false
  }
}
```

TaskConfig supports harness, host, model, ordered `envFrom` and string `env`;
explicit Task fields override it, and model selection needs a matching
`runtime.modelEnv` mapping. EnvProvider supports string `data` and ordered file
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
