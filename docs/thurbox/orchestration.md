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
flow gives each repository it ticks one URL, the first where the agent
starts:

| Choice on the repo step | URL |
| --- | --- |
| A worktree (`Alt+W`), the first one | `git+file:///path/to/repo?branch=<branch>#<base branch>` |
| A worktree, any other | `git+file:///path/to/repo?branch=<branch>` |
| Ticked as it is | `file:///path/to/repo` |
| An existing worktree (`→`, then `Enter`) | `file:///path/to/that/worktree` |

Every worktree is on the one branch the flow names. A branch that exists is
checked out; a new one starts from the base branch for the first repository
and from its own default for the rest. No mapping is needed: the URL is the
whole request. A directory that is no git repository, such as the installed
interface, is refused a worktree and gets a `file://` URL. So does a session
on another Fleet host, because the flow reads branches on this machine only.
A Task you write yourself can name any URL its host's runtime lists.

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
