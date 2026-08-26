# plugin-events Specification

## Purpose
Lets a plugin be told that something changed — a session appeared, changed
status, was focused; a command finished; another plugin said something — instead
of rediscovering it by diffing the snapshot on every render, and bounds what a
handler can cost the interface.
## Requirements
### Requirement: A plugin subscribes by declaring a handler

A plugin SHALL be able to declare a single `on_event(name, payload)` function
and be called with each event it subscribes to. Subscription is declared as
data (`events = { "session.status", "focus.session", … }`) so it is listable,
lintable and visible in the inventory; a plugin declaring a handler with no
`events` list receives nothing.

#### Scenario: A declared subscription fires

- **WHEN** a plugin declares `events = { "session.status" }` with an `on_event`
  handler, and a session's status changes
- **THEN** the handler is called once with `"session.status"` and a payload
  naming the session and both statuses

#### Scenario: An undeclared event does not fire

- **WHEN** a plugin declares `events = { "session.status" }` and a session is
  created
- **THEN** its handler is not called

#### Scenario: A subscription to a name the kernel does not emit

- **WHEN** a plugin declares an event name that is neither a kernel event nor
  of the `user.` form
- **THEN** the plugin fails to load with an error naming the unknown event,
  reported where load errors are already reported (the Interface tab, the log,
  and `thurbox-cli plugin check`'s non-zero exit)

### Requirement: The kernel emits a fixed, documented set of events

The events the kernel emits SHALL be a closed enumeration, each with a
documented payload, and that enumeration SHALL be readable from the running
interface (the help modal) and from a terminal (`thurbox-cli plugin events`).
At minimum:

| Event | Fired when | Payload |
|---|---|---|
| `session.created` | a row appears in the snapshot | `session` (id), `name`, `agent`, `repo` |
| `session.deleted` | a row leaves the snapshot | `session`, `name` |
| `session.status` | a row's derived status changes | `session`, `from`, `to` |
| `session.changed` | a row's name, branch, repo set or parent changes | `session`, `fields` (list) |
| `session.post_create` / `post_delete` / `post_restart` / `post_restore` | an operation *this* interface performed completed | the same facts the lifecycle hook receives on stdin |
| `focus.session` | the selected session changes | `from`, `to` (ids, either absent) |
| `focus.pane` | focus moves to another plugin | `from`, `to` (plugin names) |
| `command.done` / `command.failed` | a command a plugin issued reaches a terminal phase | `kind`, `session`, `subject`, `error` |
| `interface.reloaded` | the interface was rebuilt from disk | `reason` (`f10`, `watch`, `settings`) |

#### Scenario: A session created by another process

- **WHEN** `thurbox-cli session create` adds a session while the interface runs
- **THEN** subscribers receive `session.created` on the iteration that
  publishes the new row, and no `session.post_create`

#### Scenario: A session created by this interface

- **WHEN** the creation flow creates a session
- **THEN** subscribers receive `session.post_create` when the operation
  completes and `session.created` when the row is published, in that order or
  the other, and each exactly once

#### Scenario: A status change derived on the tick

- **WHEN** a `working` session goes quiet past the output-quiescence window
  and its published status becomes `Idle`
- **THEN** `session.status` fires with `from = "working"`, `to = "idle"`, the
  same way it would for a hook-reported change

#### Scenario: The enumeration is listed

- **WHEN** the help modal's events page or `thurbox-cli plugin events` is read
- **THEN** every emitted event name appears with its payload fields, and no
  name appears that the kernel does not emit

### Requirement: Events are delivered once, in order, off the render path

Each event SHALL be delivered to each subscriber exactly once, in the order the
kernel observed the changes, on the iteration after the change was published
and before that iteration's input is dispatched. A handler SHALL NOT run
during render, during another handler, or during input dispatch.

#### Scenario: Several changes in one iteration

- **WHEN** three sessions change status between two iterations
- **THEN** each subscriber receives three `session.status` events, in the
  order the rows are published, before any key pressed in that iteration is
  dispatched

#### Scenario: A handler is not re-run per frame

- **WHEN** nothing changes for many frames after an event
- **THEN** no handler runs again for it

#### Scenario: A reload

- **WHEN** the interface is reloaded from disk
- **THEN** events observed before the reload but not yet delivered are dropped,
  and `interface.reloaded` is the first event the rebuilt plugins receive

### Requirement: A handler is bounded and its failure is contained

A handler SHALL run under the same instruction and memory budget as a render.
A handler that throws or overruns SHALL cost only itself: the event is still
delivered to every other subscriber, the plugin's pane still renders, and the
failure is reported where a render failure is reported (the log and the
plugin's error state), once per event rather than per frame.

#### Scenario: One subscriber throws

- **WHEN** two plugins subscribe to `session.created` and the first handler
  throws
- **THEN** the second handler still runs, the first plugin's pane still
  renders on the next frame, and the error is reported against the first
  plugin naming the event

#### Scenario: A handler loops

- **WHEN** a handler exceeds the instruction budget
- **THEN** it is interrupted, reported, and the loop continues in the same
  iteration

### Requirement: A handler can only do what a render can do

A handler SHALL receive the same environment a render receives — the published
tables, `state`, `store`, `command` — and nothing more. It cannot block, wait,
or return a value the kernel acts on; its effects are the commands it enqueues
and the state it writes.

#### Scenario: A handler focuses a session

- **WHEN** a handler for `session.status` with `to = "blocked"` calls
  `command("focus", { session = payload.session })`
- **THEN** the command is queued and takes effect as any command does

#### Scenario: A handler's return value

- **WHEN** a handler returns a value
- **THEN** it is ignored

### Requirement: A plugin can emit an event to other plugins

A plugin SHALL be able to emit a user event, `command("emit", { text = name,
… })`, delivered as `user.<name>` with the remaining fields as the payload and
the emitting plugin's name as `payload.source`, to every plugin subscribed to
that name — including the emitter — on the next iteration. Kernel event names
cannot be emitted by a plugin.

#### Scenario: A user event is delivered

- **WHEN** plugin `a` emits `command("emit", { text = "refresh", scope = "x" })`
  and plugin `b` declares `events = { "user.refresh" }`
- **THEN** `b`'s handler runs next iteration with `"user.refresh"` and a
  payload `{ scope = "x", source = "a" }`

#### Scenario: A plugin emits a kernel name

- **WHEN** a plugin emits `session.created`
- **THEN** the emit is refused and reported as a command error, and no
  subscriber receives it

#### Scenario: Emits cannot cascade without bound

- **WHEN** a handler emits an event whose handler emits again, indefinitely
- **THEN** delivery stops at a fixed depth per iteration, the remainder is
  dropped and reported once, and the loop keeps its frame cadence

### Requirement: An unknown subscription is refused before the interface runs

The event names a plugin may subscribe to SHALL be checked when the plugin is
loaded — by the interface and by `thurbox-cli plugin check`, which loads it the
same way — so a subscription to a name the kernel does not emit fails there,
with the name in the message, rather than as a handler that never fires. The
`emit` command needs no new global: it is a kind of `command`, which the lint
contract already declares.

#### Scenario: A typo in a subscription

- **WHEN** a plugin declares `events = { "sesion.status" }`
- **THEN** `thurbox-cli plugin check` exits non-zero naming the event, and the
  interface refuses to load the plugin with the same message
