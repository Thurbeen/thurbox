# Forge controller interface

The controller is a pure decision function over a Task and the ChangeRequests
it follows. The manager owns I/O, persistence and process execution. The
[state machine](state-machine.md) defines how the values below change
`status.forge`; the [Task resource](../../resources/task.md#following-declared-changes)
lists the fields on the Task.

## Controller API

Source: [ForgeController](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-controllers/src/forge.rs) and
[the Task's forge fields](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-api/src/follow.rs).

```rust
pub struct ForgeInput<'a> {
    pub task: &'a Object,
    pub observations: ForgeObservations,
    pub now: Millis,
    pub defaults: ForgeDefaults,
}

pub fn due(task: &Object, now: Millis) -> bool;
pub fn probes(&self, input: &ForgeInput<'_>) -> Result<Vec<Probe>, StoreError>;
pub fn reconcile(&self, input: &ForgeInput<'_>) -> Result<Plan, StoreError>;
```

`task` is the stored Task, including the worker's `status.declared` and this
controller's `status.forge`. Missing `status.forge` decodes to no entries.
`defaults` carries `forge.relayNotes`, `forge.relayChecks`,
`forge.relayBots`, `forge.noCheckGraceMs`, `forge.draftPolicy` and the
`[forge.readyPolicies]` tables from the native config, as values. `due` is the
controller's own gate, which the manager asks before anything else. Neither
method reads the clock, disk, environment, forge or runtime itself.

## Fields this controller reads and owns

`status.declared` is the worker's, one item per `fleet declare`. Each item
is a JSON object with a required string `kind` and nothing else validated,
so other tools can declare their own facts through the same verb. This
controller reads items whose `kind` is `ChangeRequest` and ignores the rest.
The item `fleet declare --change-request` writes:

| Field | Meaning |
| --- | --- |
| `kind` | `ChangeRequest`. |
| `url` | The URL as declared. |
| `name` | The name of the ChangeRequest the verb ensured for it. |

`status.forge` is this controller's:

| Field | Meaning |
| --- | --- |
| `changes` | A map from ChangeRequest name to one entry, below. One per declared change; a change declared twice has one entry. |
| `conditions` | `Following`, `Settled` and `NeedsAttention`, as the [state machine](state-machine.md#conditions) defines them. |

Each entry of `changes`:

| Field | Meaning |
| --- | --- |
| `uid` | The ChangeRequest's `uid` when the entry was made or last rebound. A different `uid` under the same name is a new incarnation. |
| `state` | `following` or `settled`. |
| `cursor` | The ChangeRequest `revision` relayed through. Set to the revision of the join message; advances in the same write as each later relay. |
| `gaps` | How many times a relay found `droppedBefore` above the cursor. |
| `markReady` | `{state, head, attempts, intents, receipts}`; `state` is `none`, `pending`, `done` or `failed`; `head` the head of the last intent; `attempts` the intents written against the change; `intents` and `receipts` by effect key, the mark-ready effect's own, apart from TaskController's. |
| `notBefore` | Earliest next pass for this entry after a failed mark-ready, in Unix milliseconds. Separate from the Task's own `notBefore` and from other entries'. |

A Task that opened a change in each of two repositories:

```json
"status": {
  "declared": [
    {"kind": "ChangeRequest", "url": "https://github.com/OWNER/app/pull/88",
     "name": "cr-1b7d02e4a9c3"},
    {"kind": "ChangeRequest", "url": "https://github.com/OWNER/infra/pull/12",
     "name": "cr-9e40c6f1d2ab"}
  ],
  "forge": {
    "changes": {
      "cr-1b7d02e4a9c3":
        {"uid": "…", "state": "following", "cursor": 7, "gaps": 0,
         "markReady": {"state": "done", "head": "9f3c…", "attempts": 1}},
      "cr-9e40c6f1d2ab":
        {"uid": "…", "state": "following", "cursor": 2, "gaps": 0,
         "markReady": {"state": "none", "head": null, "attempts": 0}}
    },
    "conditions": [{"type": "Following", "status": "True", "reason": "Following",
                    "message": "2 of 2 followed change(s) still open"}]
  }
}
```

Entries are independent: each is probed, relayed and marked ready on its own, each
relay is its own inbox entry naming the change, and a problem on one is
reported in the condition message and stops nothing else.

Each entry of `status.inbox`, which this controller, GatewayController,
`fleet send`, `fleet amend` and a plugin append to and TaskController removes
from:

| Field | Meaning |
| --- | --- |
| `key` | Deterministic: `task/<name>/<uid>/relay/<change uid>/<revision>` for a relay or a join, `task/<name>/<uid>/send/<n>` for `fleet send`, `task/<name>/<uid>/amend/<generation>` for `fleet amend`, `task/<name>/<uid>/plugin/<plugin>/<key>` for a plugin's append, `task/<name>/<uid>/gateway/<conversation uid>/<revision>` for a chat relay, `task/<name>/<uid>/gateway/child/<child uid>/<item>` for a child's words. Refused if already present in `inbox` or `deliveries`. |
| `from` | `forge/<change>`, the ChangeRequest's name; `cli/<user>`; `plugin/<name>`; or `gateway/…` for a chat relay or a child's words. |
| `text` | The message, delivered as typed. |
| `about` | For a relay: the mirror's `uid`, `revision` and `head.id` the message was computed from. Absent for `fleet send`. TaskController's `deliveries.revisions` keeps the highest delivered revision per mirror `uid`, so a recreated mirror starts afresh. |
| `answers` | For `fleet send --answers`: the declared Question ids the entry answers. Absent otherwise. |
| `amends` | For `fleet amend`: the spec generation it created. Absent otherwise. |
| `queuedAt` | Unix milliseconds. |

### Requested probes

| Task state | Probes |
| --- | --- |
| No declaration and no entries | None. |
| A declaration without an entry, or an entry not Settled | `Object { kind: ChangeRequest, name }` per such change; `Whoami { host }` once per distinct host. |
| An entry whose mark-ready is being evaluated and whose snapshot predicates all hold | `AtRest { session }` for the Task's recorded session, for the `workerIdle` predicate: the runtime is asked last, and only when its answer could decide something. |
| Every entry Settled | None. |
| Deleting | None. |

`Probe::Object` is generic: the manager answers it from the store as
`Known(Some)`, `Known(None)` for proven absence, or Unknown when the record
is unreadable. It needs no lease renewal and runs before any runtime or forge
probe. `Probe::Whoami` is answered by `Forge::whoami` and cached by the
manager for the sweep. `AtRest` is the Task controller's existing probe,
reused and answered by the runtime. `probes` returns only what the
observations lack, so the manager asks in rounds: the objects first, then
the login on each host those objects' `status.ref` names, then the session,
until nothing is asked.

`ForgeObservations` holds `changes` (keyed by ChangeRequest name), `whoami`
(keyed by host), `at_rest`, and `workers`, every Task's name by `uid`, which
labels another worker's marked note. An absent key means not asked.

### Returned plan

The same `Plan` type the Task controller returns, restricted to this
controller's fields: `status` carries the desired `status.forge` alone,
including every advanced cursor and every new mark-ready intent, and
`appends` the inbox entries to land with it, and nothing else; the store
refuses a plan that names another manager's field. `effects` holds
`MarkReady` actions only, one per entry whose intent the status carries.
`remove_finalizer` is always `None`.

| Action | Performed by the manager through | Receipt |
| --- | --- | --- |
| `MarkReady { change, expect: {head, state} }` | The Forge port, which re-reads head and state first | `MarkedReady { already }`; `Moved { head, state }` when either differed, nothing mutated; Failed when the forge answered no; or Unknown. Written onto the entry under the effect key. |

A relay is not an effect. It is part of the status write: the inbox entries
and the cursors land together or not at all.

## Effect and recovery keys

| Item | Key |
| --- | --- |
| Join or relay (inbox entry) | `task/<name>/<uid>/relay/<change uid>/<revision>` |
| Mark ready | `task/<name>/<uid>/ready/<change uid>/<head.id>` |

`<change uid>` is the ChangeRequest's `uid`, so a mirror recreated after
retention never collides with keys from its predecessor; `<revision>` is the
ChangeRequest revision the message covers. The same snapshot can never
produce two different relays, the append refuses a duplicate key, and a
relay key is also the runtime intent key the Task controller journals the
delivery under. A mark-ready intent without a receipt is a question the next pass
answers by observing the change's state, never by repeating the effect.
Mark-ready intents and receipts live on the entry, apart from TaskController's.

## Ready policies

A policy is a set of predicates, declared in native config under
`[forge.readyPolicies.<name>]` and selected by `spec.forge.draft.policy` or
the `forge.draftPolicy` default. The name is only a key; Fleet attaches no
meaning to it. Each predicate is a pure function of the ChangeRequest
snapshot, the Task and the probes above:

| Predicate | Holds when |
| --- | --- |
| `workerDone` | The Task has a `shipped` result for `status.brief.generation`. A result that a delivered amendment superseded does not count. On in every policy and not configurable. A change already marked ready stays ready. |
| `workerIdle` | `AtRest` for the Task's session is Known true: the agent has finished its turn and is not mid-way through answering feedback. On in every policy and not configurable. |
| `checksPassed` | `head.checks` is `passed`; or it is `none` and `forge.noCheckGraceMs` has passed since `head.firstSeenAt`, because a repository whose CI starts only once the change is ready would otherwise wait forever. `pending` and Unknown never pass. |
| `botReviewsAddressed` | Every bot-authored inline note in the snapshot is resolved or has a later note from this Task's worker in its thread, and no bot review in `reviews` on the current head is `changes-requested`. |
| `noUnresolvedThreads` | `threads.unresolved` is `0`, over the whole change. |
| `forgeMergeable` | `mergeable.value` is `true`: the forge itself says the change can be merged. Optional; off in the shipped policies because a draft is often reported unmergeable for being a draft. |

`fleet init` writes three policies into the generated config, `checks`,
`checks-and-bots` and `strict`, and no `draftPolicy`; the
[native config](../../configuration/fleet-toml.md#the-forge-section) shows
them. They are ordinary entries: rename, edit or delete them. The four
predicates above are the whole configurable vocabulary: a policy naming any
other key, `workerDone` and `workerIdle` included, is refused at config
load, as is a `draftPolicy` naming no policy; a Task selecting a policy the
config does not define is refused at apply, and a policy removed from the
config after that raises `NeedsAttention/PolicyUnknown` on the Tasks that
name it. Adding a predicate is a controller change and a config key, not an
API change.

The standing worker policy tells every worker to open its change as a draft.
Fleet never puts a change back into draft, and never marks ready a change that has
ever been seen ready.

### `spec.forge`

```json
"forge": {
  "relay": {"notes": true, "checks": false},
  "draft": {"policy": "checks-and-bots"}
}
```

Each field present overrides the matching `[forge]` default; a field absent
inherits it. `"draft": {"policy": null}` disables mark-ready for this Task
even when the config names a default. The block is mutable like `dispatch`
and takes effect on the next pass; turning relay back on relays only what
comes after, because the cursor advanced meanwhile. A spec change increments
the Task's generation as today, and a worker result still names the
generation of the brief it received, so a running worker is never asked for
a generation it has not seen.

## The declare verb

`fleet declare TASK --change-request URL` is a manager verb, like
`fleet result` (`declare_change_request` in
[the verbs](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-manager/src/verbs.rs)):

1. Parse the URL through the Forge port; a host not configured is refused.
2. Ensure one ChangeRequest exists for it, named from the canonical ref, by
   exclusive creation; an existing one is reused after its `status.ref` is
   verified against the parsed URL.
3. Append `{"kind": "ChangeRequest", "url": …, "name": …}` to
   `Task.status.declared` as field manager `worker`, with CAS on the Task.
   A repeated URL is a no-op.

`fleet declare TASK --json '{"kind": "…", …}'` appends any object with a
string `kind`; Fleet validates nothing else and this controller ignores
kinds it does not know; the same object again is a no-op. Both forms are
accepted after the brief has a receipt and before deletion, and conclude
nothing; the Task is checked before any ChangeRequest is created, so an
ineligible Task leaves no orphan. The two steps of the first form
are not atomic; a crash between them leaves a ChangeRequest nobody follows,
which is polled until it settles and then removed by retention, and the next
`declare` of that URL finds it.

## Manager execution protocol

The [Task controller's protocol](../task/interface.md#manager-execution-protocol)
applies, with these additions:

1. Each controller has its own due gate: TaskController's `status.notBefore`,
   and for ForgeController any entry's `notBefore` or a declaration without
   an entry. A Task due for either is swept. TaskController runs first; the
   manager rereads the Task before running ForgeController, so the second
   plan does not conflict with the first on every sweep.
2. `Object` probes are answered from the store before the lease is renewed
   for runtime or forge probes; `Whoami` is asked through the Forge port
   once per host per sweep and the answer shared by every Task on it.
3. The plan's status write uses the store's typed mutation for this
   controller, `record_relay`: replace `status.forge` and append the listed
   inbox entries, in one CAS write; a duplicate key refuses the whole write
   and the next pass decides again. TaskController's counterpart records a
   delivery and removes the delivered entry in one write. `fleet send`
   appends one entry.
4. `MarkReady` is performed through the Forge port after the lease is
   renewed, like a runtime effect, once the status carrying its intent is
   written; its receipt is then written onto the entry that holds the
   intent, through the same typed mutation with nothing appended, re-read
   and retried on a conflict so it lands whatever the Task did meanwhile.
   A conflict on the intent write runs no effect.

## Store port and write ownership

| Writer | May change | Required fencing |
| --- | --- | --- |
| `apply` | New Task, labels, `dispatch`, `spec.forge`; request deletion | Store validation, exclusive creation or resourceVersion CAS. |
| `worker` | `status.result`; `status.declared` through `fleet declare` | Current resourceVersion and a delivered brief. |
| `forge-controller` | All of `status.forge`, the mark-ready intents and receipts included; appends to `status.inbox`, both through `record_relay` | Current resourceVersion and live manager lease. |
| `cli` | Appends to `status.inbox` through `fleet send` | Current resourceVersion. No lease: the CLI calls a manager verb. |
| `task-controller` | Everything else in Task status; `status.deliveries`; removes delivered `inbox` entries | Current resourceVersion and live manager lease. |

`status.inbox` has no single owner. It is reached only through three typed
mutations, `append_inbox`, `record_relay` and `record_delivery`, each named
by its caller and each naming its source with its own prefix, with one rule
the store enforces, no duplicate keys across `inbox` and `deliveries`.
This is an addition to the store's one-owner-per-field contract, stated in
[the contracts](../../CONTRACTS.md#what-is-served), not an exception
to it: no mutation touches another manager's field. The architecture test
lists which crates may name each manager, as it does for the managers that
exist.
