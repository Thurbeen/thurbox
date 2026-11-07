# TaskConfig resource

TaskConfig supplies reusable execution defaults for a [Task](task.md).
It is passive data: it has no controller, status or finalizer. Its spec is
mutable; editing it affects future resolutions, while existing frozen launches
keep their original values. Brief, workspace and outputs always belong to
each Task. TaskConfig does not define task templates.

Save this manifest as `task-config.json`:

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "TaskConfig",
  "metadata": {"name": "reviewer"},
  "spec": {
    "harness": "claude-code",
    "host": "local",
    "envFrom": [{"name": "project-env"}],
    "env": {"REVIEW_MODE": "careful"}
  }
}
```

## Fields

| Field | Required | Meaning |
| --- | --- | --- |
| `harness` | no | Default harness. The Task or this config must supply one. |
| `host` | no | Default Thurbox host name; ultimately defaults to `local`. |
| `model` | no | Default model; requires a matching harness variable in native `runtime.modelEnv`. |
| `envFrom` | no | Ordered references to [EnvProvider](env-provider.md), each `{"name":"provider"}`. |
| `env` | no | String environment map; overrides all referenced provider values. |
| `pauseAfterMs` | no | Pause a Task's session once its agent was idle this long, in milliseconds; at least 1. Absent means never. A thurbox that does not report activity never pauses either. See [pausing an idle session](../controllers/task.md#pausing-an-idle-session). |

All fields are optional; unknown fields are refused. Harness/host/model must
be nonempty, contain no NUL and not start with `-`. References use the same
lowercase DNS-like names as resources. No `ownerReferences` or supplied status
is allowed. Names are scoped to the current store; references do not load
files or fetch resources from a different Fleet installation.

TaskConfig is a JSON resource applied with `fleet apply`, separate from the
native [Fleet TOML configuration](../configuration.md#native-fleet-configuration-fleettoml). There is
no `[taskConfig]` section in that file and no implicit global TaskConfig.

## Referencing and overriding defaults

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "Task",
  "metadata": {"name": "review"},
  "spec": {
    "configRef": {"name": "reviewer"},
    "env": {"REVIEW_MODE": "focused"},
    "workspace": {"dir": "/work/project"},
    "brief": "Review the current change.",
    "dispatch": false
  }
}
```

An explicitly present Task harness, host, model or `pauseAfterMs` overrides
the corresponding config default. Explicit `model: null` disables an
inherited model. Explicit `host: "local"` overrides a remote default;
omitting host inherits it.
The Task spec, including its reference and whether these fields were present,
is immutable except `dispatch` and `forge`; `fleet amend` appends to
`spec.amendments` (see [Amendments](task.md#amendments)). Existing Tasks
need no reference and retain
their original behavior. To dispatch the example, apply the same Task with
`dispatch: true` after preparing its workspace and providers.

Environment precedence, lowest to highest:

```text
provider.data -> provider.sources (list order)
              -> TaskConfig.envFrom (list order)
              -> TaskConfig.env -> Task.env -> runtime.modelEnv selection
```

Later sources win, including case variants, retaining the winning spelling.
Each map refuses case collisions and invalid names. See the
[environment JSON format](../configuration.md#environment-json-file-and-command-output) for key,
value and size rules. Literal `env` is visible in resource reads and is not
secret storage. Use external provider sources when values must stay out of
manifests. The native model-variable selection is frozen with the launch.

## Resolution and freezing

```text
Task dispatch enabled
  -> validate config/provider references and execution settings
  -> resolve sources through Store/Runtime ports
  -> persist private snapshot under Task UID and manager lease
  -> persist status.execution identity
  -> ordinary Task probes, intent, spawn and brief
```

Undispatched Tasks consume current defaults when enabled. The first successful
resolution is frozen before runtime probes/effects. `status.execution` records
harness, host, model, `pauseAfterMs`, and every source
kind/name/UID/generation, without resolved environment values. A missing,
deleting, corrupt or invalid reference
blocks launch observably in `fleet reconcile`; correcting the input allows
retry. Fleet never launches a partial/defaulted wrong worker.

Retries and manager restarts reuse the snapshot, including a crash between
snapshot and status writes. Config/provider edits, deletion, name reuse,
changed files or command output cannot redirect an existing execution or
create another session/brief. Providers are not polled for refresh.

Private plaintext snapshots live at `launches/<Task UID>.json` inside the
store, outside resource reads. Unix directory/files use 0700/0600, including
temporary writes; exposed snapshots are refused. Windows inherits the store
ACL: keep it user-private, including custom data paths. Snapshots are not
encrypted and backups require equivalent protection. They remain through
conclusion and are removed on final Task deletion; interrupted private temp
writes are cleaned on store open.

A missing/corrupt snapshot for an already frozen Task blocks re-resolution.
Restore a protected backup or deliberately release/delete and recreate the
Task. Do not delete its snapshot to refresh credentials mid-run. Thurbox must
support `session create --env-stdin`; values never enter Task status, effect
intents, error output or process arguments. The session receives this
environment, not the manager's `OTEL_*` variables; see
[telemetry](../configuration.md#opentelemetry-export).

## Commands and compatibility

```sh
fleet --config fleet.toml apply -f env-provider.json
fleet --config fleet.toml apply -f task-config.json
fleet --config fleet.toml apply -f task.json
fleet --config fleet.toml get taskconfig reviewer
fleet --config fleet.toml list taskconfigs --json
fleet --config fleet.toml status taskconfig reviewer --json
fleet --config fleet.toml reconcile
fleet --config fleet.toml delete taskconfig reviewer
```

Apply config/provider resources before reconciling a dispatched Task. Apply
can store a reference whose target is not present yet; reconcile validates all
references before running a provider command. Passive status reports current
spec generation and no lifecycle phase. Deletion removes the config
immediately; unresolved Tasks then block, while frozen Tasks continue.

API/store versions remain `fleet/v1alpha1` and format 1. Older binaries reject
the new kinds/fields. Use the updated binary for stores containing them;
this feature performs no live migration. `fleet version --json` advertises
served kinds and configuration versions.
