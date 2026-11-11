# Fleet

Fleet is the engine behind thurbox: it persists Tasks and the other resource
kinds and drives their controllers, one agent session per Task. Its crates are
`crates/fleet-*` in this repository, imported from `Thurbeen/fleet` with their
history, and `crates/architecture.toml` is their dependency graph. How
Fleet fits between thurbox and the runtimes is [the overview](../README.md).

| Page | Answers |
| --- | --- |
| [architecture.md](architecture.md) | The components, boundaries, ports and execution flow, and the target: views and workspaces |
| [contracts.md](contracts.md) | What each part guarantees, and which tests hold it to that |
| [configuration.md](configuration.md) | `fleet init`, `fleet up`, running VEGA, every `fleet.toml` field, the environment file, OpenTelemetry export |
| [signal-contract.md](signal-contract.md) | For an agent vendor: how an agent reports its state to Fleet through `fleet signal` |

## Controllers

Each controller has one page: its lifecycle, its state machine (with the
diagram of the same name beside it) and its interface.

| Controller | Resource | Responsibility |
| --- | --- | --- |
| [Task](controllers/task.md) | [Task](resources/task.md) | Runs and releases one agent session, and delivers its brief and inbox. |
| [ChangeRequest](controllers/change-request.md) | [ChangeRequest](resources/change-request.md) | Mirrors one forge change and retains its numbered snapshot. |
| [Forge](controllers/forge.md) | [Task forge status](resources/task.md#following-declared-changes) | Follows declared changes, relays feedback and plans mark-ready. |
| [Plugin](controllers/plugin.md) | [Plugin-defined kinds](resources/plugin-kinds.md) | Hosts external controllers behind the same probe, plan and effect boundaries. |
| [Gateway](controllers/gateway.md) | [Conversation](resources/conversation.md), [Listener](resources/listener.md) | Mirrors chat threads, ties each to one Task, relays messages in and posts declarations out. |

The Gateway's platform adapters, Slack and Discord, share one page,
[gateway-adapters.md](controllers/gateway-adapters.md): the mapping onto the
port, the comparison, and one section per platform with fixed headings.

## Resources

One page per kind, because each is a reference read on its own.

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

## Diagrams

[architecture.svg](architecture.svg) is today's execution flow,
[architecture-target.svg](architecture-target.svg) the target with views and
runtimes, and
`controllers/<name>.svg` each controller's state machine.
