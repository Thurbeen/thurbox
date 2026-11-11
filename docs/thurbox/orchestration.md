# Orchestration

Fleet is thurbox's engine (ADR-41), so orchestration is Fleet's. A Task is
one unit of work. Fleet's Task controller runs one session per Task, and a
worker reports its outcome with `fleet result`. The design is in
[Fleet's documentation](../fleet/README.md). This page says what thurbox
adds on top.

## Every Task is a session in the list

The interface follows `fleet watch`. A Task that a script, an agent or an
automation creates shows up in the session list within a tick. A Task that
names a parent with its `gateway.fleet.dev/parent` label nests under that
parent, so a lead's workers sit under the lead. The link is informational:
deleting a lead deletes none of its workers.

## Driving sessions from a script

A session's agent can run `fleet` like any other command, so "start a
session per crate and send each one its part" needs no script of its own.
The verbs:

| To | Run |
| --- | --- |
| list sessions | `fleet list task` |
| read one, as JSON | `fleet get task <name>` |
| create one | `fleet apply --create-only -f <task.json>` |
| type into its agent | `fleet send <task> <text>` |
| follow every change | `fleet watch --json` |
| delete one | `fleet delete task <name>` |

A Task's workspace is a list of URLs, and the runtime of its host makes them
([runtime workspaces](../runtime/README.md#workspaces)). The new-session
flow's workspace step chooses the URL for a git repository on this machine:

| Choice | URL |
| --- | --- |
| New worktree, new branch from the default branch | `git+file:///path/to/repo?branch=<session>#<default branch>` |
| New worktree, new branch from another base | `git+file:///path/to/repo?branch=<session>#<base>` |
| Worktree on an existing branch | `git+file:///path/to/repo?branch=<branch>` |
| No worktree | `file:///path/to/repo` |

No mapping is needed: the URL is the whole request. A directory that is no
git repository, such as the installed interface, skips the step and gets a
`file://` URL. So does a session on another Fleet host, because the flow
reads branches on this machine only. A Task you write yourself can name any
URL its host's runtime lists, several of them too: the first is where the
agent starts.

## Read the state, do not infer it

A session's state is Fleet's `status.sessionState`, which the agent's own
hooks report through `fleet signal`. Wait for a state with a bound on the
wait, and read the Task again when the wait ends: a timeout is not evidence
that the agent finished. A worker's outcome is its `status.result`, written
by `fleet result`, never the state of its terminal.

## Automations

Fleet has no schedule kind. A recurring send or spawn is a thurbox
automation (`thurbox-cli automation`), which fires through `fleet send` and
`fleet apply` ([features.md](features.md#automations)).
