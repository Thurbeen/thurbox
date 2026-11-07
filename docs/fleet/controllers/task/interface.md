# Task controller interface

The controller is a pure decision function. The manager owns I/O, persistence
and process execution. The [state machine](state-machine.md) defines how the
values below change the Task; the [resource](../../resources/task.md) defines the external
JSON fields.

## Controller API

Source: [TaskController](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-controllers/src/task.rs) and
[plan types](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-api/src/plan.rs).

```rust
pub struct TaskInput<'a> {
    pub task: &'a Object,
    pub observations: Observations,
    pub now: Millis,
}

pub fn probes(&self, input: &TaskInput<'_>) -> Result<Vec<Probe>, StoreError>;
pub fn reconcile(&self, input: &TaskInput<'_>) -> Result<Plan, StoreError>;
```

The two methods belong to `TaskController`. `task` is the stored object,
including metadata, spec and status. `now` is Unix time in milliseconds,
supplied by the manager's `Clock`. Missing status decodes to the default
Pending state; malformed typed spec or status returns `StoreError::Invalid`.
Neither method reads the clock, disk, environment or runtime itself.

The manager starts with empty observations, asks `probes`, performs those
probes through `Runtime`, then calls `reconcile` with their answers. It skips
Tasks with a future `status.notBefore` before asking for any probe. It also
skips a Concluded Task whose inbox is empty, unless one of these holds:

- The Task is being deleted.
- Its session recheck is due: `status.sessionCheckedAt` is absent or 5
  minutes old.
- Its session may be paused, or a pause or resume waits for its outcome.

### Requested probes

| Task state | Probes |
| --- | --- |
| Pending, `dispatch: false` | None. |
| Pending, `dispatch: true` | `Host { host }`; `FindWorkspace { host, key }` while a materialize has no Known receipt. |
| Spawning | `Find { host, key }`. |
| Spawned | `Find`; `Lost`, `WorkspaceTrust` and `Ready` for the recorded session; `BriefDelivered` when a brief intent exists without a known receipt, else the inbox probes below. |
| Briefed, no worker result for `status.brief.generation` | `Find`; `Lost`; `Paused`; the delivery probes below; `Activity` when the session may be paused. |
| Concluded, an inbox entry to deliver | `Find`; `Lost`; `Paused`; the delivery probes below. |
| Concluded, session recheck due | `Find`. |
| Concluded, session may be paused or a pause or resume in flight | `Find` when the recheck is due; `Lost`; `Paused`; `Activity` when the session may be paused. |
| Concluded otherwise, or Stalled | None. |
| Deleting, regardless of phase | `Find`; `AtRest` when a session is recorded; `FindWorkspace` while a materialize has no Known receipt. |

