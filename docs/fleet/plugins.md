# Write a plugin

A plugin adds a resource kind to Fleet, and the controller that reconciles
it. This page builds a working plugin step by step:
[examples/fleet/plugin/files.py](../../examples/fleet/plugin/files.py), a
`File` kind that keeps one file's content. The
[Plugin controller](controllers/plugin.md) page is the reference for every
field and limit.

## What a plugin is

A plugin is one program on the manager's machine. Fleet runs it once per
call, with the call name as its last argument and one JSON request on
stdin. The program answers one JSON object on stdout. It holds no lease,
writes nothing to the store and keeps no state between calls: the manager
does every write.

A plugin has two halves:

| Half | Calls | May do I/O | Gets the plugin's environment |
| --- | --- | --- | --- |
| Controller | `manifest`, `probes`, `reconcile` | No: they must be pure functions of the request | No |
| Observer | `observe`, `perform` | Yes, bounded by `callTimeoutMs` | Yes: `passEnv` and `envFrom` |

Write a plugin when you want Fleet to keep something outside it in a
declared state, or to mirror something outside it as a resource, and no
built-in kind does it. Examples: issues on a tracker, files, tickets that
start Tasks. A plugin can also create and steer [Tasks](resources/task.md).

## How one pass works

```text
manager                                   plugin program
  │ manifest  (once, at start)  ─────────▶  kinds and JSON Schemas
  │ for each due object:
  │   probes  {object, observations}  ───▶  [probe, …]   (repeat, up to 3 rounds,
  │   observe {probe}  (each plugin probe) ▶  {known} | {unknown}   until no probes)
  │   reconcile {object, observations} ──▶  {plan}
  │ write the plan's status and each effect's intent
  │   perform {effect, intentKey}  ──────▶  {outcome}
  │ write each receipt
```

An object is due when its `status.notBefore` passes, when its spec has a
new generation, and while it is deleting. A probe of type `object` or `list`
reads the store, and the manager answers it without calling the plugin.

## Step 1: write the program

Every request carries `protocol` (1), `plugin`, `call` and `id`. Every answer
carries the same `id` and `ok: true`, with the call's payload. An answer with
`ok: false`, a nonzero exit, a timeout or malformed JSON is a failed call.

