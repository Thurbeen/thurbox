# OpenTelemetry export

Fleet can export traces and metrics to an OpenTelemetry collector over OTLP.
Export is off by default. The standard `OTEL_*` environment variables turn
it on and configure it. `fleet.toml` has no telemetry section.

This page owns the telemetry interface: the variables Fleet reads, the
spans and metrics it exports, and the guarantees of the export.

## Turning export on

Fleet exports a signal when all of these conditions are true:

- An endpoint is set: `OTEL_EXPORTER_OTLP_ENDPOINT`, or the signal's own
  `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` or `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT`.
- The signal's exporter, `OTEL_TRACES_EXPORTER` or `OTEL_METRICS_EXPORTER`,
  is unset or `otlp`. The value `none` turns that signal off.
- `OTEL_SDK_DISABLED` is not `true`.

Each Fleet process reads its own environment. Set the variables where
`fleet up` runs to export the manager's sweeps. Set them for a worker's
session to export that worker's `fleet result` and `fleet declare` calls.

A session never inherits the manager's `OTEL_*` variables. The runtime
removes them from each `thurbox-cli` call. A session receives only the
`OTEL_*` variables that its Task declares in `spec.env`, a TaskConfig or an
EnvProvider.

```sh
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 fleet up
```

## Variables

| Variable | Effect |
| --- | --- |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | The collector's base URL. Fleet sends to `/v1/traces` and `/v1/metrics` under it. |
| `OTEL_EXPORTER_OTLP_{TRACES,METRICS}_ENDPOINT` | The full URL for one signal. It overrides the base URL for that signal. |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | Must be `http/protobuf`, the default. |
| `OTEL_EXPORTER_OTLP_HEADERS` | Headers for each export request, such as an authorization header. |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | The bound of one export request, in milliseconds. The default is 10000. |
| `OTEL_SERVICE_NAME` | The `service.name` resource attribute. The default is `fleet`. |
| `OTEL_RESOURCE_ATTRIBUTES` | More resource attributes, as `key=value` pairs separated by commas. |
| `OTEL_SDK_DISABLED` | `true` turns all export off. |
| `OTEL_TRACES_EXPORTER`, `OTEL_METRICS_EXPORTER` | `otlp` or `none`. |
| `OTEL_BSP_*`, `OTEL_METRIC_EXPORT_INTERVAL` | The batch and export intervals of the OpenTelemetry SDK. |

Fleet sets `service.version` to its own version, unless
`OTEL_RESOURCE_ATTRIBUTES` sets it.

Fleet speaks OTLP over HTTP with protobuf only. A `grpc` or `http/json`
protocol, or an exporter other than `otlp` or `none`, turns export off.
Fleet then prints one `warning: telemetry is off:` line on stderr and runs
the command without telemetry.

This release has no TLS client. Send to a collector on `http://`, such as a
local OpenTelemetry Collector that forwards to a remote backend.

## Traces

Each command is one trace. Its root span is `fleet COMMAND`, for example
`fleet reconcile`. `fleet up` runs until it stops, so each pass of
`fleet up` is a trace of its own, with the `reconcile pass` span as its root.

| Span | Parent | Attributes |
| --- | --- | --- |
| `fleet COMMAND` | none | `fleet.command`, `process.exit.code` |
| `reconcile pass` | `fleet reconcile`, or none under `fleet up` | `fleet.pass` |
| `reconcile KIND` | `reconcile pass` | identity, `fleet.controller`, `fleet.outcome` |
| `effect spawn`, `effect brief`, `effect deliver`, `effect release`, `effect revive`, `effect pause`, `effect resume`, `effect markReady` | `reconcile KIND` | identity, `fleet.effect`, `fleet.outcome` |
| `forge poll` | `reconcile ChangeRequest` | identity, `fleet.forge.host`, `fleet.outcome` |

The identity attributes are the object's `fleet.kind`, `fleet.name`,
`fleet.uid` and `fleet.generation`. Only the Task effects and mark-ready
have a span; the Conversation and Gateway controllers' effects and Task
creation are not traced.

A command with a non-zero exit has the error status. A reconcile that
fails has the error status.

| Span | `fleet.outcome` values |
| --- | --- |
| `reconcile KIND` | `changed`, `unchanged`, `conflict`, `error`, `leaseLost` |
| `effect …` | `spawned`, `adopted`, `delivered`, `queued`, `already`, `released`, `revived`, `paused`, `resumed`, `markedReady`, `moved`, `failed`, `unmapped`, `trustUnmapped`, `unknown` |
| `forge poll` | `known`, `absent`, `timeout`, `incomplete`, `throttled`, `unreadable`, `notLoggedIn`, `refused`, `unavailable` |

The `fleet.controller` values are `TaskController`, `ForgeController`,
`ChangeRequestController`, `ConversationController`, `ListenerController`
and `GatewayController`.

`effect brief` sends the Task's brief. `effect deliver` sends one inbox
entry.

## Metrics

| Metric | Instrument | Unit | Attributes |
| --- | --- | --- | --- |
| `fleet.reconcile.duration` | histogram | `s` | `fleet.controller`, `fleet.outcome` |
| `fleet.effects` | counter | `{effect}` | `fleet.effect`, `fleet.outcome` |
| `fleet.forge.calls` | counter | `{call}` | `fleet.forge.host`, `fleet.outcome` |
| `fleet.inbox.depth` | gauge | `{entry}` | none |
| `fleet.task.amendments` | counter | `{amendment}` | `fleet.outcome` |

`fleet.forge.calls` counts the change polls that Fleet sent to a forge. A
poll that the forge throttled has the outcome `throttled`. A poll that
Fleet deferred after a throttle on the same host is not a call.

`fleet.inbox.depth` is the number of undelivered inbox entries over all
Tasks, at the start of each pass.

`fleet.task.amendments` counts `fleet amend` calls. The outcome is
`appended`, or the refusal: `precondition`, `conflict`, `notFound`,
`forbidden`, `invalid` or `error`. The delivery of an amendment is an
effect, counted in `fleet.effects` as `deliver`, on a span that carries
`fleet.generation`.

## What Fleet never exports

Fleet exports names, identities and outcomes only. These values never
leave the process as telemetry:

- A brief, an inbox message or a worker's result.
- An environment value, or a value that an EnvProvider resolved.
- The text of an error or of an Unknown observation.

## Guarantees

- Telemetry never changes what a command does or its exit code.
- An unreachable collector never fails a command.
- The SDK batches spans and points on its own threads, in bounded queues.
  When a queue is full, Fleet drops new spans.
- The exporter retries a failed request inside that request's timeout.
  After the timeout, Fleet drops the batch. Fleet never writes a batch to
  disk.
- At exit, Fleet flushes the batched spans and points. The flush waits at
  most 2 seconds. Ctrl-C on `fleet up` flushes the same way.
- Without an endpoint, or with `OTEL_SDK_DISABLED=true`, Fleet opens no
  connection and starts no telemetry thread.
