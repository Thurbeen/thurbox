# Plugin-defined kinds

Registered plugin kinds are served alongside Fleet’s built-in resources.

A plugin declares the kinds it reconciles in its manifest, and the store
serves them beside Task, TaskConfig, EnvProvider, ChangeRequest, Conversation
and Listener while the plugin is registered. They have Fleet's envelope, a
spec the plugin's schema validates, and a status the plugin owns. This page
defines what is common to every plugin kind; the
[Plugin controller](../controllers/plugin/README.md) defines how they are
reconciled.

## Example

Two kinds of an issues plugin registered as `github-issues`. A person applies
the source; the plugin creates the mirrors.

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "IssueSource",
  "metadata": {"name": "app"},
  "spec": {"repo": "OWNER/app", "labels": ["fleet"]}
}
```

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "Issue",
  "metadata": {"name": "issue-88", "labels": {"fleet.dev/plugin": "github-issues", "source": "app"}},
  "spec": {"source": "app", "number": 88, "url": "https://github.com/OWNER/app/issues/88"},
  "status": {
    "phase": "tasked",
    "title": "Retry a lost brief",
    "task": {"name": "issue-88-review", "uid": "…"},
    "observedGeneration": 1,
    "conditions": [{"type": "Tasked", "status": "True", "reason": "Created",
                    "message": "Task issue-88-review created", "lastTransitionTime": 1730000000000,
                    "observedGeneration": 1}],
    "notBefore": 1730000300000,
    "effects": {
      "comment/88/1": {"intent": {"at": 1730000100000, "generation": 1},
                       "receipt": {"at": 1730000101000, "outcome": {"done": {"commentUrl": "…"}}}}
    },
    "host": {"observedAt": 1730000100000}
  }
}
```

## Metadata

`apiVersion` is `fleet/v1alpha1`. `kind` is the manifest's CamelCase name;
the command line takes it lowercased. `metadata.name` is a lowercase
DNS-like name of at most 253 characters, which `apply` requires and a
plugin's `creates` derives from facts. The store assigns `uid`,
`generation`, `resourceVersion` and creation time, and the finalizer
`fleet.dev/plugin/<plugin>` when the kind declared one. `ownerReferences`
are refused, as on every kind.

Labels are free, except `fleet.dev/plugin`: the store sets it to the plugin's
name on every object created through a plugin's plan, Tasks included, and
refuses it from `apply`. It is how `tasks = "own"` recognises a plugin's
Tasks and how a person sees who made an object.

## Spec

Written by people through `fleet apply`, or by the plugin through `creates`.
Validated against the kind's `spec` schema on every write; what the schema
does not allow is refused with the schema's message. The schema is the
plugin author's and should set `additionalProperties: false`, as every
built-in kind refuses unknown fields; the manifest is accepted either way.

The spec is mutable, and a change increments `generation` and makes the
object due, unless the kind declared `immutableSpec: true`. A plugin reads
`metadata.generation` and writes `status.observedGeneration` when it has
acted on it, by convention; nothing enforces that field.

## Status

Written by the plugin, through the manager, as field manager
`plugin/<name>`. Everything in it is the plugin's, validated against the
kind's `status` schema when the manifest gives one, except three reserved
fields the host writes under the same owner:

| Field | Meaning |
| --- | --- |
| `notBefore` | Earliest next pass, Unix milliseconds: the plan's, else `now + pollMs`. |
| `effects` | Per effect key: `intent {at, generation}`, written before the effect runs; `receipt {at, outcome}`, written after, `outcome` one of `done`, `failed` or `unknown` with the plugin's payload or reason. Newest 100 keys. |
| `host` | The host's bookkeeping: `observedAt`, the last pass; `generation`, the spec generation that pass read; `error`, what failed on the last pass and why, absent when nothing did; `verbs`, the outcome of each `creates`, `amends`, `patches`, `appends`, `declares` and `deletes` entry of the last pass, by index. |

A plan that names a reserved field is refused. `fleet apply` refuses a
supplied status on every kind.

Conventions the SDKs follow and `fleet status` understands: `phase`, a
string; `conditions`, in Fleet's shape; `observedGeneration`. None is
required.

## Deletion

`fleet delete <kind> NAME` removes an object of a kind without a finalizer
at once; the plugin finds it gone. A kind with a finalizer is reconciled
while deleting until its plan says `removeFinalizer`, so the plugin can
retire what it made. A plugin retires its own objects with `delete: true`
in a plan, fenced on `uid` and `resourceVersion` like a ChangeRequest's
retention. Deleting an object deletes nothing else: a Task a mirror created
stays until the mirror's plan, or a person, deletes it.

## Commands

```sh
fleet --config fleet.toml list plugins --json
fleet --config fleet.toml apply -f issue-source.json
fleet --config fleet.toml get issuesource app
fleet --config fleet.toml list issue --json
fleet --config fleet.toml status issue issue-88 --json
fleet --config fleet.toml delete issue issue-88
```

`list plugins` prints each registered plugin, its kinds, its protocol and
whether its current manifest call succeeded. The other verbs take a plugin
kind as they take `task` or `changerequest`; `apply` of a plugin kind loads
the config to find the plugin and asks it for its manifest, one bounded call
cached for the process, to validate the spec. A kind whose plugin is not in
the config is unknown, and an object of a kind whose plugin was removed stays
readable and is reported by the manager, never reconciled.
