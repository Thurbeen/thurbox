# Observability

Every program in this repository exports OpenTelemetry the same way. The
standard `OTEL_*` environment variables turn export on and configure it.
No program has a telemetry setting of its own: `fleet.toml` has no
telemetry section, and thurbox's `settings.toml` has none.

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

2. Set the endpoint where you start thurbox or Fleet:

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

## Variables

| Variable | Effect |
| --- | --- |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | The collector's base URL. A program sends to `/v1/traces`, `/v1/metrics` and `/v1/logs` under it. Unset, nothing is exported. |
| `OTEL_EXPORTER_OTLP_{TRACES,METRICS,LOGS}_ENDPOINT` | The full URL for one signal. It overrides the base URL for that signal, and turns that signal on alone. |
| `OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_EXPORTER_OTLP_{TRACES,METRICS,LOGS}_PROTOCOL` | Must be `http/protobuf`, the default. |
| `OTEL_EXPORTER_OTLP_HEADERS` | Headers for each export request, such as an authorization header. |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | The bound of one export request, in milliseconds. The default is 10000. |
| `OTEL_TRACES_EXPORTER`, `OTEL_METRICS_EXPORTER` | `otlp` (the default) or `none`. `none` turns that signal off. |
| `OTEL_LOGS_EXPORTER` | `otlp` turns on thurbox's log export. Unset or `none`, no log is exported. |
| `OTEL_SDK_DISABLED` | `true` turns all export off. |
| `OTEL_SERVICE_NAME` | The `service.name` of the program you start. The programs it starts keep their own names. |
| `OTEL_RESOURCE_ATTRIBUTES` | More resource attributes, as `key=value` pairs separated by commas. Every program in the chain adds them. |
| `OTEL_BSP_*`, `OTEL_BLRP_*`, `OTEL_METRIC_EXPORT_INTERVAL` | The batch and export intervals of the OpenTelemetry SDK. |
| `TRACEPARENT` | The W3C trace context of the caller. A program sets it for each program it starts. Do not set it yourself. |

A signal is exported when an endpoint for it is set, its exporter is unset
or `otlp`, and `OTEL_SDK_DISABLED` is not `true`.

Logs are the exception: an endpoint alone never exports them, because a
log record holds text. Set `OTEL_LOGS_EXPORTER=otlp` to export them. Only
thurbox reads the logs variables. The other programs record no logs and
ignore them. An unsupported logs exporter or protocol turns off the logs of
thurbox only: it never turns off traces or metrics.

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

`OTEL_RESOURCE_ATTRIBUTES` overrides each attribute in this table.
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
| `fleet` | `fleet COMMAND` | `TRACEPARENT`, or none | `fleet.command`, `process.exit.code` |
| `fleet` | `reconcile pass` | `fleet reconcile`, or none under `fleet up` | `fleet.pass` |
| `fleet` | `reconcile KIND` | the object (not exported); links to `reconcile pass` | identity, `fleet.controller`, `fleet.outcome` |
| `fleet` | `effect KIND` | `reconcile KIND` | identity, `fleet.effect`, `fleet.outcome` |
| `fleet` | `forge poll` | `reconcile ChangeRequest` | identity, `fleet.forge.host`, `fleet.outcome` |
| `fleet` | `runtime VERB` (client) | `effect KIND`, or `reconcile KIND` for a probe | `fleet.runtime`, `fleet.runtime.verb`, `fleet.outcome` |
| `runtime-mux` | `runtime-mux VERB` | `TRACEPARENT` | `runtime.verb`, `runtime.outcome` |
| `workspace-git` | `workspace-git VERB` | `TRACEPARENT` | `workspace.verb` |

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

| Metric | Instrument | Unit | Attributes |
| --- | --- | --- | --- |
| `fleet.tasks` | gauge | `{task}` | `fleet.phase` |
| `fleet.sessions` | gauge | `{session}` | `fleet.session.state` |
| `fleet.reconcile.duration` | histogram | `s` | `fleet.controller`, `fleet.outcome` |
| `fleet.effects` | counter | `{effect}` | `fleet.effect`, `fleet.outcome` |
| `fleet.runtime.call.duration` | histogram | `s` | `fleet.runtime`, `fleet.runtime.verb`, `fleet.outcome` |
| `fleet.forge.calls` | counter | `{call}` | `fleet.forge.host`, `fleet.outcome` |
| `fleet.inbox.depth` | gauge | `{entry}` | none |
| `fleet.task.amendments` | counter | `{amendment}` | `fleet.outcome` |

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
- `fleet.inbox.depth` is the number of undelivered inbox entries over all
  Tasks, at the start of each pass.
- `fleet.task.amendments` counts `fleet amend` calls. The outcome is
  `appended`, or the refusal: `precondition`, `conflict`, `notFound`,
  `forbidden`, `invalid` or `error`.

## Logs

Only the thurbox interface exports logs. It sends its own log lines at
`WARN` and above, from thurbox's own code and its multiplexer runtime. The
log file `thurbox.log.<date>` keeps every level, and is the only log when
export is off or the collector is unreachable.

The log lines can name paths, sessions and machines, so log export is
opt-in: thurbox exports them only when `OTEL_LOGS_EXPORTER=otlp` is set.
An endpoint alone, which an agent's own telemetry can set, exports traces
and metrics only.

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
- A session receives only the `OTEL_*` variables that its Task declares in
  `spec.env`, a TaskConfig or an EnvProvider. The agent's own telemetry is
  that Task's configuration.

## What is never exported

The programs export names, identities and outcomes only. These values
never leave a process as telemetry:

- A brief, an inbox message or a worker's result.
- An environment value, or a value that an EnvProvider resolved.
- A runtime request or answer.
- The text of an error or of an Unknown observation.

Thurbox's log records are the exception: they hold the text of its log
lines ([Logs](#logs)).

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

Unset `OTEL_EXPORTER_OTLP_ENDPOINT` and the signal endpoints, or set
`OTEL_SDK_DISABLED=true`. Set `OTEL_TRACES_EXPORTER=none` or
`OTEL_METRICS_EXPORTER=none` to turn off one signal. Logs are off unless
`OTEL_LOGS_EXPORTER=otlp` is set.

When export is off, a program reads the variables once at start. It opens
no connection, starts no telemetry thread, and records into no-op
instruments. No program passes `TRACEPARENT` to another.
