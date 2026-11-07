# Plugin controller interface

A plugin is one program the manager runs through `fleet-exec`, with one of
five calls on its argument list and one request on stdin. The controller
calls are pure functions; the observer calls are bounded I/O. The
[state machine](state-machine.md) defines when each call is made and what is
done with its answer; the [plugin kinds](../../resources/plugin-kinds.md) page
defines the objects the calls are about.

## Registration

A plugin is registered in the native config, operator-owned, under
[`[plugins.<name>]`](../../configuration/fleet-toml.md#plugins-section):
the program, its arguments, its working directory, its call bound, which
Tasks it may touch, and the environment its observer half receives. Nothing
in the store can register a plugin. At `fleet up` the manager makes the
`manifest` call once per plugin and refuses to start on a manifest it cannot
read, a kind two plugins claim, a kind a built-in already serves, or a
`name` in the manifest that differs from the config key.

## Protocol

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

### `manifest`

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

### `probes` and `reconcile`

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

### `observe` and `perform`

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

### Environment

| Half | Receives |
| --- | --- |
| Controller (`manifest`, `probes`, `reconcile`) | `PATH`; on Windows also `SYSTEMROOT`. Working directory `cwd`. No other variable, whatever the manager's environment holds. |
| Observer (`observe`, `perform`) | The controller half's variables, plus each variable `passEnv` names, copied from the manager's environment, plus the merged map of the EnvProviders `envFrom` names, resolved through the manager's existing provider machinery before the plugin's first observer call of the sweep. `envFrom` wins over `passEnv` on a collision. |

Values never reach the store, a status, a plan, argv, or the controller
half. The host passes them as environment and forgets them with the process.
The same rules apply as for EnvProvider command sources: programs run on the
manager, with the manager's user, and only trusted programs belong in the
config.

### Bounds

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

## Write ownership

| Writer | May change | Required fencing |
| --- | --- | --- |
| `apply` | A plugin kind's spec and labels, except `fleet.dev/plugin`; request deletion | Schema validation; `immutableSpec` when declared. |
| `plugin/<name>` | All of the status of its kinds, through the manager only; deletion of its own objects; the label `fleet.dev/plugin` on what it creates | Current `resourceVersion`, the live lease, the gate's admission. |
| Host, as `plugin/<name>` | The reserved fields `status.notBefore`, `status.effects`, `status.host` | The same write as the plugin's status. |
| `plugin/<name>` on a Task | `spec.dispatch`, `spec.forge`, amendments, inbox appends, declarations, deletion | The verbs' own checks; `tasks = "own"` requires the Task's `fleet.dev/plugin` label to name this plugin. |

`fleet.dev/plugin` is a label the store sets on objects created through a
plugin's plan and refuses from `apply`. `ownerReferences` stay refused on
every kind: a mirror's Task outlives the mirror unless the plugin deletes it.

## Architecture

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

## An SDK's view

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
