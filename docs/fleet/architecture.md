# Fleet architecture

Fleet is a Rust command-line program that persists Tasks, ChangeRequests
and the other resource kinds, and drives their controllers. A Task contains
the execution inputs for one agent session: harness, host, workspace, brief
and expected outputs; the worker reports its own result. A ChangeRequest
mirrors one change on a forge, read through a Forge port and numbered so
consumers diff it by one cursor.

![Execution flow: CLI calls the manager; the manager uses the pure controller, store and runtime ports; the runtime starts bounded processes through fleet-exec](architecture.svg)

Arrows show calls during execution, not Cargo dependencies. `fleet-cli`
constructs the adapters; `fleet-manager` depends on API traits rather than
concrete store, runtime or forge crates. The
[ChangeRequest controller](controllers/change-request.md) runs in the
same sweep as the Task controller, through the Forge port.

## Components and boundaries

| Crate | Owns | Boundary |
| --- | --- | --- |
| [fleet-cli](../../crates/fleet-cli/src/main.rs) | Parse arguments, load config, construct adapters, print results | Calls manager verbs; does not write resources or run controller effects directly. |
| [fleet-manager](../../crates/fleet-manager/src/lib.rs) | Reconcile loop, lease renewal, plan execution, apply/delete/send/result/declare verbs | Reads and writes through `Store`; observes and performs effects through `Runtime`; reads changes and logins and performs the one forge write, mark-ready, through `Forge`. |
| [fleet-controllers](../../crates/fleet-controllers/src/lib.rs) | Pure Task, ChangeRequest, Forge, Gateway, Conversation and Listener decisions, and the plugin admission gate | Receives time and observations as values; returns probes and a plan without I/O. |
| [fleet-api](../../crates/fleet-api/src/lib.rs) | Task, ChangeRequest, plan, observation, result and port types | Pure shared contract; no adapter dependencies. |
| [fleet-store](../../crates/fleet-store/src/lib.rs) | File persistence, CAS, write ownership and lease fencing | A corrupt or unreadable record is Unknown; only proven absence is not-found. |
| [fleet-runtime](../../crates/fleet-runtime/src/lib.rs) | Session operations through any program speaking the [runtime protocol](../runtime/README.md#the-runtime-protocol) (`exec`), or the file-backed fake host; the protocol's conformance suite | Converts external answers to Known/Unknown; retains identity and delivery keys; shares no type with any runtime's code. |
| [fleet-forge](../../crates/fleet-forge/src/lib.rs) | Reading one change through `gh`, `glab`, `az`, externally authenticated `curl`, or the file-backed fake forge, and taking a supported draft out of draft after a fresh read | One bounded observation is complete or Unknown; a write either preserves the head/state guard or fails closed; the only crate that knows a forge CLI's name. |
| [fleet-gateway](../../crates/fleet-gateway/src/lib.rs) | Reading and writing one chat thread through Slack's Web API or Discord's REST API by `curl`, or through the file-backed fake platform | Every answer is Known or Unknown; a refused write fails closed; the only crate that knows a chat client's name. |
| [fleet-plugin](../../crates/fleet-plugin/src/lib.rs) | Bounded calls to a registered plugin process through the `Plugins` port | The only crate that runs a plugin; every call is one bounded process through fleet-exec. |
| [fleet-exec](../../crates/fleet-exec/src/lib.rs) | Bounded process execution | The only production process spawner; timeout/unwind kills the process tree. |
| [fleet-telemetry](../../crates/fleet-telemetry/src/lib.rs) | The OpenTelemetry SDK and its OTLP/HTTP exporter, installed when the `OTEL_*` variables ask | The only crate that names the SDK or the exporter; export never changes a command's outcome, and shutdown is bounded. |

The manager records spans and metrics through the `opentelemetry` API,
which does nothing until `fleet-telemetry` installs a provider. The CLI
asks `fleet-telemetry` for each command's root span.
[OpenTelemetry export](configuration.md#opentelemetry-export) owns the variables, spans
and metrics.

`arch-rules` and `fleet-dev` are repository tooling: architecture enforcement,
validation and release packaging. They are not controllers or services in the
installed `fleet` runtime.

The executable dependency graph is [crates/architecture.toml](../../crates/architecture.toml).
The [architecture suite](../../crates/arch-rules/tests/architecture.rs) checks crate
edges, purity, restricted process and telemetry APIs, permitted status
writers and the holders of the Forge port, including negative fixtures. The
CLI may wire adapters, but only manager verbs may perform resource
mutations, and only the manager performs the mark-ready effect.

## Desired state, observations and effects

The [controller index](README.md#controllers) links the controller
guides. The
[Task controller documentation](controllers/task.md) defines
the [resource](resources/task.md),
[state machine](controllers/task.md#task-state-machine) and
[interface](controllers/task.md#task-controller-interface).

One reconcile of a due Task follows this protocol:

```text
read Task + resourceVersion
  -> resolve/freeze referenced execution in private store before first effect
  -> ask pure controller for probes
  -> renew lease before each runtime probe
  -> pass observations + time to pure controller
  -> CAS-write planned status, including effect intent
  -> renew lease before effect -> perform effect -> persist receipt
  -> remove finalizer if requested
```

The controller owns Task status except `status.result`, written through the
worker verb, and `status.inbox`, which nobody owns: `fleet send` appends to
it and the controller's delivery write removes from it, each through a
typed store mutation that changes nothing else. `fleet amend`, and a
plugin's `amends[]` verb, append to `spec.amendments` and queue the
amendment's inbox entry in one such mutation. A status conflict before an
effect drops the plan and retries from fresh state. A receipt conflict
rereads the object and retries the receipt write, preserving concurrent
worker changes. Expired or superseded leases refuse controller writes. An
effect already started cannot be fenced by the file store; deterministic keys
and runtime observation recover that gap.

## Resource representation

[fleet-api](../../crates/fleet-api/src/meta.rs) defines Fleet's resource envelope
and metadata as native Rust types. Manifests use `apiVersion`, `kind`,
`metadata`, `spec` and `status`; typed Task views validate spec and status.
Unknown fields are refused. The store assigns identity, generation, resource
version, timestamps and finalizers, and treats incomplete persisted metadata
as Unknown rather than repairing it with defaults.

The file-store format is version 1. Resource versions are store-wide numeric
write counters used for compare-and-swap; timestamps are Unix milliseconds.
The resource API version is separately `fleet/v1alpha1`. These contracts do
not imply compatibility with Kubernetes metadata or APIs.

The workspace has no kube dependencies. Fleet's current consumers use its
own Store and Runtime ports, so resource validation, persistence, lease
fencing and effect recovery remain explicit Fleet contracts. A familiar
manifest shape does not require a Kubernetes resource library. The bounded
manager's scheduling behavior is defined below, independently of the
resource representation.

## Persistence and execution

The store uses a fresh versioned data directory with a format marker, resource
files, a sequence counter, a lock and the manager lease. Writes use synced
temporary files and atomic publication under an OS file lock. Generation
tracks spec changes; resourceVersion tracks store writes. See the
[store contract](contracts.md#store) for durability and deletion rules.

There is one lease for manager reconciliation. `fleet reconcile` renews it
before Tasks, probes and effects, and releases it on exit when still owned.
A bounded `reconcile` run ends when a pass makes no change or reaches its pass
limit. `fleet up` instead keeps that same manager lease across periodic sweeps,
renewing before each sweep, Task, probe and effect. Retry eligibility remains
persisted as `status.notBefore`; due Tasks progress without agent activity.
Shutdown releases the lease and retains sessions; crash recovery waits for
lease expiry and observes deterministic session/effect keys.

The CLI's `init` writes a local config without Git or network, separate from
resource writes. `up` runs the ordinary Task controller for every due Task
and creates no Task of its own. VEGA, the Mission Control content in its own
repository, is an ordinary Task applied like any other: it has no extra
resource kind, controller or privileges. The
[initialization guide](configuration.md#configuration-and-customization) owns
config precedence and startup and recovery instructions.

The runtime is chosen in versioned TOML config, not a separate resource.
A runtime program owns host connections and session execution. Fleet calls
it through bounded processes; the fake runtime provides persisted fault injection for
isolated tests. Runtime operations report their worst-case duration so the
CLI can reject a lease too short to outlast them.

The runtime reports what an agent is doing, Active or Idle, and pauses or
resumes a session on request. It decides nothing. TaskController decides
when to pause an idle session and when to resume it. A runtime that cannot
pause answers Unknown, and Unknown never pauses. See
[pausing an idle session](controllers/task.md#pausing-an-idle-session).

TaskConfig and EnvProvider are passive data; the manager resolves providers
through the existing runtime port and bounded executor before freezing a
private launch snapshot. TaskController stays pure and owns the lifecycle.
See [execution configuration](resources/task-config.md).

## Sessions and the runtime port

A Session is one running agent: the unit the runtime port starts,
observes, briefs, sends to and releases. Fleet defines it in
[fleet-api's session module](../../crates/fleet-api/src/session.rs) and
documents it on the [Session page](resources/session.md). A
Session is not a stored kind. One Task owns at most one Session, from the
Task's spawn to the Task's release, and names it in `status.session`.

A Session has two parts:

- an identity, `SessionRef`: the runtime's `id`, the `host` and Fleet's
  `key`. The key is deterministic per Task and carries the Task's uid, so a
  second spawn finds the first Session and a recreated Task adopts nothing
  it never owned.
- a lifecycle, `SessionState`: `Running`, `Idle`, `Paused`, `Lost` or
  `Gone`, folded from the runtime's Known answers. TaskController records
  it in `status.sessionState` on every pass that proved something. Unknown
  never changes it. Nothing decides on it; a view reads it.

The runtime port is the only way Fleet reaches a Session. Fleet owns the
Session's identity, its lifecycle record and every decision about it. The
runtime owns the agent process, its window and its conversation, and
answers what it can prove. Fleet makes the workspace through the workspace
port; the runtime runs the session in it.

Fleet owns when workspaces are made and released, the agent's lifecycle hooks
(`fleet signal`), proven delivery and the harness profiles. Every real
runtime is the `exec` runtime kind:
`[runtimes.<name>]` names a program that speaks the
[runtime protocol](../runtime/README.md#the-runtime-protocol),
`[hosts.<name>]` names the runtime serving each host, and `fleet init`
configures `runtime-mux` alone. The port's calls map onto the protocol's
verbs: `spawn` is `start` with the harness profile's launch argv and the
session's `FLEET_SIGNAL_FILE`, `revive` is `start` with its resume argv,
`find`, `lost` and `at_rest` read `inspect`, `release` is `stop`, a brief
is `input`, `create_dir` and `remove_dir` are `exec`, and the workspace
calls are `workspace` and `stop` with `release`. What only the
agent can prove (readiness, workspace trust, delivery, an idle agent)
comes from the agent's signals. `fleet attach` asks the runtime's
`attach` for the command that opens the session's terminal stream.

## Workspaces

A Workspace is the set of files one Session works in. Fleet defines it in
[fleet-api's workspace module](../../crates/fleet-api/src/workspace.rs). A
Workspace is not a stored kind.

```text
Task ──owns──▶ Workspace {key, host, dirs} ◀──runs in── Session
  spec.workspace      status.workspace               status.session
```

- A Task names what its Session works in, in `spec.workspace`: a list of
  absolute URLs.
- A Task owns at most one Workspace, and its one Session runs in it.
- The Workspace key is the Session key, `<name>.<uid>`.
- TaskController is the only writer of `status.workspace`.

The host's runtime makes every Workspace, through its `workspace` verb.
Fleet keeps no workspace adapter and no repository map: the URL is the
whole request. The lifecycle follows the Task:

1. TaskController plans `Materialize` under the workspace key, before the
   first start. The manager performs it as the runtime's `workspace` call.
2. The spawn gets the ready `{host, dirs}`. The Session starts in the first
   directory and spans the others. The runtime makes nothing at the start.
3. After the release, TaskController plans `CleanWorkspace`, the runtime's
   `stop` with `release`. The release finalizer goes only after it has a
   receipt.

An empty list makes no Workspace: the Session starts in the runtime's
default directory.

The workspace calls keep the rules of the runtime port:

- Every call answers Known or Unknown. Unknown never concludes.
- The `workspace` verb is idempotent by key. A lost answer is settled by
  the same call under the same key, which adopts a whole Workspace and
  never starts a new attempt.
- A `workspace` call is whole or nothing. The runtime rolls a partial
  Workspace back on the next call.
- A runtime handles the URL schemes its manifest lists. Fleet refuses any
  other URL before anything is made, and the Task waits with
  `WorkspaceUnsupported`.

The host of a Workspace is observed: `status.workspace.host` is the host
whose runtime made it. Credentials stay with the host and its runtime, and
Fleet sends none.

## Forge integration

[ChangeRequest](resources/change-request.md) and its
[controller](controllers/change-request.md) mirror one change on a
forge through the `Forge` port, with adapters in `fleet-forge` (`github` via
`gh`, `gitlab` via `glab`, Azure DevOps Services via `az`, Bitbucket Cloud via
externally authenticated `curl`, and a fake), and
number every item they see so
consumers diff by one cursor. A ChangeRequest belongs to no Task; it outlives
its followers and is removed by its own controller after a retention period,
by a delete fenced on its `uid`, its `resourceVersion` and the lease. The
crate graph has `fleet-forge` beside `fleet-runtime`, an adapter depending
on `fleet-api` and `fleet-exec`, and the manager a third port. The purity and
process rules above are unchanged: every forge call is one bounded process
through `fleet-exec`, and no forge-specific name appears above the port.

### The Forge controller

The [Forge controller](controllers/forge.md) is a second controller
over the Task. When a worker declares the change it opened, `fleet declare`
ensures the ChangeRequest exists, and the controller follows it: it joins at
the present with one message holding the whole snapshot, then relays what is
new into the Task's inbox by a cursor kept on the Task. It owns
`status.forge` and appends to the Task's inbox through one typed store
mutation that lands the cursor and the entries together; it writes nothing
else of the Task and no ChangeRequest, and the relay is a status write, not
an effect. The manager runs it after the Task controller on each sweep,
under its own due gate, from a fresh read of the Task, answering its object
probes from the store, its login probe through the Forge port and, last,
its at-rest probe through the runtime. The Task inbox the relay lands in,
`status.inbox`, is the one field with no single owner: the write-ownership
rule has that typed exception, and every other field keeps one owner.

Its one effect, and Fleet's one forge write, is mark-ready: when a
configured ready policy holds on a draft change that was never seen ready,
the worker has reported, the session is at rest and the mirror is fresh,
the controller writes the intent on the entry with its status and the
manager performs `MarkReady` through the Forge port, which re-reads the
head and the draft state immediately before the mutation and refuses when
either moved; the receipt lands on the entry, attempts are bounded at three
with the wait on the entry's own `notBefore`, and an unknown outcome is
answered by the change's next fresh observation, never by repeating the
effect. Policies are named sets of a fixed predicate vocabulary in the
native config, selected per Task or by a default. Fleet never puts a change
back into draft and never merges: the forge's mergeability answer is
reported, not acted on.

### The Plugin controller

The [Plugin controller](controllers/plugin.md), a host for controllers
written outside this repository as scripts. A plugin registered under
`[plugins.<name>]` declares its own kinds with a JSON Schema each, which the
store then serves. For each due object of those kinds the manager runs the
plugin's pure `probes` and `reconcile` functions as bounded processes
through the `Plugins` port and a `fleet-plugin` adapter crate, answers
store probes itself and plugin probes through the plugin's bounded `observe`
call, and admits the returned plan through a pure gate in
`fleet-controllers` before writing the plugin's status under field manager
`plugin/<name>`, performing its Task verbs (create, patch `dispatch` and
`forge`, send, declare, delete) and running its keyed effects through the
plugin's `perform` call with intent before and receipt after. The purity and
process rules above are unchanged: the plugin decides, the manager performs,
and every plugin process is one bounded call through `fleet-exec`.

### The Gateway controller

The [Gateway controller](controllers/gateway.md) is the Forge
controller's pattern applied to chat. A listener outside the manager holds a
chat platform's connection and hands each raw event to `fleet gateway
receive`, a bounded manager verb that reads it through the adapter and spools
it on a [Conversation](resources/conversation.md), the mirror of one thread.
ConversationController numbers its messages, applies the route's admission
rules, creates one Task per thread in the thread's own directory, and retires
an idle thread with its Task and that directory; GatewayController, a third
controller over the Task, owns `status.gateway`, relays new messages into the
inbox under a cursor through `record_relay`, and posts what the Task declares
as keyed effects through a `Gateway` port, with adapters in the
`fleet-gateway` crate, for Slack and Discord. ListenerController reports
each account's inbound health from the listener's own beats, on a
[Listener](resources/listener.md). The purity and process rules above are
unchanged: the Gateway runs no model, every adapter call is bounded, and no
platform name appears above the port.

## Scope

The Task controller runs sessions and records worker claims. It does not
verify that an artifact merged, conduct reviews, manage a forge or dispatch
a hierarchy. Registered [controller plugins](controllers/plugin.md) define their
own kinds and act through admitted plans. Connection details stay with the runtime;
credentials stay with the agent and host.

## Views

thurbox is a Fleet client and reads nothing of Fleet's store directly. The
runtime side, the protocol and the terminal stream, is
[Runtimes](../runtime/README.md).

- `fleet watch --json`: a long-running NDJSON stream, one event per changed
  Task, resumable by the store's write counter (`--since`). Its events
  are in [the command line](contracts.md#the-command-line). It feeds
  thurbox's snapshot behind the `SnapshotSource` trait: `FleetWatch`
  (`crates/thurbox/src/kernel/snapshot/fleet.rs`) publishes each Task as a session row.
- `fleet apply`, `delete`, `send`: the interface's commands, behind the
  `Executor` trait. `crates/thurbox/src/kernel/command/fleet.rs` runs them.
  No interface command maps to `amend`.
- `fleet attach <task> --json`: the command that opens the Task's terminal
  stream ([the terminal stream](../runtime/README.md#the-terminal-stream)).
  It asks the runtime's `attach` and wraps a `tty` answer in
  `fleet term-bridge`. thurbox opens the stream it names as a stream pane
  (`crates/thurbox/src/kernel/terminal/fleet.rs`), whose backend is the stream itself.
- thurbox starts `fleet up` when no manager holds the lease, and starts it
  again when it exits. A `fleet up` that exits with
  6 found the lease held, which is no error.

Fleet is thurbox's only engine: `[fleet] program` in thurbox's
`settings.toml` names the binary
([thurbox's configuration](../thurbox/config.md#fleet--the-session-engine)).

An interactive session is a Task with no brief: it spawns and waits for its
user. It stays Spawned with `Briefed=False/Interactive`, and its inbox
is delivered as any other.

## Workspace URLs

A Task names its workspace as a list of URLs. Fleet passes the list to the
runtime and gets directories back. Everything between, how files come to
exist, where they live, what is installed, how they are kept and removed, is
the runtime's. [Runtimes](../runtime/README.md#workspaces) owns that side.

```json
"workspace": [
  "git+https://github.com/acme/app.git#main",
  "git+https://github.com/acme/lib.git",
  "file:///home/me/notes"
]
```

- Each entry is an absolute URL. Fleet checks that, and nothing else: the
  scheme, the reference and any query are the runtime's to read.
- The first entry is where the agent starts; the others are the extra
  directories it spans.
- An empty list means the agent starts in the runtime's default directory.

Fleet makes two calls, each idempotent by the Task's key:

| When | Call | Answer Fleet records |
| --- | --- | --- |
| before the first start | `workspace {key, urls}` | `status.workspace.dirs`: one directory per URL, as the process sees it |
| after the session is released | `stop {key, release: true}` | the workspace is gone |

Fleet builds the harness's argv from `dirs` (the first is the working
directory, the rest go to `additional_dir_args`) and starts the session. A
stop, a pause, a resume or a revive leaves the workspace alone.

The answers map to conditions and nothing more:

- `Unsupported{url}`: the runtime does not handle that URL. Fleet answers
  this itself, before any call, for a scheme the manifest does not list.
  The Task waits with `WorkspaceUnsupported`, spends no attempt, and asks
  again after each wait.
- `Failed{why}`: the runtime made nothing. The Task waits with
  `MaterializeRetry` and asks again after the wait, at most three times;
  then it stalls with `MaterializeFailed`.
- Unknown: Fleet asks again with the same key, which adopts a workspace that
  was made.

A `workspace` call has its own bound, `workspaceTimeoutMs` (600000 when
absent), because a first clone can take minutes. Every other call keeps
`callTimeoutMs`.

### The collector

A deletion releases the session and its workspace once. Whatever that could
not finish (an unreachable host, an Unknown answer, a crash between two
effects) does not keep the Task: its finalizer goes, and the collector takes
over. On each sweep, for each runtime that implements `list`, every key with
no live Task in the store gets `stop {key, release: true}`.

The store proves the Task is absent and the runtime proves the leftover
exists, both Known; Unknown on either side waits for the next sweep. The key
carries the Task's uid, so a recreated Task with the same name is never
mistaken for the old one.
