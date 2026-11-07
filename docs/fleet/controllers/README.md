# Controllers

Fleet has built-in pure controllers and a plugin host with pure admission.
Each guide keeps the controller's lifecycle, interface and state-machine
diagram together.

| Controller | Resource | Responsibility |
| --- | --- | --- |
| [Task](task/README.md) | [Task](../resources/task.md) | Runs and releases one agent session, and delivers its brief and inbox. |
| [ChangeRequest](change-request/README.md) | [ChangeRequest](../resources/change-request.md) | Mirrors one forge change and retains its numbered snapshot. |
| [Forge](forge/README.md) | [Task forge status](../resources/task.md#following-declared-changes) | Follows declared changes, relays feedback and plans mark-ready. |
| [Plugin](plugin/README.md) | [Plugin-defined kinds](../resources/plugin-kinds.md) | Hosts external controllers behind the same probe, plan and effect boundaries. |
| [Gateway](gateway/README.md) | [Conversation](../resources/conversation.md), [Listener](../resources/listener.md), [Task gateway status](../resources/task.md#talking-in-a-conversation) | Mirrors chat threads, ties each to one Task, relays messages in and posts declarations out. |

A controller with platform adapters documents them under
`<controller>/adapters/`. The directory holds a `README.md` with the shared
mapping onto the port and a comparison of the platforms. It also holds one page
per platform, with the fixed H2 headings that the
[gateway adapters](gateway/adapters/README.md#adding-a-platform) list. The
Gateway follows this layout. The Forge documents its adapters in its own guide.

See the [overall architecture](../ARCHITECTURE.md) for the ports and crate
boundaries shared by the controllers and the plugin host.
