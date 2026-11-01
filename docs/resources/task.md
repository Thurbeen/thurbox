# Task resource

A Task is one agent session doing one brief. TaskController spawns the
session on the host the Task names, sends the brief once, delivers what is
queued in the Task's inbox, records the worker's answer, and deletes the
session when the Task is deleted. The guarantees behind each step are in
[the contracts](../../crates/CONTRACTS.md). Read the
[controller overview](../controllers/task/README.md),
[interface](../controllers/task/interface.md) and
[state machine](../controllers/task/state-machine.md) for execution semantics.

## Example

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "Task",
  "metadata": {"name": "design-305"},
  "spec": {
    "harness": "claude-code",
    "host": "local",
    "brief": "Write the controller design.",
    "workspace": {"dir": "/work/design-305"},
    "dispatch": true,
    "outputs": {"prUrl": {"type": "string", "required": true}}
  }
}
```

## Spec

Written by people and scripts through `fleet apply`. Fixed once created,
except `dispatch` and `forge`.

| Field | Required | Meaning |
| --- | --- | --- |
| `harness` | unless inherited | The agent harness, such as `claude-code`, passed to the runtime as is. |
| `host` | no | The runtime host the session runs on. Default `local`. |
| `model` | no | The model the harness runs; it reaches the harness through the variable `runtime.modelEnv` names for it. |
| `configRef` | no | `{"name":"config"}` references passive execution defaults. |
| `env` | no | Explicit string environment map; overrides inherited values. |
| `brief` | yes | The brief, sent once to the ready session. |
| `workspace` | yes | What the session works in: `{"dir": PATH}`, a directory that exists before the Task (the thurbox runtime needs an absolute path on the host), or `{"repositories": [{"name", "url", "ref"?}]}`, repositories the runtime materializes for this Task. The first repository is the primary, where the agent starts. Fleet never interprets `ref`; the runtime decides what it means. Deleting the session cleans the workspace up as the runtime is configured to. See [Workspaces](#workspaces). |
| `dispatch` | no | Nothing spawns while `false` (the default). |
| `outputs` | no | The structured outputs a result must carry, by name: each `{"type": T, "required": bool}`, where T is `string`, `integer`, `number`, `boolean`, `object` or `array`. |
| `forge` | no | Overrides of the `[forge]` defaults for the changes this Task's worker declares: `relay.notes`, `relay.checks`, `draft.policy` (a name under `[forge.readyPolicies]`, refused at apply when the config does not define it; `null` disables mark-ready). Field-level: absent fields inherit. Mutable like `dispatch`. |

`harness` and `host` may not start with `-`. Any other field is refused.

See [TaskConfig](task-config.md) and [EnvProvider](env-provider.md) for provider sources,
precedence, validation and private launch snapshots.

### Workspaces

A Task names its repositories. The runtime decides how they become files.

```json
{"workspace": {"repositories": [
  {"name": "app", "url": "https://github.com/example/app", "ref": "main"},
  {"name": "docs", "url": "https://github.com/example/docs"}
]}}
```

- Each `name` and each `url` is unique in the Task. Each `url` and `ref` is
  non-empty, has no whitespace and does not start with `-`.
- A workspace has `dir` or `repositories`, never both.
- Fleet has no branch, base, worktree or cleanup field. The runtime chooses
  them. The worker reports the change it opened through `fleet declare`.
- The thurbox runtime creates one worktree per repository, on the branch
  `<keyPrefix>/<session key>`. `runtime.repositories` in `fleet.toml` maps
  each URL to its source checkout. An unmapped URL keeps the Task in Pending
  with `RepositoryUnmapped`.
- With thurbox, a `harness` that `runtime.workspaceTrust` does not map keeps
  the Task in Pending with `TrustUnmapped`.
- Deleting the session cleans the workspace. With thurbox, the worktrees go
  with the session by default. `runtime.workspaceCleanup = "keep"` keeps
  them instead.

## Status

Written by TaskController under the manager lease, except `result` and
`declared`, which the worker writes through `fleet result` and `fleet
declare`, `forge`, which the [Forge controller](../controllers/forge/README.md)
writes, and `inbox`, which nobody owns and which changes only through the
typed mutations described under [Inbox](#inbox).

| Field | Meaning |
| --- | --- |
| `phase` | Execution phase, defined by the [state machine](../controllers/task/state-machine.md). |
| `observedGeneration` | The spec generation the controller last acted on. |
| `conditions` | One entry per [condition type](../controllers/task/state-machine.md#conditions), with `status`, `reason`, `message` and `lastTransitionTime`. |
| `execution` | Frozen execution selection and source identities; environment values are private. |
| `session` | The session's runtime id, host and key, once spawned or adopted. |
| `intents`, `receipts` | Each effect's intent, written before it runs, and its outcome, written after. |
| `spawnAttempts`, `briefSends` | Spawn and brief-send counters, each bounded at 3. A spawn refused for an unmapped repository does not count. |
| `briefProbeFailures`, `releaseAttempts` | Three failures raise `NeedsAttention`; reads and releases continue after each wait. |
| `revivals`, `reviveAttempts` | Revives over the Task's life, which number their keys, and revives since the session was last seen not lost. Three revives raise `NeedsAttention/ReviveFailed`; revives continue after each wait. |
| `resumes`, `resume` | Resume notes over the Task's life, which number their keys, and the key of the note owed to a revived session until its delivery is known. |
| `notBefore` | Earliest next reconcile after a failed attempt, in Unix milliseconds. The fixed retry delay is 30 seconds and survives manager restarts. |
| `sessionCheckedAt` | When a Concluded Task's session was last looked up, in Unix milliseconds. At rest, the next lookup is due 5 minutes later. |
| `brief` | The brief's key and the generation it was sent for, once it has a receipt. |
| `result` | The worker's claim: outcome, generation, summary, artifacts, outputs. |
| `inbox` | Messages queued for the session, oldest first: `{key, from, text, about, queuedAt}`. Appended by `fleet send`, removed by TaskController once delivered, each through a typed store mutation; never replaced as a list. |
| `deliveries` | TaskController's: `receipts`, the newest 100 delivery receipts by inbox key (`{key, from, at, recovered}`), and `revisions`, per mirror (`about.uid`) the highest `about.revision` delivered, which keeps a duplicate out after its receipt has rotated. |
| `deliveryAttempts` | TaskController's bookkeeping per inbox entry being delivered: `sends` (bounded at 3), `probeFailures` and `failed`. Removed with the entry. |
| `declared` | The worker's, through `fleet declare`: facts it declared, each a JSON object with a required string `kind` and nothing else validated. See [Following declared changes](#following-declared-changes). |
| `forge` | The Forge controller's: one entry per declared change, keyed by ChangeRequest name, with its `uid`, `state`, `cursor`, `gaps`, mark-ready record and `notBefore`, plus that controller's conditions. Absent for a Task that declared nothing. |
| `gateway` | The [Gateway controller](../controllers/gateway/README.md)'s: its tie to a chat conversation, relay cursor, children and post records, plus that controller's conditions. Absent for a Task that never talks. See [Talking in a conversation](#talking-in-a-conversation). |
| `answered` | Per Question id, `{key, from, at}`: the inbox entry that answered it, its source and when. No owner; written only by an inbox append carrying `answers`, first answer wins. |

## Metadata and ownership

`apiVersion` is `fleet/v1alpha1`; `kind` is `Task`. `metadata.name` is a
lowercase DNS-like name of at most 253 characters. The store assigns `uid`,
`generation`, `resourceVersion`, creation time and `fleet.dev/release` finalizer.
A spec change increments generation; compare-and-swap uses resourceVersion.
Deleting a Task sets `deletionTimestamp` while the finalizer holds its record.
Nonempty `ownerReferences` are refused.

`fleet apply` accepts desired spec and labels, and refuses supplied status.
Only `dispatch` and `forge` can change in an existing spec. Setting
`dispatch` false prevents a Pending Task from spawning; it does not stop a
session already spawned.

## Worker result

`fleet result` writes `status.result`: `outcome` is `shipped`, `stuck`,
`failed` or `not-applicable`, and `generation` must match the delivered
brief's generation. Optional `summary`, `artifacts` (strings) and `outputs`
carry the answer. Required outputs must be present with their declared JSON
types; undeclared outputs and top-level null output values are refused.
Nested object and
array contents have no additional schema validation.

A result is accepted only after a brief receipt exists and before deletion.
Repeating the same result is a no-op; replacing it with another result is
refused. `shipped` is a worker's claim: Fleet does not verify a forge artifact.
The next reconcile changes phase to `Concluded`; it does not release the
session until the Task is deleted.

## Inbox

`fleet send TASK TEXT` queues a message for the Task's session. It appends
one entry to `status.inbox` as `cli/<user>` under the key
`task/<name>/<uid>/send/<n>`, where `n` is the resourceVersion the append
was made against, so a key never repeats for the life of the Task. `--json`
prints the entry as appended.

| Field | Meaning |
| --- | --- |
| `key` | Deterministic and unique across `inbox` and `deliveries`; also the runtime intent key the delivery is journaled under. |
| `from` | `cli/<user>` for `fleet send`; `forge/<change>` for a relay, the ChangeRequest's name. |
| `text` | The message, delivered as typed. |
| `about` | For a relay: the mirror's `uid`, `revision` and `head`. Absent for `fleet send`. |
| `queuedAt` | Unix milliseconds. |

`status.inbox` has no owner. The store reaches it through typed mutations,
each named by its caller: append (`fleet send`, and the Forge controller's
relay write, which also replaces `status.forge`), which refuses a key
already in `inbox` or `deliveries`, an `about.revision` not above the
mirror's highest delivered one, an append while more than 20 entries are
undelivered, and a deleting Task; and record-and-remove, TaskController's
status write that also moves one entry's receipt into `deliveries` and the
entry out of `inbox`. No manager may replace the list.

TaskController delivers inbox entries as it delivers the brief, oldest first,
one per pass: only to a session observed ready on that pass, under the entry's
key as the runtime intent key, journaled by the runtime, at most 3 sends, then a
receipt. A send whose answer was lost is read back from the journal, never
repeated blindly. An entry that exhausts its sends raises
`NeedsAttention/DeliveryFailed` naming it and stays queued behind nothing: later
entries are still delivered. An entry whose journal stays unreadable for 3 reads
raises `NeedsAttention/DeliveryUnconfirmed` naming it. That entry is never sent
again, stays in `status.inbox`, and holds back no later entry. The Thurbox
runtime refuses an entry over 16 000 bytes before it types anything, because
tmux refuses a longer command. More than 20 undelivered entries raise
`NeedsAttention/InboxFull`. A Concluded Task with a nonempty inbox is due again
for delivery, and at rest for a session recheck every 5 minutes, as the [Task
state machine](../controllers/task/state-machine.md#transition-rules) describes;
a readable lookup that no longer shows its session is `WorkerGone` and
`NeedsAttention`, never a respawn. A session that is still there without its
window, as after a host reboot, is revived to read its inbox. See [after a host
reboot](../controllers/task/state-machine.md#after-a-host-reboot). A Stalled or
deleting Task delivers nothing: a deleting Task refuses `fleet send` and
releases its session as before.

## Following declared changes

The [Forge controller](../controllers/forge/README.md), a second controller
over the Task, reads the changes a worker declared and relays what is new on
each into the inbox.

| Field or verb | Owner | Meaning |
| --- | --- | --- |
| `spec.forge` | `apply`; mutable like `dispatch` | Optional overrides of the `[forge]` defaults: `relay.notes`, `relay.checks`, `draft.policy` (a name under `[forge.readyPolicies]`, refused at apply when the config does not define it; `null` disables mark-ready). Field-level: absent fields inherit. |
| `status.declared` | `worker`, through `fleet declare` | Facts the worker declared, each a JSON object with a required string `kind` and nothing else validated. `--change-request URL` writes `{kind: "ChangeRequest", url, name}` and ensures the named ChangeRequest exists; `--json` writes any object. Accepted after the brief has a receipt and before deletion; a repeated URL is a no-op; it concludes nothing. |
| `status.forge` | ForgeController | One entry per declared change, keyed by ChangeRequest name: its `uid`, `state`, `cursor`, `gaps`, mark-ready record and `notBefore`; plus this controller's conditions. Absent for a Task that declared nothing. |

The Forge controller appends to `status.inbox` as `forge/<change>`, with
`about` set, in the same write that advances the entry's cursor. The
[Forge controller interface](../controllers/forge/interface.md#fields-this-controller-reads-and-owns)
defines every field of `status.declared` and `status.forge`, with an example
of a Task following two changes.

Any Task may declare a change, with or without a `spec.forge` block; the
standing worker policy tells every worker to open its change as a draft, to
declare it, and to end every post on it with
`<!-- fleet:worker <task uid> -->` on its own line, which is how Fleet tells
the worker's words from a person's on the same account and one worker's from
another's. Those three rules are the worker's to keep: a change opened ready
is never marked ready, a change never declared is never followed, and a post
without the mark comes back to its author as feedback. Nothing forge-related
has to be created in advance, and a second Task may declare a change another
Task opened. No new finalizer and no owner reference: a ChangeRequest
outlives the Tasks that follow it, and deleting a Task deletes nothing else.
The [Forge controller interface](../controllers/forge/interface.md#store-port-and-write-ownership)
lists the resulting writers.

## Talking in a conversation

Partly implemented. The [Gateway controller](../controllers/gateway/README.md)
ties a Task to a chat thread and relays the thread into its inbox, as the
Forge controller does for a change. Posting what the Task declares, and
opening a thread for a Question, are design, not implemented. A worker
never posts in a chat and holds no platform credential: it declares, and
Fleet relays.

| Field, label or verb | Owner | Meaning |
| --- | --- | --- |
| `fleet declare --json '{"kind":"Question","id":"q1","text":"…"}'` | `worker` | Ask once. Posted in the tied thread, or, on a Task with no tie and no parent, opens a thread in the questions route's channel and ties the Task to it. Never reposted. |
| `fleet declare --json '{"kind":"Comment","id":"c1","text":"…"}'` | `worker` | Say something in the tied thread. Never opens a thread. The `id`, unique for the Task, keeps two identical Comments apart. |
| `fleet declare --json '{"kind":"Draft","id":"d1","title":"…","text":"…"}'` | `worker` | Show a draft for review in the tied thread. Never opens a thread. |
| `status.gateway` | GatewayController | `conversation`, the verified tie; `cursor`, the Conversation revision relayed through; `seen`, `declared`, `result`, `changes`, `children` and `posted`, what was read or posted; `posts`, the post effects' intents and receipts; `questions`, each posted Question; `gaps`; and its conditions. Absent for a Task that never talks. |
| `status.answered` | none; inbox appends carrying `answers` | Per Question id `{key, from, at}`, first writer wins. Another channel showing the same Question reads it and stops waiting. |
| `gateway.fleet.dev/conversation`, `gateway.fleet.dev/conversation-uid` labels | the Conversation's controller, at creation; `apply` refuses both | The Conversation a conversation Task was created for, by name and `uid`. |
| `gateway.fleet.dev/parent`, `gateway.fleet.dev/parent-uid` labels | whoever creates a child and may already append to the parent's inbox (a person, or a plugin whose Task scope covers the parent), at creation only | The parent Task's name and `uid`. A child never talks in a thread; its parent relays its Questions, Comments, Drafts and results into its own inbox, so naming a parent is writing to it. A verified tie wins over them. |
| `gateway.fleet.dev/route` label | whoever creates the Task | The route whose channel a Question from this untied Task opens a thread in, instead of `gateway.questions`. |
| `fleet send TASK --answers ID TEXT` | `cli` | Answer a declared Question from the command line, or as a parent answering a child, recording `status.answered`. |

Inbox entries from the Gateway come from `gateway/<conversation>` or
`gateway/child/<child>`, under keys the
[interface](../controllers/gateway/interface.md#inbox-entries) defines.
Unknown declaration kinds are ignored, as the Forge controller ignores
them.

## Commands

```sh
fleet apply -f task.json             # create, or change dispatch
fleet status task design-305 --json  # phase and conditions
fleet send design-305 "Also update the docs."  # queue a message for the session
fleet declare design-305 --change-request https://github.com/OWNER/app/pull/88  # follow the change the worker opened
fleet declare design-305 --json '{"kind": "Note", "text": "docs need a pass"}'   # any fact with a kind
fleet result design-305 --generation 1 --outcome shipped \
  --output prUrl='"https://example.test/pr/1"'
fleet delete task design-305         # release the session, then remove
fleet reconcile                      # run the controller under the lease
```

## Plugin operations

The [Plugin controller](../controllers/plugin/README.md)
lets a registered plugin do to a Task what a person does, through the same
manager verbs, never through a status write:

| Verb | On a Task | Carried by |
| --- | --- | --- |
| Create | A full Task manifest, exclusive by name; the store labels it `fleet.dev/plugin: <plugin>`, a label `apply` refuses to set | The plan's `creates` |
| Patch | `dispatch` and `forge`, the two mutable spec fields | The plan's `patches` |
| Send | An inbox entry from `plugin/<name>` under `task/<name>/<uid>/plugin/<plugin>/<key>`, with the inbox's own checks | The plan's `appends` |
| Declare | `fleet declare TASK --change-request URL` on the Task's behalf, with that verb's preconditions | The plan's `declares` |
| Delete | By name and `uid`; the release finalizer then runs as for any deletion | The plan's `deletes` |

`status.result` stays the worker's: no plugin writes it. Under the config's
`tasks = "own"`, every verb but create requires the Task to carry the
plugin's label; `tasks = "any"` lifts that. The field owners above are
unchanged; the inbox gains a third named appender beside `fleet send` and
the Forge controller.
