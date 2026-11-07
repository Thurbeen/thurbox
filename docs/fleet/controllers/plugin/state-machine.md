# Plugin controller state machine

A plugin kind has no phase Fleet defines: the plugin's status is the plugin's,
phases included. What Fleet defines is how one object of a plugin kind moves
through a sweep, how an effect is carried from intent to receipt, and how an
object is deleted. The [interface](interface.md) defines the calls, probes
and plan verbs used here.

![One object of a plugin kind: when due, its plugin's pure probes function is asked in rounds, store probes are answered from the store and plugin probes through the plugin's bounded observe call; reconcile returns a plan; the pure gate admits it; status with the effect intents is written by CAS under the lease, then creates, amends, patches, appends, declares and deletes run as manager verbs, then each effect runs through perform and its receipt is written. A deleting object with a finalizer is reconciled until its plan removes it.](state-machine.svg)

## Due

The manager reconciles an object of a plugin kind when any of these holds:

| Condition | Why |
| --- | --- |
| `status.notBefore` is absent or past | The ordinary cadence, as for a Task or a ChangeRequest. |
| `metadata.generation` is above `status.observedGeneration` | The spec changed; the plugin sees it at once. |
| `deletionTimestamp` is set and the kind's finalizer is held | The plugin must finish before the record goes. |

A plan that sets no `notBefore` gets `now + pollMs` of its kind, from the
manifest. Nothing is reconciled while its plugin is unavailable; the object
keeps its `notBefore` and the manager counts it in its report.

## One pass

| Step | Input | Outcome | On failure |
| --- | --- | --- | --- |
| Read | The object by name | The object and its `resourceVersion` | An unreadable record is Unknown and skipped; nothing is written. |
| Probe rounds | `probes` is asked with the observations so far, at most three rounds | Store probes (`object`, `list`) answered from the store without renewing the lease; plugin probes answered through `observe`, the lease renewed before each | A failed `probes` call ends the pass with `host.error`; an Unknown probe answer is handed to the plugin as such. |
| Reconcile | `reconcile` with the object, the answers, `now` and the kind's defaults | A plan | A failed or malformed call ends the pass with `host.error` and `notBefore = now + pollMs`; the plugin's status is untouched. |
| Admit | The plan, the plugin's rights, the object | The plan as typed writes | A refused plan ends the pass with `host.error` naming the verb refused; nothing of it is written. |
| Write | The admitted status, carrying every new effect's intent | One compare-and-swap write under the lease | A conflict drops the whole plan; the next pass rereads. |
| Verbs | `creates`, `amends`, `patches`, `appends`, `declares`, `deletes`, in that order | Each a manager verb under the lease; each outcome recorded in `host.verbs` | A refused verb is recorded and stops nothing after it. |
| Effects | Each `effects[]` entry with an intent and no receipt | `perform`, the lease renewed before each | The outcome, Known or Unknown, is the receipt. |
| Receipts | Each performed effect | Written into `status.effects` by compare-and-swap | A conflict rereads and retries the receipt write, preserving concurrent changes. |

`host.error` is the host's reserved record of what went wrong on the last
pass; the plugin sees it in its next input and may raise its own condition
from it. The host writes nothing else of the plugin's status.

## Effects

An effect is `{key, request}`. The key is the plugin's and must be unique for
the life of the object; the host prefixes it with
`plugin/<plugin>/<kind>/<name>/<uid>/` for the intent it journals. Rules:

| State of the key | What the host does | What the plugin sees |
| --- | --- | --- |
| No intent | Writes the intent with the status, then performs | Nothing yet |
| Intent, no receipt | Nothing: the process that performed it may have died after acting | `effects[key].receipt` absent, `intent.at` set |
| Receipt `done` | Nothing, ever | The receipt and its result |
| Receipt `failed` | Nothing, ever, for this key | The receipt and its reason |
| Receipt `unknown` | Nothing, ever, for this key | The receipt and its reason |

Fleet never repeats an effect under the same key. A plugin that wants to try
again plans a new key, `comment/88/2`, and decides for itself how many
attempts are enough and after how long, on its own `notBefore`. Before
retrying an Unknown, it should ask a probe that would show whether the
first attempt acted, the way TaskController observes a session key, because
nothing else can. An intent without a receipt that is older than
`callTimeoutMs` plus the manager's lease is a receipt `unknown` with
`LostAnswer` written by the host on the next pass, so the plugin is not
left waiting on a crash.

A plan may carry at most eight effects. The host keeps the newest 100 keys
under `status.effects`; a plugin must not plan a key it already rotated out,
and derives keys from facts, an issue number and a result generation, so a
repeat is recognisable.

## Creates

A `creates[]` entry is a full object of a kind this plugin may create: its
own kinds, and Task. The host sets the label `fleet.dev/plugin: <plugin>` on
it and creates it exclusively by name:

| Existing object with that name | Outcome |
| --- | --- |
| None | Created. |
| Created by this plugin | `already`; nothing changes, the spec is not compared. |
| Anything else | Refused, recorded in `host.verbs`. |

A plugin derives names from facts, `issue-88` or `issue-88-review`, so a
retried pass creates nothing twice. It learns the `uid` of what it created by
probing the object on a later pass, and records it in its own status, as
the Forge controller records a ChangeRequest's `uid`.

## Deleting

A kind without a finalizer is deleted at once by the store, as an
EnvProvider is; the plugin is not consulted, and its next pass finds the
object gone. A kind that declared `finalizer: true` gets
`fleet.dev/plugin/<plugin>` on create, and a deleting object stays due until
a plan says `removeFinalizer: true`. The plugin uses those passes to post a
last comment or retire a Task it created; the host performs the plan as
usual and removes the finalizer last. A plugin removed from the config holds
its finalizers until it is back. Fleet has no forced delete and this design
adds none: retire a plugin's objects before removing its entry.

A plugin retires its own objects with `delete: true`, performed by `uid` and
`resourceVersion` under the lease like a ChangeRequest's retention. It
deletes a Task with a `deletes[]` entry naming the Task and its `uid`; the
Task's own controller then releases its session as for any deletion.

## Unavailable plugins

A plugin is unavailable when its `manifest` call failed at `fleet up`, which
refuses to start, or when one call type fails three consecutive times to
produce a valid answer (a process that could not start, exited nonzero
without an answer, or timed out). Successful calls of another type do not reset
that failure count. An
unavailable plugin's objects keep their
`notBefore` and are not reconciled; the manager reports it in run output and
retries a manifest call once per
sweep. `fleet list plugins` checks registration afresh in its own process. An
Unknown answer from `observe` is not a failure of the
plugin: it is an answer.

## Conditions

The host writes none. A plugin writes its own `status.conditions`, in
Fleet's shape, `{type, status, reason, message, lastTransitionTime}`, and
should keep `lastTransitionTime` unless the status changes, so a quiet pass
writes nothing. The SDKs provide this. The host's `host.error` and
`host.verbs` are facts for the plugin to turn into a condition, typically
`NeedsAttention`, which `fleet status` prints for every kind.
