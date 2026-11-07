# Fleet documentation

Fleet is the engine behind thurbox: it persists Tasks and the other resource
kinds and drives their controllers, one agent session per Task. This directory
is Fleet's own documentation, imported from
[`Thurbeen/fleet`](https://github.com/Thurbeen/fleet) at commit
[`961da39`](https://github.com/Thurbeen/fleet/tree/961da39903338f3fb0493ec5a34aa8417cb98d89)
(2026-11-06) ahead of the crates themselves, which
[step 3 of the migration](../FLEET-ENGINE.md#migration) brings here with
their history. Until then every link to Fleet source is pinned to that commit;
the import turns them back into relative links. How the two fit together is
[Fleet as thurbox's engine](../FLEET-ENGINE.md).

## Where to start

| Read | Answers |
| --- | --- |
| [Architecture](ARCHITECTURE.md) | What are the components, their boundaries, the ports and the execution flow? |
| [Contracts](CONTRACTS.md) | What does each part guarantee, and which tests hold it to that? |
| [Controllers](controllers/README.md) | How does each resource move through its state machine, and what is its interface? |
| [Resources](#resources) | What does each manifest field mean? |
| [Configuration](#configuration) | How is Fleet set up: the native TOML, initialization, environment, telemetry? |
| [Migrating from v1](migrating-from-v1.md) | How do v1 concepts, configuration and commands map to v2? |

## Layout

```text
docs/fleet/
├── README.md                       this index
├── ARCHITECTURE.md                 components, boundaries, ports, execution flow
├── architecture.svg                the execution-flow diagram
├── CONTRACTS.md                    integration guarantees and their tests
├── migrating-from-v1.md            v1 → v2 mapping
├── configuration/                  user configuration
│   ├── fleet-toml.md               the native fleet.toml
│   ├── init.md                     fleet init and the VEGA starter
│   ├── environment-json.md         the environment file
│   └── telemetry.md                OpenTelemetry export
├── controllers/                    one guide per controller
│   ├── README.md                   the controller index
│   └── <controller>/
│       ├── README.md               lifecycle and responsibilities
│       ├── interface.md            probes, plans, effects, verbs
│       ├── state-machine.md        the states, with state-machine.svg
│       └── adapters/               platform adapters, when the controller has them
├── resources/                      one page per resource kind
└── media/                          the target-architecture animation
```

The convention, kept from Fleet: an upper-case file is a top-level subject;
a lower-case, hyphenated file sits in the directory that owns it; every
directory with more than one page has a `README.md` index; a controller keeps
its lifecycle, interface and state machine together under its own name, and a
diagram sits beside the page that embeds it.

### Controllers

| Controller | Resource | Responsibility |
| --- | --- | --- |
| [Task](controllers/task/README.md) | [Task](resources/task.md) | Runs and releases one agent session, and delivers its brief and inbox. |
| [ChangeRequest](controllers/change-request/README.md) | [ChangeRequest](resources/change-request.md) | Mirrors one forge change and retains its numbered snapshot. |
| [Forge](controllers/forge/README.md) | [Task forge status](resources/task.md#following-declared-changes) | Follows declared changes, relays feedback and plans mark-ready. |
| [Plugin](controllers/plugin/README.md) | [Plugin-defined kinds](resources/plugin-kinds.md) | Hosts external controllers behind the same probe, plan and effect boundaries. |
| [Gateway](controllers/gateway/README.md) | [Conversation](resources/conversation.md), [Listener](resources/listener.md) | Mirrors chat threads, ties each to one Task, relays messages in and posts declarations out. |

### Resources

| Resource | Page |
| --- | --- |
| Task | [task.md](resources/task.md): every field, repository workspaces, structured outputs |
| Session | [session.md](resources/session.md): the agent a Task runs, its identity and states |
| TaskConfig | [task-config.md](resources/task-config.md): reusable execution defaults |
| EnvProvider | [env-provider.md](resources/env-provider.md): environment sources |
| ChangeRequest | [change-request.md](resources/change-request.md): one mirrored forge change |
| Conversation | [conversation.md](resources/conversation.md): one mirrored chat thread |
| Listener | [listener.md](resources/listener.md): one chat account's inbound health |
| Plugin kinds | [plugin-kinds.md](resources/plugin-kinds.md): kinds a plugin defines |

### Configuration

| Page | Owns |
| --- | --- |
| [fleet-toml.md](configuration/fleet-toml.md) | Every field of the native config: store, runtime, workspaces, forges, gateway, plugins |
| [init.md](configuration/init.md) | `fleet init`, the VEGA starter, `fleet up` |
| [environment-json.md](configuration/environment-json.md) | The environment file a Task's session inherits |
| [telemetry.md](configuration/telemetry.md) | The `OTEL_*` variables, and the spans and metrics Fleet exports |

## What this design changes

[Fleet as thurbox's engine](../FLEET-ENGINE.md) keeps these guides as they are
and changes three things they describe: the runtime port becomes a process
protocol served by any runtime (`[runtimes.<name>]` in place of `[runtime]`),
workspaces are made by recipes through the runtime rather than by the
`worktree` adapter or the runtime alone, and `fleet watch` and `fleet attach`
serve views. Each change lands in these pages with its migration step.