The inbox probes, for the oldest entry not given up on: `Ready` for the
recorded session, and `BriefDelivered` under the entry's key when it was
sent without a known receipt. The delivery probes are the same probes for
the catch-up note while one is owed, and the inbox probes otherwise. A
session may be paused when the conditions in
[pausing an idle session](state-machine.md#pausing-an-idle-session) hold,
apart from its activity.

`Observations` holds `host`, `find`, `ready`, `workspace_trust`,
`brief_delivered`, `at_rest`, `lost`, `paused`, `activity` and
`workspace`. The manager
answers `Activity` with Unknown on a runtime that cannot pause.
Each is optional: `None` means no answer was supplied, not a negative answer.
Each supplied answer is an [Observation](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-api/src/observation.rs):

| Value | Meaning | Safe inference |
| --- | --- | --- |
| `Known(true)` / `Known(false)` | A probe answered yes / no. | Use that specific answer for that specific probe. |
| `Known(Some(SessionRef))` | A readable lookup found a session under the key. | Adopt it while spawning; afterward compare it with recorded identity. |
| `Known(None)` from `find` | A readable lookup proved the key absent. | Handle absence; do not confuse it with an unreadable lookup. |
| `Unknown { why }` | A timeout, unreadable answer or ambiguous outcome. | Retain uncertainty and report why; never infer absence or success. |

### Returned plan

```rust
pub struct Plan {
    pub status: Option<serde_json::Value>,
    pub effects: Vec<Effect>,
    pub remove_finalizer: Option<String>,
    pub delete: bool,                 // other controllers; TaskController leaves it false
    pub delivery: Option<DeliveryReceipt>,
    pub appends: Vec<InboxEntry>,     // other controllers; TaskController leaves it empty
    pub creates: Vec<Create>,         // other controllers; TaskController leaves it empty
}
```

| Field | Contract |
| --- | --- |
| `status` | Desired typed Task status serialized as JSON, including every new effect intent; `None` means unchanged. The worker-owned `result` and the unowned `inbox` are passed back as read. |
| `effects` | Ordered effects, each with a deterministic key and `Action`. The controller does not execute them. |
| `remove_finalizer` | Request to remove `fleet.dev/release` after the status and effects; present only for a deleting Task whose release is established. |
| `delivery` | An inbox entry proven delivered: `{key, at, recovered, deliveredVia, mayHold}`. `deliveredVia` is the runtime's route, when the send named one. `mayHold` is true when that route can hold the text behind an approval dialog; it raises no attention. The manager writes it with `status` as one record-and-remove mutation: the receipt into `status.deliveries`, the entry out of `status.inbox`. |

TaskController plans the actions `Spawn(SpawnSpec)`, `Brief { session,
text }`, `Release { session }`, `Revive { session }`, `Pause { session }`
and `Resume { session }`. SpawnSpec carries `host`, `key`, `harness`,
`workspace` (the Task's `dir` or `repositories`), optional `model`, the
private environment map and `environmentFrozen`. The
manager injects frozen values and prevents model-variable remapping before
performing a spawn; environment values are not persisted in effect intents.
`SessionRef` carries `id`, `host` and `key`; a later session
reusing a key does not inherit readiness or at-rest proof for the recorded id.

## Manager execution protocol

Source: [Manager](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-manager/src/lib.rs).

1. Acquire the manager lease and list Tasks. A list that is Unknown is not
   treated as an empty list.
2. For each due Task, renew the lease. For a dispatched Task with reusable
   configuration/environment, resolve or recover its private launch snapshot
   and persist its public identity before runtime probes/effects. Request and
   perform probes, renewing before each probe, then call the pure controller.
3. CAS-write planned status against the resourceVersion read, through the
   record-and-remove mutation when the plan carries a delivery. This
   persists intents before effects. A conflict drops this plan; no effect
   runs from it.
4. Renew before each effect, perform it, then persist its receipt. On a
   receipt conflict, reread current status and retry that receipt write so a
   concurrent worker result is preserved.
5. If requested, remove the finalizer with CAS and the current lease. The
   store removes the deleting record when no finalizers remain.
6. Repeat passes while changes or conflicts occur, up to `max_passes`, then
   release the lease when still owned. An idle report means no change in the
   last pass; it is not proof that every Task concluded.

`Manager::run(RunOptions)` returns `Result<RunReport, StoreError>`.
`RunOptions` holds `max_passes` and `lease_ttl`. The report carries counts,
`idle`, per-Task `errors` and `unknowns`. Lease loss ends the run; other
per-Task errors are reported rather than counted as successful progress.

`Manager::serve(RunOptions, wait)` retains one lease across bounded sweeps.
The edge supplies the wait/shutdown callback; controller scheduling still uses
persisted `notBefore`. `fleet up` supplies native signal handling and polling,
prints changed errors/unknowns, and stops on lease loss. A normal stop releases
the lease; a crashed process leaves an expiring lease. Neither stop deletes
Tasks or sessions. Bootstrap Task creation uses the manager's `ensure_task`
verb, retaining an existing Task's immutable inputs and identity.

The CLI's default lease is at least 30 seconds and twice the longest
operation bound of the configured ports (runtime, forge, gateway, plugins).
An explicitly supplied lease must exceed that bound. A file-store
lease fences store writes and checks before effects; it cannot cancel an
external action already started. Recovery relies on stable runtime keys.

## Runtime port

Source: [Runtime](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-api/src/runtime.rs). Every call acts on
one Session, defined in [session](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-api/src/session.rs):
the agent a Task owns, named by `SessionRef`. All session operations
return `Observation<T>`; a refused effect is
`Known(EffectOutcome::Failed { why })`, distinct from an Unknown outcome.

| Method | Input | Answer |
| --- | --- | --- |
| `operation_bound` | None | Worst-case duration of one session operation, including timeout and cleanup grace, in milliseconds. |
| `prepare_execution` | Resolved execution | Freeze the runtime model-variable mapping into its private environment, or Unknown. |
| `env_source` | Explicit file/command source | Bounded manager-local JSON string map, or redacted Unknown. |
| `host` | Host name | Whether the runtime can reach it. |
| `spawn` | `SpawnSpec`, with `ready` when Fleet made the workspace | `Spawned { session, adopted }`, `Unmapped { url }` for a repository the runtime has no source for, `TrustUnmapped { harness }` for a harness it knows no workspace-trust interface for, or Failed/Unknown. |
| `find` | Host and session key | Optional session under that key, or Unknown. |
| `ready` | `SessionRef` | Whether the session's agent can take a message now. A working turn can take one; a dialog that holds the agent cannot. |
| `workspace_trust` | `SessionRef`, the Task's workspace and the `ready` workspace Fleet made, if any | `Trusted { how }`, `Untrusted { gap, why }` with gap `Unsupported` or `Mismatch`, or Unknown. |
| `brief` | Session, text and intent key | `Delivered { already, via, may_hold }`, `Queued { why }`, or Failed/Unknown. `via` names the runtime's route. `may_hold` is true when the route can hold the text behind an approval dialog. `Queued` means the runtime kept the text where its agent does not read it. |
| `brief_delivered` | Session and intent key | Whether that brief was delivered, or Unknown. |
| `at_rest` | `SessionRef` | Whether it is safe to release that recorded session, or Unknown. |
| `release` | `SessionRef` | `Released` if deleted or proven absent, or Failed/Unknown. Repeating a completed release is a no-op. |
| `lost` | `SessionRef` | Known true only when the runtime holds the session and proves its agent's window gone, and nobody stopped it. Known false for a running or stopped session. |
| `revive` | `SessionRef` | `Revived` after the runtime started the lost session's agent again in its own workspace and conversation, under the same identity, or Failed/Unknown. A running session is left alone, so a repeat is harmless. |
| `can_pause` | None | Whether the runtime can pause and resume a session. False by default. Fleet never pauses on a runtime that cannot. |
| `activity` | `SessionRef` | `Active` or `Idle`, or Unknown. Known `Idle` only when the runtime proves that the agent waits with no background work. Unknown never pauses. |
| `paused` | `SessionRef` | Known true when the runtime holds the session with its agent stopped, by Fleet or anyone else. Known false for a running or lost session. |
| `pause` | `SessionRef` | `Paused` after the runtime stopped the agent and kept its row, workspace and conversation, or Failed/Unknown. A stopped session is left alone. |
| `resume` | `SessionRef` | `Resumed` after the runtime started the stopped agent again in its own workspace and conversation, under the same identity, or Failed/Unknown. A running session is left alone, so a repeat is harmless. |
| `create_dir` | Host and directory | `Ok(true)` when made, `Ok(false)` when it existed, `Err` with why, or Unknown. Used for a conversation Task's thread directory. |
| `remove_dir` | Host and directory | `Ok(())` when removed or absent, `Err` with why, or Unknown. |

The thurbox adapter executes bounded `thurbox-cli` calls via `fleet-exec`.
Its session key is `<keyPrefix>.<uid>`. It journals each brief intent in
session metadata as pending before sending and delivered afterward. A pending
journal entry is ambiguous, so it is never a license to resend. A text over
16 000 bytes is a Known failure, refused before the journal and the send.
The adapter gives every text to the agent's own inbox with `thurbox-cli
message send --kind fleet`. It never types into the terminal composer.
Thurbox chooses the route and reports it as `delivered_via`:
`claude-socket` and `codex-queue` reach a working turn. Thurbox answers
`mailbox`, with exit 0 and a `delivery_note`, when it cannot reach the
agent. Thurbox 2.81 answers `mailbox` for these sessions:

- a session on a remote host,
- a Codex session that no hook has bound to its conversation yet,
- a Claude Code session whose inbox socket failed,
- a session of any other harness.

The adapter returns a `mailbox` answer as `Queued` with the note, and
journals the key as queued. A queued key is never sent again. A failed
`message send` can still have queued the text, so the adapter never
repeats it. A refused call's reason is the first
line of its stderr, else of its stdout, where `thurbox-cli` prints runtime
errors. On release, a different id holding the key returns Unknown and is
not deleted.
`lost` reads `session get`: `hook_corroboration` `missing` is lost. Any
window that is there, even a dead one, is not lost. `revive` runs
`session restart --if-missing`. A session with no window is at rest.
`paused` reads `stopped` and `activity` from `session get`: either one
saying stopped is Known true. `activity` reads the `activity` field:
`active`, `idle`, `unknown` or `stopped`. Only `idle` is Idle. A missing
field, `unknown` or `stopped` is Unknown. The adapter never derives Idle
from `state` or `hook_state`: they read `done` while background shells,
Monitors or crons are still pending. `pause` runs `session stop` and
`resume` runs `session start`. Both first check that the key still belongs
to the recorded id, as `release` does. A stopped session is at rest.

No released thurbox reports the `activity` field yet:
[Thurbeen/thurbox#1665](https://github.com/Thurbeen/thurbox/pull/1665)
adds it. Thurbox 2.83.1, the latest release, does not have it. Without the
field, `activity` is Unknown, and Fleet never pauses a session.
The fake adapter provides persisted sessions and faults without starting an
agent. Explicit provider commands still run through the same bounded executor
with either adapter; command resolution requires a lease above 20000 ms and
renews it per source. Both implement the same port; neither is called from
pure code.

### Workspace port

Source: [Workspaces](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-api/src/workspace.rs). The manager
holds this port only when the config has a `[workspaces]` section. Without
it, `TaskInput.materialize` is false and the controller plans no workspace
effect. The [workspace contract](../../CONTRACTS.md#workspace-contract)
states the adapter rules.

| Method | Input | Answer |
| --- | --- | --- |
| `operation_bound` | None | Worst-case duration of one call, in milliseconds. The lease must outlast it. |
| `materialize` | Host, workspace key and the Task's repositories | `Materialized { workspace, adopted }`, `Unmapped { url }`, or Failed/Unknown. |
| `find` | Host and workspace key | The whole workspace under the key, `Known(None)` when none is whole, or Unknown. |
| `clean` | `WorkspaceRef` | `Deleted`, also for a workspace already gone, or Failed/Unknown. |

The controller asks `FindWorkspace` only while a materialize has no Known
receipt. The manager performs `Materialize` and `CleanWorkspace` as it
performs a spawn: intent first, then the effect, then the receipt.

### Workspace trust

Workspace trust is an agent's own gate on a folder's project settings. It is
not a permission policy, and it approves no tool action. The controller sees
only the port's answer and knows no harness.

- `Trusted` lets the brief proceed to the readiness check.
- `Untrusted` holds the brief and asks a person, with the adapter's `why`.
- Unknown holds the brief. It asks nobody, because it proves nothing.

The thurbox adapter reads the harness from the session row's `agent`. It
takes the harness's interface from `runtime.workspaceTrust`. The
[runtime contract](../../CONTRACTS.md#workspace-trust) lists each
interface and what it proves. The fake adapter answers from each session's
`trusted` flag and its `workspaceTrust` fault.

## Store port and write ownership

Source: [Store](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-api/src/store.rs) and
[worker verbs](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-manager/src/verbs.rs).

| Writer | May change | Required fencing |
| --- | --- | --- |
| `apply` | New served resources, labels and permitted spec changes; request deletion. Never `spec.amendments`. | Store validation, exclusive creation or resourceVersion CAS for spec updates. |
| `worker` | `status.result` through `record_result` | Current resourceVersion, `status.brief.generation`, no amendment waiting, and the output types declared up to that generation. |
| `cli` | Appends to `status.inbox` through `send`. Appends to `spec.amendments` and `status.inbox` together through `amend_task`. | Current resourceVersion. No lease: the CLI calls a manager verb. |
| `plugin/<name>` | Appends to `spec.amendments` and `status.inbox` together through a plan's `amends[]`, with `amend_task`'s checks. | The Task's `uid`, `ifGeneration`, the plugin's `tasks` scope and live manager lease. |
| `forge-controller` | `status.forge`; appends to `status.inbox`, both through `record_relay` | Current resourceVersion and live manager lease. |
| `task-controller` | Other Task status, `status.deliveries` and the release finalizer; removes delivered `inbox` entries | Current resourceVersion and live manager lease. |

The worker's `status.declared` is written through `declare`, with the same
fencing as `record_result`. `status.inbox` has no owner. It is reached only
through three typed mutations, `append_inbox`, `record_relay` and
`record_delivery`, each named by its caller (`kinds::may_append_inbox`,
`kinds::may_record_relay`, `kinds::may_record_delivery`), with the rules the
store enforces: no duplicate key across `inbox` and `deliveries`, no
`about.revision` at or below the mirror's highest delivered one, no append
while more than 20 entries are still to deliver (entries given up on do not
count) or the Task is deleting, and no
other field of the Task changed by any of them. A plain `update_status` that
changes `inbox` is refused whoever writes it.

The amend mutation is the only write to `spec.amendments` and the only write
that queues an entry with `amends`. `fleet amend` reaches it through
`amend_task`, and a plugin through its plan's `amends[]` verb. In one CAS
write it appends the amendment, increments `metadata.generation` to the
amendment's generation and queues the entry under
`task/<name>/<uid>/amend/<generation>`, with the inbox's checks. It
refuses a deleting or Stalled Task. `update_spec`, and so `apply` and a
plugin patch, refuses any change to `spec.amendments`, and a create refuses a
Task that carries any.

`Store` supplies `get`, `list`, `create`, `update_spec`, `update_status`,
`append_inbox`, `amend_task`, `record_relay`, `record_delivery`, `delete`, `delete_fenced`,
`remove_finalizer`, validation and lease acquire/renew/release.
Private `launch`/`freeze_launch` methods recover or exclusively freeze launch
data under Task UID, separately from resources. Freezing checks Task UID,
resourceVersion and manager lease; final Task deletion removes the snapshot.
Reads return Known/Unknown; writes return values or `StoreError`. The store
checks ownership, CAS and lease tokens at the write boundary, including when
a holder name is reused with a newer token. Lease-free worker writes do not
permit controller fields to change.

## Intent, receipt and recovery keys

| Item | Stable identity / stored fields |
| --- | --- |
| Session key | `<name>.<uid>`; the uid prevents a recreated Task adopting an earlier Task's session. |
| Spawn effect | `task/<name>/<uid>/spawn/<attempt>`. |
| Brief effect | `task/<name>/<uid>/brief`, reused for every permitted send of that one brief. |
| Inbox entry | Its own `key`, reused for every permitted send of it: `task/<name>/<uid>/send/<n>` for `fleet send`, `n` the resourceVersion the append was made against; `task/<name>/<uid>/amend/<generation>` for `fleet amend`; `task/<name>/<uid>/relay/<change uid>/<revision>` for a forge relay; `task/<name>/<uid>/plugin/<plugin>/<key>` for a plugin's append; `task/<name>/<uid>/gateway/<conversation uid>/<revision>` for a chat relay; `task/<name>/<uid>/gateway/child/<child uid>/<item>` for a child's words. |
| Release effect | `task/<name>/<uid>/release/<attempt>`. |
| Intent | Action name, timestamp and generation, stored under the effect key. The brief's receipt records the generation of its intent. |
| Receipt | Action name, timestamp and `Observation<EffectOutcome>`, stored under the effect key. |

An intent without a receipt means the action may have happened. Spawn looks
up its session key, brief and inbox delivery read the delivery journal under
their keys, and release confirms absence or retries after the persisted wait.
Once an inbox entry's receipt is recorded in `status.deliveries`, its
intent, effect receipt and attempt record are removed with it, so a Task's
status stays bounded however many messages it is sent. The
[state machine](state-machine.md#retries-and-recovery) defines each decision.

## The second controller over the Task

The [Forge controller](../forge/interface.md) extends this
interface without changing what exists:

- ForgeController has its own due gate (`ForgeController::due`: a
  declaration without an entry, or a following entry whose `notBefore` is
  absent or due) and runs after this controller on each sweep, from a fresh
  read of the Task, owning `status.forge` and appending to `status.inbox`.
  Its plan names no other Task field and adds no finalizer; its one effect,
  `MarkReady`, carries its intent and receipt on the entry, apart from this
  controller's.
- `Probe::Object { kind, name }` is answered by the manager from the store
  before any runtime or forge probe, and `Probe::Whoami { host }` through
  the `Forge` port, cached for the sweep, as `Probe::Change { change }` is for
  the [ChangeRequest controller](../change-request/interface.md).
  `AtRest` is reused by ForgeController's `workerIdle` predicate. The
  plan's `delete` field, by which that controller retires its own object,
  `creates`, the Tasks ConversationController creates, and `appends`, the
  inbox entries ForgeController lands with its status,
  exist.
- Field managers `ChangeRequestController` and `ForgeController` exist, and
  `Worker` owns `status.declared` as it owns `status.result`.
