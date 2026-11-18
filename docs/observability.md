# Observability

Every program in this repository exports OpenTelemetry the same way. Two
sources configure the export:

- The `[telemetry]` table of a config file: Fleet's `config.toml` for Fleet
  and each program and session that Fleet starts, and thurbox's
  `settings.toml` for the interface ([Config files](#config-files)).
- The standard `OTEL_*` environment variables
  ([Variables](#variables)).

An `OTEL_*` variable that is set wins over the file's value for that
variable ([Precedence](#precedence)).

This page owns the telemetry interface of the whole system: the variables,
the spans, the metrics, the logs, and how trace context crosses from one
program to the next. The `otel-env` crate implements it
([source](../crates/otel-env/src/lib.rs)).

## Turn it on

1. Start a collector that receives OTLP over HTTP on port 4318. Jaeger
   receives OTLP and shows traces:

   ```sh
   docker run --rm -p 16686:16686 -p 4318:4318 jaegertracing/jaeger:2.23.0
   ```

   To see metrics and logs too, use an
   [OpenTelemetry Collector](https://opentelemetry.io/docs/collector/) that
   forwards each signal to your backend.

2. Set the endpoint in Fleet's `config.toml` and in thurbox's
   `settings.toml`:

   ```toml
   [telemetry]
   endpoint = "http://localhost:4318"
   ```

   Or set the variable where you start thurbox or Fleet:

   ```sh
   export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
   thurbox            # or: fleet up
   ```

3. Open <http://localhost:16686>. Search the service `fleet` for the tag
   `fleet.name=<task>` to find the trace of one Task.

Each program reads its own environment. A program that another program
starts inherits the variables, so one `export` before `thurbox` or
`fleet up` turns on the whole chain: thurbox, the `fleet` commands it runs,
the `runtime-mux` calls Fleet makes, and the `workspace-git` calls
`runtime-mux` makes.

A variable reaches only the processes started below the place where you set
it. A `fleet` command in another shell, or a thurbox started from a desktop
launcher, does not get it. The config file reaches each of them: each
`fleet` command reads Fleet's `config.toml`, and the interface reads its
`settings.toml`.

## Config files

Fleet's `config.toml` and thurbox's `settings.toml` each have a
`[telemetry]` table with the same keys. Fleet's table configures `fleet up`,
each `fleet` command, the `runtime-mux` and `workspace-git` calls that Fleet
makes, and the telemetry of each agent that Fleet starts
([Coding agents](#coding-agents)). thurbox's table configures the interface
only. thurbox never reads Fleet's table, and Fleet never reads thurbox's.

```toml
[telemetry]
endpoint = "http://localhost:4318"
protocol = "http/protobuf"
headers = { file = "/run/user/1000/otel-headers" }
traces = true
metrics = true
logs = false
resource = { "service.namespace" = "fleet", "deployment.environment.name" = "local" }
```

| Key | Default | Variable it stands for | Meaning |
| --- | --- | --- | --- |
| `endpoint` | Absent: nothing is exported | `OTEL_EXPORTER_OTLP_ENDPOINT` | The collector's base URL. It must start with `http://`. |
| `protocol` | `http/protobuf` | `OTEL_EXPORTER_OTLP_PROTOCOL` | Must be `http/protobuf`. |
| `headers` | Absent | `OTEL_EXPORTER_OTLP_HEADERS` | `{ file = "<absolute path>" }`: a file that holds the headers, as `key=value` pairs separated by commas. The program reads the file at each start. |
| `traces` | `true` | `OTEL_TRACES_EXPORTER` | `false` turns traces off. |
| `metrics` | `true` | `OTEL_METRICS_EXPORTER` | `false` turns metrics off. |
| `logs` | `false` | `OTEL_LOGS_EXPORTER` (thurbox only) | `true` turns on the log export of thurbox, `fleet` and the `runtime-mux` that Fleet starts ([Logs](#logs)). In Fleet's table it is the only switch: `fleet` and its `runtime-mux` ignore `OTEL_LOGS_EXPORTER`. |
| `resource` | Absent | `OTEL_RESOURCE_ATTRIBUTES` | More resource attributes, as a table of strings. |

Rules:

- Put no secret in the table. Put a header that holds a secret in the
  headers file, and give the file mode `0600`. A secret manager can write
  the file, for example `systemd-creds` or `sops`.
- `resource` cannot set `service.name`. Each program keeps its own name.
  Use `service.namespace` to group the programs of one deployment.
- A key or value in `resource` holds no `,`. A key holds no `=` and has
  no space at its start or end.
- Fleet refuses a config whose table breaks a rule or has an unknown key.
  The command fails and names the key. A command that does not read the
  whole config, such as `fleet version`, runs, exports nothing, and writes
  one `warning: telemetry is off:` line that names the key. The
  `OTEL_*` variables do not turn export on in that case.
- thurbox reports an unknown key at startup, and
  `thurbox-cli config validate` fails on it. A table that breaks a rule
  turns the interface's export off, and the interface writes the reason to
  its log file.
- An unreadable headers file turns the program's export off, also when an
  `OTEL_*` variable names an endpoint. The programs and sessions that it
  starts export nothing either. The program writes one
  `warning: telemetry is off:` line.
- A program reads the table at start. A change applies at the next start
  of the program. `fleet up` gives a session the values in force at each
  start of the session.

`fleet doctor` shows Fleet's export in force, and where each value comes
from: `config file`, a variable's name, or `default`. `thurbox-cli doctor`
shows the interface's export the same way. Neither shows a header's value.

```text
ok    telemetry    export       endpoint http://localhost:4318 (config file); traces on (default); metrics on (default); logs off (default); headers (config file); resource service.namespace (config file)
```

### Precedence

- An `OTEL_*` variable that is set and not empty wins over the file's value
  for that variable. The other values of the file stay in force. The
  exception is `OTEL_LOGS_EXPORTER`: `fleet` and its `runtime-mux` ignore it
  and read only the `logs` key.
- `logs = true` decides whether `fleet` exports logs, not where. An
  endpoint variable in the environment of a `fleet` command, such as one an
  agent's settings set for its hooks, wins over the file's `endpoint` for
  log records too.
- The file's headers go only to the file's endpoint. When a variable sets
  `OTEL_EXPORTER_OTLP_ENDPOINT` or the endpoint of one signal, the program
  does not use the file's headers.
- `OTEL_RESOURCE_ATTRIBUTES` replaces the whole `resource` table.
- `OTEL_SDK_DISABLED=true` turns all export off, also when the file sets an
  endpoint.

Fleet gives each program that it starts the file's values that are in
force, as variables. These are `runtime-mux` and the `workspace-git` that
`runtime-mux` starts. These programs read no config file of their own.

## Variables

| Variable | Effect |
| --- | --- |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | The collector's base URL. A program sends to `/v1/traces`, `/v1/metrics` and `/v1/logs` under it. Unset, nothing is exported. |
| `OTEL_EXPORTER_OTLP_{TRACES,METRICS,LOGS}_ENDPOINT` | The full URL for one signal. It overrides the base URL for that signal, and turns that signal on alone. |
| `OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_EXPORTER_OTLP_{TRACES,METRICS,LOGS}_PROTOCOL` | Must be `http/protobuf`, the default. |
| `OTEL_EXPORTER_OTLP_HEADERS` | Headers for each export request, such as an authorization header. |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | The bound of one export request, in milliseconds. The default is 10000. |
| `OTEL_TRACES_EXPORTER`, `OTEL_METRICS_EXPORTER` | `otlp` (the default) or `none`. `none` turns that signal off. |
| `OTEL_LOGS_EXPORTER` | `otlp` turns on thurbox's log export. Unset or `none`, thurbox exports no log. `fleet` and the `runtime-mux` it starts ignore it, and read only Fleet's `logs` key. |
| `OTEL_SDK_DISABLED` | `true` turns all export off. |
| `OTEL_SERVICE_NAME` | The `service.name` of the program you start. The programs it starts keep their own names. |
| `OTEL_RESOURCE_ATTRIBUTES` | More resource attributes, as `key=value` pairs separated by commas. Every program in the chain adds them. |
| `OTEL_BSP_*`, `OTEL_BLRP_*`, `OTEL_METRIC_EXPORT_INTERVAL` | The batch and export intervals of the OpenTelemetry SDK. |
| `TRACEPARENT` | The W3C trace context of the caller. A program sets it for each program it starts. Do not set it yourself. |

A signal is exported when an endpoint for it is set, its exporter is unset
or `otlp`, and `OTEL_SDK_DISABLED` is not `true`.

Logs are the exception: an endpoint alone never exports them, because a
log record holds text. Set `OTEL_LOGS_EXPORTER=otlp` to export them.
thurbox reads `OTEL_LOGS_EXPORTER`. `fleet` reads only the `logs` key of
its `[telemetry]` table, and ignores the variable: an agent's own telemetry
sets it in the agent's session, where hooks and agents run `fleet`. Fleet
gives each runtime program `OTEL_LOGS_EXPORTER` from its `logs` key, `otlp`
or `none`, in place of the one it inherited. `workspace-git` records no logs
and ignores them. An unsupported logs exporter or protocol
turns off the logs of that program only: it never turns off traces or
metrics. `fleet` and `runtime-mux` write no line for it; `fleet doctor` reports
it.

The programs speak OTLP over HTTP with protobuf only. A `grpc` or
`http/json` protocol, or an exporter other than `otlp` or `none`, for
traces or metrics turns all export off. The program then writes one `warning: telemetry is off:` line
(thurbox writes it to its log file) and runs without telemetry.

The programs have no TLS client. Send to a collector on `http://`, such as
a local OpenTelemetry Collector that forwards to a remote backend.

## Resource

| Program | `service.name` | Other attributes |
| --- | --- | --- |
| `fleet` | `fleet` | `service.version` |
| `runtime-mux` | `runtime-mux` | `service.version` |
| `workspace-git` | `workspace-git` | `service.version` |
| `thurbox` (the interface) | `thurbox` | `service.version`, `thurbox.instance` (the multiplexer socket, which tells a sandbox apart from the real instance) |
| `thurbox-cli` | exports nothing | — |

The `resource` table of the `[telemetry]` table adds attributes, such as
`service.namespace` and `deployment.environment.name`, to every program
that reads the table. The attributes are on every signal of the program:
spans, metrics and logs. Fleet's table reaches each `fleet` command,
`fleet up`, and the `runtime-mux` and `workspace-git` that Fleet starts.
thurbox's table reaches the interface's spans and log records. Set
`service.namespace` in both tables to group thurbox and Fleet.

`OTEL_RESOURCE_ATTRIBUTES` overrides each attribute in this table, and
replaces the `resource` table ([Precedence](#precedence)).
`OTEL_SERVICE_NAME` overrides `service.name` of the program you start
only: a program removes it from the environment of each program it starts.

## Traces

### One trace per object

Every reconcile of one object is in one trace. The trace ID comes from the
object's `uid`, so the trace holds every reconcile of a Task from its spawn
to its release, over all `fleet reconcile` commands and `fleet up` passes.
The trace also holds the Task's effects, the runtime calls of those
effects, the `runtime-mux` spans of those calls, and the `workspace-git`
spans under them.

The parent of each `reconcile KIND` span stands for the object. Fleet
never exports that parent, so a trace view shows the reconciles as the top
spans of the trace. Each `reconcile KIND` span links to the
`reconcile pass` span that ran it.

A command is a trace of its own: `fleet apply`, `fleet send`,
`fleet delete`. When thurbox runs the command, the command's trace starts
at thurbox's `fleet COMMAND` span.

Without telemetry, no reconcile carries an object's trace context.

### Trace propagation

```text
thurbox ──TRACEPARENT──▶ fleet COMMAND
fleet: effect KIND ─▶ runtime VERB ──TRACEPARENT──▶ runtime-mux VERB
runtime-mux: runtime-mux workspace ──TRACEPARENT──▶ workspace-git make
```

- A program puts the current span in `TRACEPARENT` for each program it
  starts. The format is the W3C `traceparent` value,
  `00-<trace id>-<span id>-<flags>`.
- A program starts its root span under the span `TRACEPARENT` names. An
  invalid value is ignored, and the span is a root.
- `fleet up` ignores `TRACEPARENT`. Each pass is a trace of its own.
- `TRACEPARENT` does not cross ssh or WSL. A Fleet on another machine
  starts its own traces.

### Spans

| Program | Span | Parent | Attributes |
| --- | --- | --- | --- |
| `thurbox` | `fleet COMMAND` (client) | none | `fleet.command` |
| `fleet` | `fleet COMMAND` | `TRACEPARENT`, or none | `fleet.command`, `process.exit.code`, and `fleet.kind` and `fleet.name` when the command names them |
| `fleet` | `reconcile pass` | `fleet reconcile`, or none under `fleet up` | `fleet.pass` |
| `fleet` | `reconcile KIND` | the object (not exported); links to `reconcile pass` | identity, `fleet.controller`, `fleet.outcome` |
| `fleet` | `effect KIND` | `reconcile KIND` | identity, `fleet.effect`, `fleet.outcome` |
| `fleet` | `forge poll` | `reconcile ChangeRequest` | identity, `fleet.forge.host`, `fleet.outcome` |
| `fleet` | `runtime VERB` (client) | `effect KIND`, or `reconcile KIND` for a probe | `fleet.runtime`, `fleet.runtime.verb`, `fleet.outcome` |
| `runtime-mux` | `runtime-mux VERB` | `TRACEPARENT` | `runtime.verb`, `runtime.outcome` |
| `workspace-git` | `workspace-git VERB` | `TRACEPARENT` | `workspace.verb` |

A `fleet COMMAND` span carries `fleet.kind` and `fleet.name` as follows:

- `get`, `status`, `label` and `delete`: the kind and the name they are
  given. A built-in kind is spelt as the store serves it, such as `Task`.
  A plugin's kind is spelt as the command gives it.
- `list`: the kind only.
- `send`, `amend`, `result`, `declare`, `attach`, `shell`,
  `event subscribe` and `event unsubscribe`: `Task` and the Task's name.
- `event publish`, `event list`, `event wait` and `event watch`: `Board`
  and the Board's name.
- Every other command names no one object and carries neither.

The identity attributes are the object's `fleet.kind`, `fleet.name`,
`fleet.uid` and `fleet.generation`. `KIND` in `effect KIND` is one of
`spawn`, `brief`, `deliver`, `release`, `revive`, `pause`, `resume`,
`markReady`, `materialize` and `cleanWorkspace` for a Task, and `create`
and `retireScheduled` for a CronTask. `VERB` is a runtime
protocol verb ([runtimes](runtime/README.md#the-runtime-protocol)) or a
`workspace-git` verb (`make`, `release`, `discard`, `inspect`).

| Span | Outcome values |
| --- | --- |
| `reconcile KIND` | `changed`, `unchanged`, `conflict`, `error`, `leaseLost` |
| `effect KIND` | `spawned`, `adopted`, `delivered`, `queued`, `already`, `released`, `revived`, `paused`, `resumed`, `markedReady`, `moved`, `materialized`, `created`, `deleted`, `workspaceBusy`, `failed`, `trustUnmapped`, `unknown` |
| `forge poll` | `known`, `absent`, `timeout`, `incomplete`, `throttled`, `unreadable`, `notLoggedIn`, `refused`, `unavailable` |
| `runtime VERB`, `runtime-mux VERB` | `known`, `unknown` |

The `fleet.controller` values are `TaskController`, `ForgeController`,
`ChangeRequestController`, `ConversationController`, `ListenerController`,
`CronTaskController` and `GatewayController`.

These spans have the error status: a command with a non-zero exit, a
reconcile that fails, a runtime call that answers Unknown, and a
`runtime-mux` or `workspace-git` call that answers Unknown.

The Conversation and Gateway controllers' effects, gateway calls and Task
creation by `fleet apply` have no span. A CronTask's Task creation is the
`effect create` span.

## Metrics

Only `fleet` exports metrics.

thurbox 3 exports no metrics of its own, by design. thurbox 2 exported
session metrics: state transitions, lifecycle events and turn durations. In
thurbox 3, Fleet owns the sessions, so `fleet.tasks` and `fleet.sessions`
cover them.

| Metric | Instrument | Unit | Attributes |
| --- | --- | --- | --- |
| `fleet.tasks` | gauge | `{task}` | `fleet.phase` |
| `fleet.sessions` | gauge | `{session}` | `fleet.session.state` |
| `fleet.reconcile.duration` | histogram | `s` | `fleet.controller`, `fleet.outcome` |
| `fleet.effects` | counter | `{effect}` | `fleet.effect`, `fleet.outcome` |
| `fleet.runtime.call.duration` | histogram | `s` | `fleet.runtime`, `fleet.runtime.verb`, `fleet.outcome` |
| `fleet.forge.calls` | counter | `{call}` | `fleet.forge.host`, `fleet.outcome` |
| `fleet.inbox.depth` | gauge | `{entry}` | `fleet.name`, `fleet.inbox.state` |
| `fleet.task.amendments` | counter | `{amendment}` | `fleet.outcome` |
| `fleet.manager.info` | gauge | `{manager}` | `fleet.manager.version`, `process.pid`, `host.name`, `fleet.manager.lease` |

- `fleet.tasks` is the number of Tasks in each phase at the start of each
  pass: `Pending`, `Spawning`, `Spawned`, `Briefed`, `Stalled`,
  `Concluded` and `Releasing`. A phase with no Task reports 0.
- `fleet.sessions` is the number of Task sessions in each state at the
  start of each pass: `Running`, `Idle`, `Paused`, `Lost` and `Gone`. The
  `Paused` point is the number of paused sessions.
- `fleet.runtime.call.duration` measures each runtime protocol call. Its
  count is the number of calls. The outcome `unknown` is a call that
  failed: the program did not start, timed out, exited non-zero, or
  answered Unknown.
- `fleet.forge.calls` counts the change polls that Fleet sent to a forge. A
  poll that the forge throttled has the outcome `throttled`. A poll that
  Fleet deferred after a throttle on the same forge is not a call.
- `fleet.inbox.depth` is the number of undelivered inbox entries of each
  Task, at the start of each pass. `fleet.name` is the Task.
  `fleet.inbox.state` is `pending` for an entry that Fleet still delivers,
  and `failed` for an entry that Fleet gave up on. A backlog alert filters
  on `fleet.inbox.state="pending"`.
- A Task with an empty inbox reports 0 in both states. When a Task is
  gone, the process that reported it reports 0 in the next export, and then
  stops reporting it. A gone Task's series ends there, so the gauge does
  not keep its old backlog. If that one export fails, the backend keeps the
  old value until it drops the series.
- A `fleet reconcile` that ends reports the last pass it ran. A later
  process does not report a Task that is already gone.
- `fleet.task.amendments` counts `fleet amend` calls. The outcome is
  `appended`, or the refusal: `precondition`, `conflict`, `notFound`,
  `forbidden`, `invalid` or `error`.
- `fleet.manager.info` is 1 for each `fleet up` process.
  `fleet.manager.version` is the binary's version. `host.name` is the
  machine's host name. `fleet.manager.lease` is `true` while the process
  holds the manager lease. A `fleet up` reports `false` until it takes the
  lease, for example while it waits for another manager, and again after it
  gives the lease back. The point with the other lease value is 0, so one
  process never shows as both.

### Suggested alerts

The examples are PromQL. A Prometheus backend changes each `.` in a name
to `_`, for example `fleet_manager_info` and `fleet_manager_lease`. Other
backends use the same filters and sums. The series of a process that ended
stays in a backend for a short time. Give each alert a hold time, for example
`for: 10m` in Prometheus, so a manager that restarted a moment ago does not
fire it.

Each Fleet has its own lease. When more than one Fleet exports to one
backend, give each Fleet its own `service.namespace`. Prometheus puts
`service.namespace` and `service.name` in the `job` label, so the examples
group by `job`.

More than one lease holder, or more than one manager on one host:

```promql
sum by (job) (fleet_manager_info{fleet_manager_lease="true"}) > 1
  or
sum by (job, host_name) (fleet_manager_info) > 1
```

More than one version across the managers:

```promql
count by (job) (count by (job, fleet_manager_version) (fleet_manager_info == 1)) > 1
```

## Logs

Three programs export logs:

| Program | What it sends | Severity |
| --- | --- | --- |
| The thurbox interface | Its own log lines, from thurbox's own code and its multiplexer runtime. | `WARN` and above |
| `fleet` | One record for each `warning:` and `error:` line it writes to stderr. The body is the text after the prefix. | `WARN` for a `warning:` line, `ERROR` for an `error:` line |
| `runtime-mux` | Its own `tracing` events, from its own code only. | `WARN` and above |

thurbox's log file `thurbox.log.<date>` keeps every level, and is the only
log when export is off or the collector is unreachable. `fleet` adds no
line for a record: the record and the line have the same text. The
`warning: telemetry is off:` lines come before the logs signal exists, so
`fleet` does not export them. `runtime-mux` writes nothing new to stderr:
with logs off, it installs no log handler.

`runtime-mux` reads no config file. The `logs` key in Fleet's `[telemetry]`
table reaches it as `OTEL_LOGS_EXPORTER`, with the rest of the table
([Config files](#config-files)).

The log lines can name paths, sessions and machines, so log export is
opt-in. `fleet` and its `runtime-mux` export them only when Fleet's
`[telemetry]` table has `logs = true`. thurbox exports them when
`OTEL_LOGS_EXPORTER=otlp` is set, or its table has `logs = true`. An
endpoint alone, which an agent's own telemetry can set, exports traces and
metrics only.

`thurbox-cli` exports nothing. Each agent hook runs it, and a network
round trip on each hook costs too much
([ADR-38](thurbox/architecture.md#adr-38-the-interfaces-own-telemetry-reads-the-standard-otel_-variables)).

## Sessions

A session never inherits the telemetry of the program that started it.

- `runtime-mux` removes every `OTEL_*` variable, `TRACEPARENT` and
  `TRACESTATE` from each multiplexer command. The multiplexer server and
  its windows never see them.
- It removes them as well from each command it runs on a remote place
  through ssh or `wsl.exe`: an `exec`, a readiness probe, a copy of a
  harness's files. A `SendEnv` or `WSLENV` setting cannot pass them on.
- A session receives only two sets of `OTEL_*` variables: the ones that its
  Task declares in `spec.env`, a TaskConfig or an EnvProvider, and the ones
  of the agent's own telemetry ([Coding agents](#coding-agents)). Fleet
  gives each of them in the runtime's `start` request, after the strip.
- A session never receives `TRACEPARENT` or `TRACESTATE`. An agent starts
  its own traces.

## Coding agents

When Fleet exports telemetry, each agent that Fleet starts exports its own
telemetry to the same collector. The agent's harness profile tells Fleet
how to turn on the agent's telemetry
([profile format](fleet/controllers/task.md#profile-format)).

Fleet turns on an agent's telemetry when all of these are true:

- Fleet exports traces or metrics ([Variables](#variables)).
- Fleet has a base endpoint: `endpoint` in its `[telemetry]` table, or
  `OTEL_EXPORTER_OTLP_ENDPOINT`. A signal's own endpoint does not name a
  collector for the agent.
- The harness profile has a `[telemetry]` table.
- The Task, or its TaskConfig, does not set `agentTelemetry` to `false`.
- The Task's environment names no collector of its own: no variable whose
  name holds `OTLP` and ends in `ENDPOINT`, in any case. Examples are
  `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` and
  `QWEN_TELEMETRY_OTLP_ENDPOINT`. Such a Task gets none of Fleet's
  collector values: not the endpoint, and not the headers.

Fleet gives the agent:

- the collector's base URL, from `OTEL_EXPORTER_OTLP_ENDPOINT` or the
  table's `endpoint`;
- the protocol `http/protobuf`, when the agent reads one;
- the headers, from `OTEL_EXPORTER_OTLP_HEADERS` or the table's headers
  file, as a variable only;
- the resource attributes `service.name=<harness>`,
  `fleet.task=<Task name>` and `fleet.uid=<Task uid>`, through
  `OTEL_RESOURCE_ATTRIBUTES`.

To find the telemetry of one Task's agent, search for the resource
attribute `fleet.task=<Task name>`.

Fleet adds these variables and arguments at each start of the session: a
spawn, a revive and a resume. So a start after a change of Fleet's
collector gets the new endpoint and headers. The launch snapshot does not
keep them. The Task's own environment wins over each variable.

| Harness | Supported | What Fleet sets | Content switches left off | Source |
| --- | --- | --- | --- | --- |
| `claude` | yes: metrics and events (logs) | `CLAUDE_CODE_ENABLE_TELEMETRY=1`, `OTEL_METRICS_EXPORTER=otlp`, `OTEL_LOGS_EXPORTER=otlp`, `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME=claude`, `OTEL_RESOURCE_ATTRIBUTES` | `OTEL_LOG_USER_PROMPTS`, `OTEL_LOG_ASSISTANT_RESPONSES`, `OTEL_LOG_TOOL_DETAILS`, `OTEL_LOG_TOOL_CONTENT`, `OTEL_LOG_RAW_API_BODIES`. Its beta traces stay off. | [Monitoring](https://code.claude.com/docs/en/monitoring-usage) |
| `codex` | yes: logs, traces and metrics | `-c otel.exporter=…`, `-c otel.trace_exporter=…` and `-c otel.metrics_exporter=…`, each `otlp-http` with the signal's URL and `protocol="binary"`; `OTEL_RESOURCE_ATTRIBUTES` | `otel.log_user_prompt`, `otel.log_agent_responses` | [Configuration](https://learn.chatgpt.com/docs/config-file/config-advanced), [source](https://github.com/openai/codex/blob/main/codex-rs/config/src/types.rs) |
| `pi` | no | nothing | — | [pi variables](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/environment-variables.md) |
| `opencode` | yes: traces and logs | `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_RESOURCE_ATTRIBUTES` | `experimental.openTelemetry` (its AI SDK spans) | [source](https://github.com/anomalyco/opencode/blob/v1.19.5/packages/core/src/observability/otlp.ts) |
| `kilo` | yes, but Fleet sets nothing | nothing | Its AI SDK spans are on by default, and Fleet cannot turn them off at launch. | [source](https://github.com/Kilo-Org/kilocode/blob/main/packages/core/src/observability/otlp.ts) |
| `omp` | yes: traces, logs and metrics | `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME=omp`, `OTEL_RESOURCE_ATTRIBUTES` | `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT` | [omp variables](https://github.com/can1357/oh-my-pi/blob/main/docs/environment-variables.md) |
| `qwen` | yes: traces, logs and metrics | `QWEN_TELEMETRY_ENABLED=true`, `QWEN_TELEMETRY_OTLP_ENDPOINT`, `QWEN_TELEMETRY_OTLP_PROTOCOL=http`, `QWEN_TELEMETRY_LOG_PROMPTS=false`, `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME=qwen`, `OTEL_RESOURCE_ATTRIBUTES` | `QWEN_TELEMETRY_LOG_PROMPTS`, which is on by default: Fleet sets it to `false` | [Telemetry](https://github.com/QwenLM/qwen-code/blob/v0.27.4/docs/developers/development/telemetry.md) |
| `copilot` | yes: traces and metrics | `COPILOT_OTEL_ENABLED=true`, `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME=copilot`, `OTEL_RESOURCE_ATTRIBUTES` | `OTEL_INSTRUMENTATION_GENAI_CAPTURE_CONTENT` | `copilot help monitoring`, [changelog](https://github.com/github/copilot-cli/blob/main/changelog.md) |
| `kimi` | no | nothing | — | [kimi-cli telemetry](https://github.com/MoonshotAI/kimi-cli/blob/main/src/kimi_cli/telemetry/transport.py) |
| any other | no | nothing | — | — |

Notes:

- Codex reads no `OTEL_EXPORTER_OTLP_*` variable. Fleet gives it no
  headers: in an argument, a header is visible to each user of the
  machine. Use a collector that needs no headers, or give Codex its headers
  in its own `config.toml`.
- Codex documents no resource variable. Its OpenTelemetry SDK can read
  `OTEL_RESOURCE_ATTRIBUTES`, and its service name stays `codex-cli`.
- Claude Code documents its service name as `claude-code`. Fleet sets
  `OTEL_SERVICE_NAME=claude` and `service.name=claude`.
- qwen's `http` protocol is OTLP over HTTP with JSON. A collector that
  receives OTLP over HTTP takes both encodings on one port.
- opencode exports over HTTP. Its encoding is the default of its
  exporter.
- The research behind this table is of 2026-11-15. A harness can change
  its telemetry in a later release.

### Privacy

An agent's telemetry is not Fleet's. Each agent decides what it exports.
Some agents can export the text of a prompt, a response or a tool call.
Fleet leaves each switch that exports such content off, or sets it to off
when the agent turns it on by default. To export content, set the agent's
switch in the Task's `spec.env`, its TaskConfig or an EnvProvider.

A collector receives the agent's metrics, events and spans. These name the
model, the tools, the token counts and the costs of each turn. They can
also name paths and commands.

### Turn agent telemetry off

- For one Task, set `"agentTelemetry": false` in its spec.
- For the Tasks of a TaskConfig, set `"agentTelemetry": false` in that
  TaskConfig. The Task's own field wins.
- For every Task that has no `configRef`, set `"agentTelemetry": false` in
  the [default TaskConfig](fleet/resources/task-config.md#the-default-taskconfig).
  `fleet init` writes `default` without the field, so agent telemetry is on.
- To turn off Fleet's own telemetry too, see [Turn it off](#turn-it-off).

## What is never exported

The programs export names, identities and outcomes only. These values
never leave a process as telemetry:

- A brief, an inbox message or a worker's result.
- An environment value, or a value that an EnvProvider resolved.
- A runtime request or answer.
- The text of an error or of an Unknown observation.

Log records are the exception: they hold the text of thurbox's and
`runtime-mux`'s log lines, and of `fleet`'s `warning:` and `error:` lines,
which can include the text of an error ([Logs](#logs)). `fleet` puts no
brief, inbox message or environment value in a record: an object, status or
result that does not parse is reported by the path that does not fit and
the type expected there, never by the value found. An agent's own telemetry is the agent's, not
Fleet's ([Privacy](#privacy)).

## Guarantees

- Telemetry never changes what a program does or its exit code.
- An unreachable collector never fails a command or a call.
- The SDK batches spans, points and records on its own threads, in bounded
  queues. When a queue is full, the program drops new items.
- The exporter retries a failed request inside that request's timeout.
  After the timeout, the program drops the batch. No program writes a batch
  to disk.
- `fleet up` and the thurbox interface run until they stop. At exit they
  flush their batches for at most 2 seconds. Ctrl-C on `fleet up` flushes
  the same way.
- A one-shot `fleet` command, `runtime-mux` and `workspace-git` answer one
  call of their caller. Each one writes its answer first, then flushes for
  at most 100 milliseconds. `runtime-mux` and `workspace-git` flush after
  they release their locks. A stalled collector never makes a call time
  out. Spans still unsent after 100 milliseconds are dropped.

## Turn it off

Remove `endpoint` from the `[telemetry]` table. Unset
`OTEL_EXPORTER_OTLP_ENDPOINT` and the signal endpoints, or set
`OTEL_SDK_DISABLED=true`. Set `OTEL_TRACES_EXPORTER=none` or
`OTEL_METRICS_EXPORTER=none` to turn off one signal. Logs are off unless
`logs = true` is in the `[telemetry]` table, or, for thurbox,
`OTEL_LOGS_EXPORTER=otlp` is set. Remove `logs = true` from Fleet's table to
turn off the logs of `fleet` and `runtime-mux`.

When export is off, a program reads the variables once at start. It opens
no connection, starts no telemetry thread, and records into no-op
instruments. No program passes `TRACEPARENT` to another.