| Call | The request adds | The answer adds |
| --- | --- | --- |
| `manifest` | nothing | `manifest`: `{protocol, name, kinds}`. Each kind: `kind` (CamelCase), `pollMs` (at least 1000), `spec` (a JSON Schema), and optionally `status`, `finalizer`, `immutableSpec`. |
| `probes` | `object`, `observations`, `now`, `defaults` | `probes`: a list of `{id, type: "plugin", request}`, `{id, type: "object", kind, name}` or `{id, type: "list", kind, labels}`. An empty list ends the rounds. |
| `observe` | `probe`: `{id, request}` | `known`: any JSON, or `unknown`: `{why}`. |
| `reconcile` | `object`, `observations`, `now`, `defaults` | `plan`: see [What a plan can do](#what-a-plan-can-do). |
| `perform` | `effect`: `{key, request}`, and `intentKey` | `outcome`: exactly one of `done`, `failed` or `unknown`, each with any JSON. |

`files.py` does this for `File`:

1. `manifest` declares the kind. Its `spec` schema requires `name`, a plain
   file name, and `content`, a string. `additionalProperties: false` makes
   Fleet refuse unknown fields, as it does for built-in kinds.
2. `probes` asks for one plugin probe, `file`, until `observations` has it.
3. `observe` reads the file in the plugin's working directory. It answers
   `{"known": {"exists": …, "content": …}}`, or `{"unknown": {"why": …}}`
   when it cannot read it.
4. `reconcile` compares `spec.content` with the observation:
   - the same: the status is `{"phase": "Ready", "observedGeneration": N}`;
   - different: the status is `Writing`, with one effect,
     `{"key": "write/<generation>", "request": {name, content}}`, and
     `notBefore` one second later, to confirm the write;
   - Unknown: the status is `Unknown`, and nothing is written. Unknown never
     means absent.
5. `perform` writes the file and answers `{"outcome": {"done": {"bytes": N}}}`.

Check each call by hand before you register the program:

```sh
cd /srv/fleet-files
echo '{"protocol":1,"plugin":"files","call":"manifest","id":"manifest"}' \
  | /opt/fleet/examples/fleet/plugin/files.py manifest
echo '{"protocol":1,"plugin":"files","call":"observe","id":"observe","probe":{"id":"file","request":{"name":"hello.txt"}}}' \
  | /opt/fleet/examples/fleet/plugin/files.py observe
# {"known": {"exists": false}, "ok": true, "id": "observe"}
```

## Step 2: register it

Add the plugin to `fleet.toml`. The key is the plugin's name, and the
manifest's `name` must be the same. `program` and `cwd` are absolute paths
on the manager. Fleet runs the program directly, with no shell.

```toml
[plugins.files]
program = "/opt/fleet/examples/fleet/plugin/files.py"
cwd = "/srv/fleet-files"
callTimeoutMs = 15000      # the default
```

[The `[plugins]` section](configuration.md#plugins-section) has the other
fields: `args`, `tasks`, `passEnv` and `envFrom`. Then check the
registration:

```sh
fleet list plugins --json
# [{"available": true, "kinds": ["File"], "name": "files", "protocol": 1}]
```

## Step 3: apply an object of the kind

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "File",
  "metadata": {"name": "hello"},
  "spec": {"name": "hello.txt", "content": "Hello from Fleet.\n"}
}
```

```sh
fleet apply -f hello.json
# File/hello created
```

The store validates the spec against the kind's schema. A spec that the
schema refuses, such as `"name": "../outside.txt"`, fails with
`plugin value fails JSON Schema`.

## Step 4: reconcile, and read the status

Run `fleet up`, or one bounded run:

```sh
fleet reconcile --json      # "effects": 1, "errors": []
cat /srv/fleet-files/hello.txt
fleet status file hello --json
```

After the first pass, `hello.txt` exists and the status is:

```json
{
  "phase": "Writing",
  "observedGeneration": 1,
  "effects": {
    "write/1": {
      "intent": {"at": 1730000000100, "generation": 1},
      "receipt": {"at": 1730000000500, "outcome": {"done": {"bytes": 18}}}
    }
  },
  "host": {"generation": 1, "observedAt": 1730000000100, "verbs": []},
  "notBefore": 1730000001100
}
```

`phase` and `observedGeneration` are the plugin's. `effects`, `host` and
`notBefore` are the manager's. One second later the next pass observes the
file, and the phase becomes `Ready`.

How the status flows back:

1. The plan's `status` replaces the plugin's own fields. Without `status`,
   they stay as they are.
2. The manager writes each new effect's intent in the same write, then runs
   `perform`, then writes the receipt.
3. An effect key with an intent is never performed again. To act again, use
   a new key, as `write/<generation>` does.
4. A receipt whose answer was lost is `{"unknown": {"why": "LostAnswer"}}`.
   The next `reconcile` sees it in `object.status.effects` and decides.

## Step 5: change it and delete it

Apply the object with new content. The generation becomes 2, the object is
due at once, and the plan has the effect `write/2`.

`fleet delete file hello` deletes the object at once, because `File`
declares no finalizer. A kind with `finalizer: true` stays while deleting,
and the plugin reconciles it until its plan says `removeFinalizer: true`.
Use that to remove what the plugin made outside Fleet.

## What a plan can do

| Field | Effect |
| --- | --- |
| `status` | The plugin's own status fields. The kind's `status` schema checks them. |
| `notBefore` | The next pass, in Unix milliseconds. Default `now + pollMs`. |
| `effects` | `[{key, request}]`: work for `perform`, journaled under `status.effects`. At most 8. |
| `creates` | Objects to create, of the plugin's own kinds or `Task`, without a status. At most 20. The store labels them `fleet.dev/plugin: <name>`. |
| `patches` | `[{kind, name, uid, spec}]`: change a Task's `dispatch` or `forge`. |
| `appends` | `[{task, uid, key, text, answers}]`: queue a message in a Task's inbox. |
| `amends` | `[{task, uid, text, reason, outputs, ifGeneration}]`: amend a Task's brief. |
| `declares` | `[{task, uid, changeRequest}]`: declare a change for a Task. |
| `deletes` | `[{kind, name, uid}]`: delete objects of the plugin's own kinds, or Tasks. |
| `delete` | `true`: delete this object. |
| `removeFinalizer` | `true`: remove this object's finalizer while it is deleting. |

A pure gate admits each plan before the manager writes anything. It refuses
a whole plan that names a reserved status field, acts on a Task that the
plugin does not own (with `tasks = "own"`), or passes a limit. The
[Plugin controller interface](controllers/plugin.md#probes-and-reconcile)
gives each limit.

## Debugging

| Symptom | Where to look |
| --- | --- |
| `fleet list plugins` fails, or shows `"available": false` | Run the `manifest` call by hand. The manifest's `name` must equal the config key, and no kind may reuse a built-in name or its plural. |
| `unknown kind "file"` | The plugin is not in the config that the command read. Pass `--config`. |
| A pass failed | `status.host.error` says why. The manager also prints it in `fleet reconcile --json` under `errors`. |
| A Task verb did nothing | `status.host.verbs` has one result per `creates`, `amends`, `patches`, `appends`, `declares` and `deletes` entry. |
| An effect did not run, or ran with no answer | `status.effects.<key>`: an intent without a receipt, or `unknown` with `LostAnswer`. |
| `plugin <name> unavailable` | Three failed calls in a row. Fleet calls `manifest` again on the next sweep. |
| Objects of the kind are not reconciled | The plugin left the config. The manager reports the objects as unregistered and keeps them. |

A controller call gets only `PATH` (and `SYSTEMROOT` on Windows). Only
`observe` and `perform` get the variables of `passEnv` and `envFrom`. Fleet
never writes those values to the store, a status or an argument list.

[`e2e_doc_examples`](../../crates/fleet-cli/tests/e2e_doc_examples.rs) runs
this example from apply to delete.
