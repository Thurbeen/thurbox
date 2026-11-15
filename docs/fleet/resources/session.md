# Session

## What it is for

A Session is one running agent: the unit that the runtime port starts,
observes, briefs, sends to and releases. It is not a stored kind. A
[Task](task.md) names its Session in `status.session`, and reports what the
runtime proved about it in `status.sessionState`. The source is
[fleet-api's session module](../../../crates/fleet-api/src/session.rs).

## When you write one

Never. TaskController starts a Session when it spawns a Task, and releases
it when the Task is deleted. You read it in the Task's status.

## Example

The part of a Task's status that describes its Session:

```json
{
  "session": {"id": "fake-1", "host": "local", "key": "design-305.u1-a864456b5b3dfedb"},
  "sessionState": "Running",
  "agentState": "working",
  "activity": {"source": "hook", "at": 1730000400000}
}
```

## Spec

None. A Session has no spec: the Task's spec says what to run.

## Status

TaskController is the only writer of `status.session`, `status.sessionState`,
`status.agentState` and `status.activity`.

- One Task owns at most one Session, from the Task's spawn to its release.
- The key is `<task name>.<task uid>`. A second spawn finds the first
  Session under that key, and does not start a twin.
- A Task recreated under the same name has a new uid, so it never adopts the
  old Session.
- A Session that the runtime holds but Fleet did not spawn belongs to no
  Task.

### Identity

`status.session` is a `SessionRef`, written once the spawn's receipt or a
`find` under the key names the Session.

| Field | Meaning |
| --- | --- |
| `id` | The runtime's own identifier for the Session. |
| `host` | The place the Session runs on. |
| `key` | Fleet's deterministic key, `<name>.<uid>`. The exec adapter gives the runtime `<keyPrefix>.<uid>`. |

## Lifecycle

`status.sessionState` is what Fleet last proved. Each state is one Known
answer of the runtime port, or, for the agent's activity, of the agent's own
hooks through `fleet signal` ([signals](../controllers/task.md#signals)).
Unknown answers prove nothing and keep the last state. Nothing in Fleet
decides on the state; a view reads it. Activity is read only of a session
that may be paused, or while a send waits for its prompt proof.

| State | Meaning | Proved by | Exec adapter |
| --- | --- | --- | --- |
| `Gone` | A readable session list no longer shows the Session under its key, or shows another Session there. | `find` is `Known(None)`, or names a different Session. | `inspect` answers `none` for the key. |
| `Lost` | The runtime holds the Session, but its agent's window is gone, as after a host reboot. | `lost` is `Known(true)`. | `inspect` answers `exited` for the key. |
| `Paused` | The agent is stopped, by Fleet or by anyone else. The row, workspace and conversation are kept. | `paused` is `Known(true)`. | `inspect` answers `paused` for the key, on a runtime whose manifest declares `pause`. On any other runtime, `paused` is Unknown. |
| `Idle` | The agent waits for input. | Its hooks last reported `idle`. | The same: the adapter reports no activity. |
| `Running` | The runtime holds the Session and nothing proves its agent stopped, lost or idle. | Its hooks last reported `working`, `prompt` or `blocked`, or any proof of presence: `find` names the Session, `lost` or `paused` is `Known(false)`. | `inspect` answers `running` for the key. |

The strongest proof wins when one pass answers several probes: `Gone`
over `Lost`, `Lost` over `Paused`, `Paused` over the agent's activity.

The state is absent until a pass proved something. A pass that proves the
same state again writes nothing.

### Agent state

`status.agentState` is what the agent is doing, finer than
`sessionState`. The agent's hooks set it, through `fleet signal`. Below
them, a screen rule reads it off the session's screen
([agent state](../contracts.md#agent-state)). `status.activity.source` says
which: `hook`, `screen` or `none`.

| `agentState` | The agent's last report | A screen rule's state |
| --- | --- | --- |
| `working` | `working` or `prompt`: a turn runs. | `working` |
| `attention` | `blocked`: it waits on a person, for a permission or for input. | `blocked` |
| `idle` | `idle`: the turn finished. | `idle` |
| `unknown` | Nothing since it last started, and no rule matched. Not idle: a silent agent may be at work. | — |

A signal file that cannot be read keeps the last state, unless a screen
rule matches. The state is absent while the Task has no session. Nothing
decides on a screen state.

### State and phase

`status.phase` says where the Task is in its own lifecycle. `sessionState`
says what the runtime proved about the agent. The two move independently.

| `phase` | `sessionState` | Reads as |
| --- | --- | --- |
| `Briefed` | `Running` | The worker works on the brief. |
| `Briefed` | `Idle` | The worker waits; with `pauseAfterMs` set, a pause is due after that time. |
| `Briefed` | `Paused` | Fleet or a person stopped the agent; the next inbox entry resumes it. |
| `Briefed` | `Lost` | The window is gone; the next pass revives the Session. See [after a host reboot](../controllers/task.md#after-a-host-reboot). |
| `Briefed` | `Gone` | The row is gone; `WorkerGone` and `NeedsAttention` are raised, nothing respawns. |
| `Concluded` | `Running` or `Idle` | The worker reported; the Session is retained until the Task is deleted. |
| `Releasing` | `Gone` | The release landed; the finalizer can go. |

The conditions `SessionLost` and `WorkerGone` are the controller's decisions
on the same observations. `sessionState` is the observation itself.

## Related kinds

- A [Task](task.md) owns at most one Session.
- The [runtime contract](../contracts.md#runtime-contract) states what a
  runtime guarantees about a Session.

## Commands

```sh
fleet status task design-305           # one line: session <id> on <host> <state>
fleet status task design-305 --json    # .session and .sessionState
fleet get task design-305              # the whole record
```

```text
Task/design-305  phase Briefed
generation 1 observed 1 (current)
  Spawned          True     Spawned
  Briefed          True     ReceiptWritten
session fake-1 on local Running
```

A view that lists sessions, such as thurbox, reads every Task's `session`
and `sessionState` through `fleet watch`, `fleet list task` or `fleet status`.
