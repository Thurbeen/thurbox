# Plugin controller

Fleet hosts registered external controllers through bounded protocol-1 processes.
The CLI acceptance suite exercises registered kinds, reconciliation, Task
operations, environment separation and effects across process restarts.

A plugin is a controller written outside this repository, in a language a
person can read in one sitting, such as a TypeScript or Python script. It
defines its own resource kinds, decides what to do with each object of those
kinds, and may create and steer Tasks. Fleet hosts it the way it hosts its
own controllers: the manager owns every read, every write and every process,
and the plugin is a function from values to a plan. The plugin that brought
this about is one that mirrors GitHub issues and opens a Task to review,
comment on or implement each; a Notion plugin is the second.

| Read | Answers |
| --- | --- |
| [Plugin kinds](../resources/plugin-kinds.md) | What does a plugin's resource look like, and who may write what on it? |
| [State machine](#plugin-controller-state-machine) | How does one object of a plugin kind move through a sweep, an effect, and deletion? |
| [Interface](#plugin-controller-interface) | What is the protocol, the manifest, the probes, the plan verbs and the write rules? |
| [Task](../resources/task.md#plugin-operations) | What may a plugin do to a Task? |
| [Native config](../configuration.md#plugins-section) | How is a plugin registered, bounded and given its secrets? |
| [Forge controller](forge.md) | The controller this one is modelled on. |
| [Overall architecture](../architecture.md#the-plugin-controller) | Where does this host fit in Fleet? |

## One run

An issues plugin, registered in the native config as `github-issues`:

1. `fleet up` starts. For each `[plugins.<name>]` entry the manager runs the
   program once, bounded, with the `manifest` call, and learns the kinds the
   plugin defines, `IssueSource` and `Issue`, with a JSON Schema for each. The
   store serves those kinds for the life of the process. A manifest that
   cannot be read refuses `up`, like a config that does not parse.
2. A person applies an `IssueSource` naming a repository and a label. The
   store validates its spec against the plugin's schema and refuses what the
   schema refuses.
3. On the next sweep the object is due. The manager asks the plugin's pure
   `probes` function what it needs; it answers with one plugin probe, "list
   the open issues with this label". The manager performs that probe through
   the plugin's own `observe` function, a second bounded process with the
   credentials the config names, and gets a Known answer or Unknown.
4. The manager calls `reconcile` with the object, the answers and the time.
   The plugin returns a plan: its own status, when to look again, and one
   `Issue` object to create per issue not yet mirrored, each under a name it
   derives from the issue number.
5. A pure gate in the manager admits the plan against the plugin's rights,
   writes the status by compare-and-swap under the lease, and creates the
   objects, each exclusive by name and labelled as this plugin's.
6. Each `Issue` is reconciled the same way. Its plan creates one Task with a
   brief rendered from the issue, suspended, and records the Task's name and
   `uid` in its status. A later pass, after a person or a policy says so,
   patches the Task's `dispatch` to true. The plugin follows the Task by
   probing it: its phase, its result, the changes it declared.
7. When the Task concludes, the plan carries one effect, "comment on the
   issue with the result", under a key derived from the issue and the result.
   The manager writes the intent into the object's status before running the
   effect through the plugin's `perform` function, and writes the receipt
   after. A key with a receipt is never run again.
8. A closed issue settles its mirror; the plugin's plan asks for its own
   object to be deleted after a retention it chooses. Deleting the
   `IssueSource` is a person's act; its mirrors stay until their own plans
   retire them, unless the kind declared a finalizer and the plugin cleans up
   first.

One object costs one plugin process per phase per sweep when due, a few
bounded calls when it asked for plugin probes, and one status write. A quiet
object whose plan returned the same status writes only its bookkeeping.

## Why a host and not a port

- **The decisions are the plugin's.** Fleet's three controllers are already
  pure functions from an object, observations and the time to a plan. Moving
  that function out of process changes the language, not the architecture:
  the manager still owns I/O, the store still owns fencing, and a plan is
  still admitted by Rust before anything is written.
- **The I/O is bounded the one way Fleet knows.** A plugin process is one
  call through `fleet-exec`: a timeout, a killed tree, an answer that is
  Known or Unknown. A plugin that hangs costs one bound and one Unknown.
- **Nothing is trusted that was not declared.** A plugin writes only the
  status of its own kinds, creates only objects of kinds it may create,
  touches only Tasks it is allowed to touch, and reaches the world only
  through probes it asked for and effects it keyed. The gate that enforces
  this is pure and tested like the controllers.
- **A plugin persists nothing.** Its memory is the status of its objects,
  written by the manager with the same compare-and-swap and the same lease as
  every other status. A crash between two passes loses nothing it could not
  recompute.

## Pure where it decides, bounded where it acts

A plugin has two halves in one program, selected by the call the manager
makes, and they run with different environments:

| Half | Calls | Environment | May |
| --- | --- | --- | --- |
| Controller | `manifest`, `probes`, `reconcile` | `PATH` and what the OS needs to start a process, nothing the operator set | Compute. Everything it needs arrives as values; nothing it reads elsewhere is recoverable. |
| Observer | `observe`, `perform` | The variables `passEnv` and `envFrom` name, resolved per sweep | Read and change the world: run `gh`, call an HTTP API, with the manager machine's logins. Each call is bounded and answers Known or Unknown. |

This is the split Fleet already has between `fleet-controllers` and
`fleet-forge`, applied to one script. The controller half's purity is a
contract enforced by starvation, not a sandbox: Fleet withholds credentials,
configuration and the store, and states that a plugin reading outside a
probe gets an answer Fleet cannot recover after a crash. A later host, a
WASM runtime or an embedded engine, could enforce it under the same
protocol; the first host is a process, because `fleet-exec` already bounds
one.

## Every write is the manager's

The plugin returns a plan; the manager performs it, in the order every
controller's plan is performed: the status carrying each effect's intent is
written first, then the creates, amends, patches, appends, declares and deletes,
each a manager verb under the lease, then the effects, then their receipts.
The plugin's field manager is `plugin/<name>`. It owns all of the status of
its kinds; the host keeps three reserved fields in it, `notBefore`,
`effects` and `host`, under the same owner. TaskController, the worker and
ForgeController keep every field they own today; a plugin reaches a Task only
through the verbs a person has, never a status write.

## Scope

In scope: plugin-defined kinds with validated specs, a pure controller half
and a bounded observer half, store probes and plugin probes, keyed effects
with intents and receipts, and every Task verb a person has except `result`:
create, patch the mutable spec fields, amend, delete, send, declare. A plugin
reconciles only kinds it defines; it reads Tasks and ChangeRequests through
probes.

Out of scope, deliberately: a plugin writing `status.result`, because the
whole system reads a result as the worker's claim; a plugin reconciling a
built-in kind, which would need a plugin-owned status subtree on Task and is
left for a later step; a plugin creating objects of another plugin's kinds;
and a persistent plugin process, which would trade the bounded call for a
service Fleet would have to supervise.

## Decisions deferred to the implementation steps

- The exact JSON Schema dialect and the crate that validates it, chosen
  against the dependency rules in `~/.agent-rules/OPINIONS.md` when present.
- Whether the `list` probe takes a label selector only, or also a field of
  spec, once the first plugin shows which it needs.
- The size caps in the [interface](#bounds), set generously here
  and tightened on evidence.

## Implementation and evidence

The protocol types live in `fleet-api/src/plugin.rs`, pure admission in
`fleet-controllers/src/plugin.rs`, process hosting in `fleet-plugin`, and
registration and sweeps in `fleet-manager/src/plugins.rs`. The file store
validates schemas and rechecks identities, ownership and leases under its
write lock for every plugin verb. The command line loads manifests once per
process and accepts plugin kind names and plurals.

`e2e_plugin_controller` drives the built CLI and a Rust protocol stub across
process restarts. `plugin_gate` covers admission rules; `store_contract`
covers fencing, creator ownership and the amend precondition;
`architecture` enforces the crate and writer boundaries. TypeScript/Python
SDKs and issue-specific plugins remain outside this repository.

## Plugin controller state machine

A plugin kind has no phase Fleet defines: the plugin's status is the plugin's,
phases included. What Fleet defines is how one object of a plugin kind moves
through a sweep, how an effect is carried from intent to receipt, and how an
object is deleted. The [interface](#plugin-controller-interface) defines the calls, probes
and plan verbs used here.

![One object of a plugin kind: when due, its plugin's pure probes function is asked in rounds, store probes are answered from the store and plugin probes through the plugin's bounded observe call; reconcile returns a plan; the pure gate admits it; status with the effect intents is written by CAS under the lease, then creates, amends, patches, appends, declares and deletes run as manager verbs, then each effect runs through perform and its receipt is written. A deleting object with a finalizer is reconciled until its plan removes it.](plugin.svg)

### Due

The manager reconciles an object of a plugin kind when any of these holds:

| Condition | Why |
| --- | --- |
| `status.notBefore` is absent or past | The ordinary cadence, as for a Task or a ChangeRequest. |
| `metadata.generation` is above `status.observedGeneration` | The spec changed; the plugin sees it at once. |
| `deletionTimestamp` is set and the kind's finalizer is held | The plugin must finish before the record goes. |

A plan that sets no `notBefore` gets `now + pollMs` of its kind, from the
manifest. Nothing is reconciled while its plugin is unavailable; the object
keeps its `notBefore` and the manager counts it in its report.

### One pass

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

### Effects

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

### Creates

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

### Deleting

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

### Unavailable plugins

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

### Conditions

The host writes none. A plugin writes its own `status.conditions`, in
Fleet's shape, `{type, status, reason, message, lastTransitionTime}`, and
should keep `lastTransitionTime` unless the status changes, so a quiet pass
writes nothing. The SDKs provide this. The host's `host.error` and
`host.verbs` are facts for the plugin to turn into a condition, typically
`NeedsAttention`, which `fleet status` prints for every kind.

## Plugin controller interface

A plugin is one program the manager runs through `fleet-exec`, with one of
five calls on its argument list and one request on stdin. The controller
calls are pure functions; the observer calls are bounded I/O. The
[state machine](#plugin-controller-state-machine) defines when each call is made and what is
done with its answer; the [plugin kinds](../resources/plugin-kinds.md) page
defines the objects the calls are about.

### Registration

A plugin is registered in the native config, operator-owned, under
[`[plugins.<name>]`](../configuration.md#plugins-section):
the program, its arguments, its working directory, its call bound, which
Tasks it may touch, and the environment its observer half receives. Nothing
in the store can register a plugin. At `fleet up` the manager makes the
`manifest` call once per plugin and refuses to start on a manifest it cannot
read, a kind two plugins claim, a kind a built-in already serves, or a
`name` in the manifest that differs from the config key.

### Protocol

Protocol 1. One process serves one call. The call is the last argument,
after the configured `args`; the request is JSON on stdin; the answer is JSON
on stdout; stderr is captured for the manager's report and never parsed.
Exit code zero and one well-formed answer is an answer. Anything else is a
failed call, which is never an answer of `unknown` and never a plan.

```text
<program> <args...> manifest    stdin: {"protocol":1,"plugin":"github-issues"}
<program> <args...> probes      stdin: one request per line
<program> <args...> reconcile   stdin: one request per line
<program> <args...> observe     stdin: one request per line
<program> <args...> perform     stdin: one request
```

Requests and answers use JSON Lines with matching `id` values. The current
host sends one request per process for every call; batching is reserved for
a later optimization. A missing or unmatched answer fails the call. A
`perform` request is always alone, so a kill past the bound kills one effect.

Every request carries `protocol`, `plugin`, `call` and `id`. Every answer
carries `id` and either `ok: true` with the call's payload or `ok: false`
with `why`. The host treats an `ok: false` answer as a failed call;
`host.error` records that the call failed, not `why`.

#### `manifest`

```json
{"ok": true, "manifest": {
  "protocol": 1,
  "name": "github-issues",
  "kinds": [
    {"kind": "IssueSource", "pollMs": 60000, "finalizer": false,
     "spec": {"type": "object", "additionalProperties": false,
              "required": ["repo"],
              "properties": {"repo": {"type": "string"},
                             "labels": {"type": "array", "items": {"type": "string"}}}},
     "status": {"type": "object"}},
    {"kind": "Issue", "pollMs": 300000, "finalizer": true,
     "spec": {"type": "object", "additionalProperties": false,
              "required": ["source", "number", "url"],
              "properties": {"source": {"type": "string"},
                             "number": {"type": "integer"},
                             "url": {"type": "string"}}}}
  ]}}
```

| Field | Meaning |
| --- | --- |
| `protocol` | `1`. A manifest of a protocol this build does not speak is refused. |
| `name` | Must equal the `[plugins.<name>]` key. |
| `kinds[].kind` | A CamelCase kind name, unique across built-ins and plugins. The CLI accepts it lowercased, `fleet get issue issue-88`. |
| `kinds[].pollMs` | The cadence when a plan sets no `notBefore`. At least 1000. |
| `kinds[].finalizer` | Whether a deleting object is reconciled until the plan removes the finalizer. |
| `kinds[].immutableSpec` | Default false. True refuses any spec change after creation, as for Task and ChangeRequest. |
| `kinds[].spec` | A JSON Schema the store validates every applied or created spec against. Required. |
| `kinds[].status` | A JSON Schema the host validates the plugin's own status against before writing it. Optional. The reserved fields are validated by the host, not this schema. |

#### `probes` and `reconcile`

Both receive the same input:

```json
{"protocol": 1, "plugin": "github-issues", "call": "reconcile", "id": "r1",
 "object": {"apiVersion": "fleet/v1alpha1", "kind": "Issue",
            "metadata": {"name": "issue-88", "uid": "…", "generation": 1,
                         "resourceVersion": 4410,
                         "labels": {"fleet.dev/plugin": "github-issues"}},
            "spec": {"source": "app", "number": 88,
                     "url": "https://github.com/OWNER/app/issues/88"},
            "status": {"phase": "tasked", "task": {"name": "issue-88-review", "uid": "…"},
                       "observedGeneration": 1, "notBefore": 1730000000000,
                       "effects": {}, "host": {}}},
 "observations": {
   "task": {"known": {"apiVersion": "fleet/v1alpha1", "kind": "Task", "…": "…"}},
   "issue": {"unknown": {"why": "Timeout"}}},
 "now": 1730000300000,
 "defaults": {"pollMs": 300000}}
```

`observations` is keyed by the `id` the plugin gave each probe; a key absent
means not asked. `object` is the stored object as `fleet get` prints it,
including the reserved status fields. Neither call receives configuration,
environment or credentials, and neither should need them: the config a
plugin wants is spec.

`probes` answers with the probes it still lacks. The manager asks in rounds,
at most three, until the answer is empty:

```json
{"id": "p1", "ok": true, "probes": [
  {"id": "task", "type": "object", "kind": "Task", "name": "issue-88-review"},
  {"id": "mine", "type": "list", "kind": "Issue", "labels": {"source": "app"}},
  {"id": "issue", "type": "plugin", "request": {"op": "readIssue", "repo": "OWNER/app", "number": 88}}
]}
```

| Probe | Answered by | Answer |
| --- | --- | --- |
| `object {kind, name}` | The store, before any lease renewal | `known` with the object, `known: null` for proven absence, `unknown` for an unreadable record. Any served kind. |
| `list {kind, labels?}` | The store | `known` with the objects matching every label, at most 200, else `unknown` with `TooMany`; `unknown` when any record is unreadable. Any served kind. |
| `plugin {request}` | This plugin's `observe`, one bounded call per probe | Whatever `observe` answered: `known` with its value, or `unknown` with `why` and an optional `retryAfterMs`. |

`reconcile` answers with a plan:

```json
{"id": "r1", "ok": true, "plan": {
  "status": {"phase": "reviewed", "task": {"name": "issue-88-review", "uid": "…"},
             "observedGeneration": 1,
             "conditions": [{"type": "Reviewed", "status": "True", "reason": "Shipped",
                             "message": "review posted", "lastTransitionTime": 1730000300000}]},
  "notBefore": 1730003900000,
  "creates": [],
  "patches": [{"kind": "Task", "name": "issue-88-review", "uid": "…", "spec": {"dispatch": true}}],
  "appends": [{"task": "issue-88-review", "uid": "…", "key": "issue/88/comment/3",
               "text": "A maintainer replied on the issue: …"}],
  "amends": [{"task": "issue-88-review", "uid": "…", "text": "The issue changed: …",
              "reason": "new issue body", "ifGeneration": 2}],
  "declares": [],
  "deletes": [],
  "effects": [{"key": "comment/88/1", "request": {"op": "comment", "number": 88, "body": "…"}}],
  "delete": false,
  "removeFinalizer": false}}
```

| Verb | Meaning | Admitted when |
| --- | --- | --- |
| `status` | The plugin's whole status, replacing what was stored, minus the reserved fields | It validates against the kind's status schema and names no reserved field. Omitted means unchanged. |
| `notBefore` | Earliest next pass, Unix milliseconds | Absent means `now + pollMs`. |
| `creates[]` | Full objects to create, each exclusive by name, labelled `fleet.dev/plugin` by the host | Kind is one of this plugin's, or Task; the spec validates; at most 20 per plan. |
| `patches[]` | Spec fields to change on an existing object, named with its `uid` | Task: `dispatch` and `forge` only. Own kinds: nothing; a mirror's identity is its spec. The `uid` matches, and the Task is this plugin's under `tasks = "own"`. |
| `appends[]` | Inbox entries for a Task, as `plugin/<name>` | Same store checks as `fleet send`; the key is `task/<task>/<uid>/plugin/<plugin>/<key>`, unique for the Task; the Task is within `tasks`. |
| `amends[]` | `{task, uid, text, reason?, outputs?, ifGeneration}`, the `amend` verb on the Task's behalf, from `plugin/<name>` | The text is not empty and `ifGeneration` is present; the `uid` matches, and the Task is within `tasks`. At most 8 per plan. |
| `declares[]` | `{task, uid, changeRequest: url}`, the `declare` verb on the Task's behalf | The verb's own preconditions: a brief receipt, not deleting; the Task is within `tasks`. |
| `deletes[]` | Objects to delete, named with their `uid` | Own kinds, or a Task within `tasks`. Fenced on `uid`; retire the current controller object with `delete`, after effects. |
| `effects[]` | `{key, request}` to perform through `perform`, with intent before and receipt after | At most eight; existing keys are retained and never repeated. |
| `delete` | Retire this object, by `uid` and `resourceVersion` under the lease | Not deleting already. |
| `removeFinalizer` | Let a deleting object go | The object is deleting and holds this plugin's finalizer. |

A plan with any inadmissible entry is refused whole: nothing of it is
written, and `host.error` names the entry. The gate that decides this is a
pure function in `fleet-controllers`, tested like the controllers, and the
only code that names the `plugin/<name>` field manager.

An admitted verb can still be refused when it runs. `host.verbs` records the
refusal, and the other verbs of the plan still run. An `amends[]` entry
follows these rules:

- The store applies the checks of `fleet amend`. It refuses a missing,
  Stalled or deleting Task, a full inbox, and an output name that the Task
  already declares.
- The amendment's `from` is `plugin/<name>`. Its inbox entry has the key
  `task/<task>/<uid>/amend/<generation>`.
- `ifGeneration` must equal the Task's `metadata.generation` when the verb
  runs. Otherwise the store refuses the verb with
  `Task/<task> is at generation M, not N`.
- Amends run before patches. A patch in the same plan does not move the
  generation that `ifGeneration` names.

A plugin reads the Task's generation from a probe and sets `ifGeneration` to
it. A repeated or recovered pass then appends nothing a second time.

#### `observe` and `perform`

The observer half receives the probe or the effect the controller half
planned, and nothing else of the object:

```json
{"protocol": 1, "plugin": "github-issues", "call": "observe", "id": "o1",
 "probe": {"id": "issue", "request": {"op": "readIssue", "repo": "OWNER/app", "number": 88}}}

{"id": "o1", "ok": true, "known": {"state": "open", "title": "…", "comments": ["…"]}}
{"id": "o1", "ok": true, "unknown": {"why": "Throttled", "retryAfterMs": 60000}}
```

```json
{"protocol": 1, "plugin": "github-issues", "call": "perform", "id": "e1",
 "effect": {"key": "comment/88/1", "request": {"op": "comment", "number": 88, "body": "…"}},
 "intentKey": "plugin/github-issues/Issue/issue-88/…/comment/88/1"}

{"id": "e1", "ok": true, "outcome": {"done": {"commentUrl": "…"}}}
{"id": "e1", "ok": true, "outcome": {"failed": {"why": "issue is locked"}}}
{"id": "e1", "ok": true, "outcome": {"unknown": {"why": "Timeout"}}}
```

`observe` reads and answers Known or Unknown, with the discipline the Forge
adapters follow: an answer is complete or it is Unknown, and Unknown never
means absent, closed or empty. `perform` acts once under `intentKey`, which it
may embed in what it writes, a hidden marker in a comment, so a later
`observe` can tell whether an attempt landed. The SDKs say this on the
function's doorstep: Fleet never repeats a key, so a `perform` that acted
and then lost its answer has acted.

#### Environment

| Half | Receives |
| --- | --- |
| Controller (`manifest`, `probes`, `reconcile`) | `PATH`; on Windows also `SYSTEMROOT`. Working directory `cwd`. No other variable, whatever the manager's environment holds. |
| Observer (`observe`, `perform`) | The controller half's variables, plus each variable `passEnv` names, copied from the manager's environment, plus the merged map of the EnvProviders `envFrom` names, resolved through the manager's existing provider machinery before the plugin's first observer call of the sweep. `envFrom` wins over `passEnv` on a collision. |

Values never reach the store, a status, a plan, argv, or the controller
half. The host passes them as environment and forgets them with the process.
The same rules apply as for EnvProvider command sources: programs run on the
manager, with the manager's user, and only trusted programs belong in the
config.

#### Bounds

| Item | Bound |
| --- | --- |
| One call | `plugins.<name>.callTimeoutMs`, default 15000; the tree is killed past it. |
| Requests per process | One. |
| Probe rounds per object per pass | 3. |
| stdout per process | 8 MiB; past it the process is killed and every unanswered request has failed. |
| One plan's `status` | 256 KiB. |
| `creates` per plan | 20; `amends` per plan, 8; `effects` per plan, 8. |
| Verbs per plan | 200 `patches`, `appends`, `amends`, `declares` and `deletes` together. |
| Probes per round | 200. |
| `list` answer | 200 objects. |
| `status.effects` | Newest 100 keys. |
| `operation_bound` | One call's worst case; the lease is sized to outlast it, as for the runtime and forge ports. |

### Write ownership

| Writer | May change | Required fencing |
| --- | --- | --- |
| `apply` | A plugin kind's spec and labels, except `fleet.dev/plugin`; request deletion | Schema validation; `immutableSpec` when declared. |
| `plugin/<name>` | All of the status of its kinds, through the manager only; deletion of its own objects; the label `fleet.dev/plugin` on what it creates | Current `resourceVersion`, the live lease, the gate's admission. |
| Host, as `plugin/<name>` | The reserved fields `status.notBefore`, `status.effects`, `status.host` | The same write as the plugin's status. |
| `plugin/<name>` on a Task | `spec.dispatch`, `spec.forge`, amendments, inbox appends, declarations, deletion | The verbs' own checks; `tasks = "own"` requires the Task's `fleet.dev/plugin` label to name this plugin. |

`fleet.dev/plugin` is a label the store sets on objects created through a
plugin's plan and refuses from `apply`. `ownerReferences` stay refused on
every kind: a mirror's Task outlives the mirror unless the plugin deletes it.

### Architecture

| Piece | Crate | Role |
| --- | --- | --- |
| `Plugins` port: `operation_bound`, `available`, and `call` (manifest, probes, reconcile, observe, perform) | `fleet-api` | Pure types; the port the manager calls. The plan it returns is untyped JSON until admitted. |
| `PluginGate`: a pure function from an untrusted plan, the plugin's rights and the object to typed writes or a refusal | `fleet-controllers` | The only code that names `FieldManager::Plugin`. One test per rule. |
| The stdio host: batching, environment scrubbing, JSON Lines, bounds | `fleet-plugin` (new), depending on `fleet-api` and `fleet-exec` | The only crate that knows a plugin is a process. |
| Dynamic kinds, schema validation, the creator label, the plugin appender | `fleet-store` | Kinds are a value the store is configured with, built from the config and the manifests. |
| The sweep after ForgeController; the verbs on a plugin's behalf | `fleet-manager` | Renews the lease before every plugin process. |
| `[plugins]`, `fleet list plugins`, the kinds on the command line | `fleet-cli` | `apply` of a plugin kind reaches the manifest through the port, cached per process. |

`crates/architecture.toml` gains the crate, its edges, the
`FieldManager::Plugin` writer and the new forbidden methods for the CLI
(`call`, `admit`, `plugin_mutate`), each with a negative fixture.
The purity rules are unchanged: no plugin process is started outside
`fleet-exec`, and no plugin's name appears above the port except in the
config.

### An SDK's view

The TypeScript SDK, outside this repository, turns the protocol into four
functions and a manifest, and runs the batching loop:

```ts
import { plugin, type Input, type Plan, type Probe } from "@fleet/plugin";

export default plugin({
  manifest: { protocol: 1, name: "github-issues", kinds: [IssueSource, Issue] },
  probes(input: Input): Probe[] { /* pure */ },
  reconcile(input: Input): Plan { /* pure */ },
  async observe(request): Promise<Known | Unknown> { /* gh, fetch */ },
  async perform(request, intentKey): Promise<Outcome> { /* once per key */ },
});
```

The Python SDK is the same five names. Neither SDK is required: a plugin is
anything that speaks the protocol, and the test stub in this repository is a
small Rust binary that does.
