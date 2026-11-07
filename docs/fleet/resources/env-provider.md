# EnvProvider resource

EnvProvider supplies stored string data and explicit file/command sources to
[TaskConfig](task-config.md). It is passive data: no controller, status,
finalizer, polling or background credential refresh. TaskConfig references it
through `spec.envFrom: [{"name":"project-env"}]`; Tasks reference TaskConfig.
There is no separate ConfigMap kind or direct Task `envFrom` field.

Save this as `env-provider.json`:

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "EnvProvider",
  "metadata": {"name": "project-env"},
  "spec": {
    "data": {"LANG": "C.UTF-8"},
    "sources": [{"type": "file", "path": "/work/config/agent-env.json"}]
  }
}
```

Before dispatch, create the referenced file using the
[environment JSON format](../configuration/environment-json.md), for example
`{"PROJECT_MODE":"review"}`. Use native absolute paths on the manager. On
Windows, JSON paths need escaped backslashes, for example
`"C:\\work\\config\\agent-env.json"`.

## Fields

| Field | Required | Meaning |
| --- | --- | --- |
| `data` | no | Stored JSON string map; defaults to empty. Visible through resource reads. |
| `sources` | no | Ordered file/command sources; defaults to empty, maximum 32. |
| `sources[].type` | yes | `file` or `command`; selects the fields below. |
| File: `path` | yes | Absolute regular-file path on the manager. |
| Command: `program` | yes | Absolute executable path on the manager. |
| Command: `args` | no | Explicit argument array; defaults to empty. No implicit shell. |
| Command: `cwd` | yes | Absolute working directory on the manager. |
| Command: `timeoutMs` | yes | Integer from 1 through 15000; whole process tree is killed on timeout. |

Unknown fields, invalid names/maps, NUL arguments, relative source paths,
unsupported source types and invalid timeouts are refused. Like other
resources, the envelope uses `fleet/v1alpha1` and a lowercase DNS-like name;
`ownerReferences` and supplied status are refused.

`data` is plaintext configuration, not a secret vault. Do not commit real
credentials in manifests, command arguments or helper paths. Resource reads
expose declared data and source definitions. External output stays out of
ordinary Task status, logs/errors and process arguments. Fleet does not
encrypt provider data or launch snapshots.

## Command sources

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "EnvProvider",
  "metadata": {"name": "helper-env"},
  "spec": {
    "sources": [{
      "type": "command",
      "program": "/absolute/path/to/credential-helper",
      "args": ["environment", "--json"],
      "cwd": "/absolute/working/directory",
      "timeoutMs": 5000
    }]
  }
}
```

Programs run locally on the **manager**, even when the Task runs remotely.
They inherit the manager's environment and run directly with argv through
Fleet's existing bounded executor. There is no shell expansion, `~` expansion,
PATH search for the provider program or relative-path lookup beside TOML,
the manifest or the Task workspace. Only invoke trusted helpers that you
intend Fleet to execute. Helpers should only read data: failed/interrupted
resolution may execute them again before the snapshot is durable.

Commands must exit zero and write one UTF-8 JSON string map to stdout, under
[the same format and limits](../configuration/environment-json.md) as files.
Stderr is never included in source errors. Captured command streams are
bounded by the executor; outputs above 64 KiB are refused. Command sources
require a manager lease above 20000 ms; the default real-runtime lease meets
this bound. Explicit shorter leases are refused for command resolution.
The manager renews its lease before every source.

## Merge, errors and lifecycle

Provider `data` is lowest priority; each source then overrides earlier values
in list order. TaskConfig merges providers in its `envFrom` order, followed by
TaskConfig `env`, Task `env`, and runtime model-variable selection. See
[TaskConfig precedence](task-config.md#referencing-and-overriding-defaults).
Case variants override consistently across hosts; collisions within one map
are invalid. Both source maps and merged environments are bounded to 64 KiB.

Missing/deleting provider references, missing/nonregular files, nonzero exits,
timeouts, malformed/non-string maps and oversized output block launch with an
observable reconcile error. All references are checked before any provider
command. Correct the input and reconcile again; no partial worker is spawned.
Resolved values freeze privately before the first runtime probe/effect.
Subsequent source edits, deletion/recreation, changed files and command output
do not alter the existing Task's environment, retry or session identity.
[TaskConfig](task-config.md#resolution-and-freezing) owns snapshot privacy,
retention and recovery instructions.

## Commands

```sh
fleet --config fleet.toml apply -f env-provider.json
fleet --config fleet.toml get envprovider project-env
fleet --config fleet.toml list envproviders --json
fleet --config fleet.toml status envprovider project-env --json
fleet --config fleet.toml delete envprovider project-env
```

The spec is mutable and edits increment generation. Passive status has no
lifecycle phase. Deletion removes the provider immediately: pending references
block, but already frozen Tasks continue using their private snapshot.
