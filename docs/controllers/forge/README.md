# Forge controller

ForgeController is a second controller over the [Task](../../resources/task.md).
It is not a kind of its own and creates nothing. It reads the
[ChangeRequests](../../resources/change-request.md) a Task's worker declared,
keeps the worker informed about each by relaying what is new into the Task's
inbox, and takes a draft change out of draft when a configured ready policy
says it is ready. It owns one status subtree, `status.forge`, the
way the worker owns `status.result`; TaskController's fields are untouched,
and so is every ChangeRequest.

Following, the relay and the mark-ready effect are served by the installed
binary.

| Read | Answers |
| --- | --- |
| [Task](../../resources/task.md#following-declared-changes) | What does a worker declare, and what do `spec.forge` and `status.forge` hold? |
| [ChangeRequest](../../resources/change-request.md) | What does the snapshot this controller reads contain, and who deletes it? |
| [State machine](state-machine.md) | How does each declared change move from Following to Settled, and what is relayed when? |
| [Interface](interface.md) | What are the probes, the one effect, the ready policies and the write rules? |
| [Task controller](../task/README.md) | How is the Task itself run, and how is its inbox delivered? |
| [ChangeRequest controller](../change-request/README.md) | Who mirrors the forge? |
| [Native config](../../configuration/fleet-toml.md#the-forge-section) | Where do defaults, hosts and ready policies come from? |
| [Overall architecture](../../ARCHITECTURE.md) | Where does this controller fit in Fleet? |

## One run

1. A person applies a Task, with or without a `spec.forge` block. Nothing
   forge-related exists yet and nothing needs to.
2. The worker decides it needs a change, pushes, opens it as a draft with the
   forge's CLI and runs `fleet declare <task> --change-request <url>`. The
   verb ensures one ChangeRequest exists for that URL and records the
   declaration on the Task. The standing worker policy tells every worker to
   do this; a Task applied with no thought of a change declares one the same
   way.
3. On the next sweep ForgeController sees the declaration, adds an entry for
   that ChangeRequest to `status.forge.changes`, and sends the worker a
   first inbox message holding the whole current snapshot: state, head,
   checks, the forge's mergeability answer and the retained notes. The
   entry's cursor starts at the snapshot's revision.
4. On every later sweep it reads the Task and each ChangeRequest named in
   its entries. Items and transitions with `seq` above the entry's cursor,
   filtered, become one inbox entry; the inbox append and the cursor advance
   are one write to the Task. TaskController delivers the entry to the ready
   session under its key, journaled, as it delivered the brief.
5. The worker replies on the forge with the forge's CLI, ending each post
   with the worker mark carrying its Task's identity, so its own words are
   never relayed back to it. Another Fleet worker's marked posts are relayed,
   labelled as a worker's.
6. When the ready policy holds on a draft change that has never been seen
   ready, the worker has reported, the session is at rest and the mirror is
   fresh, ForgeController plans `MarkReady` once: the intent is written on
   the entry, the manager performs it through the Forge port, which re-reads
   the head and the draft state first, and the receipt lands on the entry.
   The next ChangeRequest revision shows the change ready.
7. The worker reports with `fleet result`. Notes that arrive afterwards are
   still relayed: a Concluded Task with a nonempty inbox is due again.
8. The ChangeRequest settles when the change merges or stays closed past its
   grace. The entry is marked settled and read no more. Deleting the Task
   deletes nothing else; the ChangeRequest is garbage-collected by its own
   controller after its retention.

The diff is one integer per entry. The ChangeRequest numbers every item and
transition it first sees with its `revision`; the entry's `cursor` is what
this controller relayed through. A second Task that declares the same URL,
on another host or after the first one is gone, gets its own entry, its own
join message and its own cursor on its own object.

## Every write is local

The ChangeRequest controller writes only ChangeRequests. ForgeController
writes only `status.forge` and the Task's inbox, and reads ChangeRequests by
name. Nothing pushes into another object, so a relay needs no effect, intent
or receipt: it is a status write that either lands or is retried from fresh
state. The only effect this controller performs is the mark-ready effect.

TaskController owns `status.phase`, `conditions`, `session`, the spawn,
brief and release records, and `status.deliveries`. The worker owns
`status.result` and `status.declared`. ForgeController owns `status.forge`.
`status.inbox` is touched only through typed store mutations:
`record_relay` by ForgeController and GatewayController, `append_inbox` by
`fleet send`,
`amend_task` by `fleet amend`, `plugin_mutate` by a plugin's append or
amend, and `record_delivery` by TaskController.
Each writer names itself and the store refuses a write to a field another
manager owns. This is the arrangement Kubernetes uses when several
controllers reconcile one object, a Node or a PersistentVolumeClaim, and the
API server enforces field ownership.

## Scope

In scope: following declared changes, relaying notes, transitions and
checks, taking a draft out of draft under a named policy. Out of scope,
deliberately: merging. The forge's own mergeability answer is reported on
the ChangeRequest for a person or a conductor; Fleet never merges.

## Implementation and evidence

Four steps, each its own change with its own E2E suite against the built CLI
and the fake or stub forges:

| Step | What | Where | Evidence |
| --- | --- | --- | --- |
| 1 | `fleet-forge` and the [ChangeRequest controller](../change-request/README.md), with retention | `crates/fleet-forge`, `fleet-controllers/src/change_request.rs` | `change_request_controller`, `e2e_forge` |
| 2 | The Task inbox, `fleet send`, and inbox delivery in TaskController | `fleet-api/src/inbox.rs`, `fleet-store` typed mutations, `fleet-controllers/src/task.rs` | `store_contract`, `task_controller`, `e2e_inbox` |
| 3 | `fleet declare`, `spec.forge`, `status.declared`, `status.forge`, and ForgeController with the join message and the relay | `fleet-api/src/follow.rs`, `fleet-controllers/src/forge.rs`, `fleet-manager` (`declare`, the second controller's sweep), `fleet-store` (`record_relay`) | `forge_controller`, `e2e_forge_controller`, `store_contract`, `architecture` |
| 4 | The mark-ready effect and the shipped ready policies | `fleet-api/src/follow.rs` (the policies and the key), `fleet-forge` (`mark_ready` in each adapter), `fleet-controllers/src/forge.rs` (the decision), `fleet-manager` (the effect and its receipt), `fleet-cli` (the `[forge]` keys, `fleet init`, the apply check) | `forge_controller`, `e2e_mark_ready`, `architecture` |

Each step keeps `cargo xtask check` green and adds its crate edges, status
writers and restricted capabilities to `crates/architecture.toml` with
negative fixtures. All four are served by the installed binary.
