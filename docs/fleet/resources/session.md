# Session

A Session is one running agent. It is the unit the runtime port starts,
observes, briefs, sends to and releases. A Session is not a stored kind: a
Task names its Session in `status.session` and reports what the runtime
proved about it in `status.sessionState`. The source is
[fleet-api's session module](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-api/src/session.rs); the
guarantees are in [the runtime contract](../contracts.md#runtime-contract).

```text
Task ──owns one, spawn to release──▶ Session
                                       ├── identity  SessionRef {id, host, key}
                                       └── lifecycle SessionState, proved by the runtime port
```

## Ownership

- One Task owns at most one Session, from the Task's spawn to the Task's
  release.
- TaskController is the only writer of `status.session` and
  `status.sessionState`.
- A Session's key is `<task name>.<task uid>`. A second spawn finds the
  first Session under that key instead of starting a twin.
- A Task recreated under the same name has a new uid. It never adopts the
  old Session, because the key carries the uid.
- A Session the runtime holds but Fleet did not spawn belongs to no Task.

## Identity

`status.session` is a `SessionRef`, written once the spawn's receipt or a
`find` under the key names the Session.

| Field | Meaning |
| --- | --- |
| `id` | The runtime's own identifier for the Session. |
| `host` | The runtime host the Session runs on. |
| `key` | Fleet's deterministic key, `<name>.<uid>`. The thurbox adapter stores it as `<keyPrefix>.<uid>`. |

## States

`status.sessionState` is what the runtime last proved. Each state is one
Known answer of the runtime port. Unknown answers prove nothing and keep the
last state. Nothing in Fleet decides on the state; a view reads it.

| State | Meaning | Proved by | Thurbox adapter |
| --- | --- | --- | --- |
| `Gone` | A readable session list no longer shows the Session under its key, or shows another Session there. | `find` is `Known(None)`, or names a different Session. | `session list` without the key. |
| `Lost` | The runtime holds the Session, but its agent's window is gone, as after a host reboot. | `lost` is `Known(true)`. | `session get` reports `hook_corroboration: "missing"` on a session that is not stopped. |
| `Paused` | The agent is stopped, by Fleet or by anyone else. The row, workspace and conversation are kept. | `paused` is `Known(true)`. | `session get` reports `stopped: true` or `activity: "stopped"`. |
| `Idle` | The agent waits for input, with no background work to lose. | `activity` is `Known(Idle)`. | `session get` reports `activity: "idle"`. |
| `Running` | The runtime holds the Session and nothing proves its agent stopped, lost or idle. | `activity` is `Known(Active)`, or any proof of presence: `find` names the Session, `lost` or `paused` is `Known(false)`. | `session get` reports `activity: "active"`, or the row is listed and neither stopped nor missing its window. |

The strongest proof wins when one pass answers several probes: `Gone`
over `Lost`, `Lost` over `Paused`, `Paused` over the agent's activity.

The state is absent until a pass proved something. A pass that proves the
same state again writes nothing.

## State and phase

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

## Reading it

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

A view that lists sessions, such as a TUI, reads every Task's `session`
and `sessionState` through `fleet list task` and `fleet status`.

## Next steps

This definition is the first step toward the
[target architecture](../architecture-target.svg), where
Thurbox is one view and Fleet runs Sessions behind the runtime port. The
second step added a Workspace that Fleet materializes; see
[Workspaces and the workspace port](../architecture.md#workspaces-and-the-workspace-port).
The following steps add an `attach` call on the runtime port for a live
terminal, and a Fleet-owned multiplexer adapter.
See [Sessions and the runtime port](../architecture.md#sessions-and-the-runtime-port).
