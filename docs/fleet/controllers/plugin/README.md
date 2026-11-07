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
| [Plugin kinds](../../resources/plugin-kinds.md) | What does a plugin's resource look like, and who may write what on it? |
| [State machine](state-machine.md) | How does one object of a plugin kind move through a sweep, an effect, and deletion? |
| [Interface](interface.md) | What is the protocol, the manifest, the probes, the plan verbs and the write rules? |
| [Task](../../resources/task.md#plugin-operations) | What may a plugin do to a Task? |
| [Native config](../../configuration/fleet-toml.md#plugins-section) | How is a plugin registered, bounded and given its secrets? |
| [Forge controller](../forge/README.md) | The controller this one is modelled on. |
| [Overall architecture](../../ARCHITECTURE.md#the-plugin-controller) | Where does this host fit in Fleet? |

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
- The size caps in the [interface](interface.md#bounds), set generously here
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
