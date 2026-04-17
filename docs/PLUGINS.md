# Thurbox Plugins

Thurbox plugins are bundles that extend the orchestrator with new content
and/or new processes. A single plugin can ship:

- **Content** — TOML/markdown that gets merged into the effective skills,
  roles, MCP servers, and themes lists.
- **A process** *(future)* — an executable thurbox spawns, speaking JSON-RPC
  over stdio, that contributes a backend implementation and/or MCP tools.

Both halves are optional. A plugin with only `[contributes]` is a static
content bundle. A plugin with only `[process]` is a pure-process plugin.

> **Status:** content plugins (`[contributes]`) are fully supported as of
> schema v19. The `[process]` runtime spawns every enabled process plugin at
> boot, performs the JSON-RPC handshake (carrying the plugin's effective
> configuration), captures stderr to per-plugin log files, and registers a
> `SessionBackend` adapter for each `onBackendSelected:<name>` activation
> event so the contributed backend appears in session creation. Updates from
> `set_plugin_setting` propagate to the running plugin as `config.updated`
> notifications. Plugins declaring `capabilities = ["mcp-tools"]` can be
> driven over a Unix control socket from `thurbox-mcp`, either listing
> tools statically from the manifest or delegating to the running plugin
> via `mcp.list_tools` / `mcp.call`. The lazy-activation hooks for
> `onCommand`, `onRole`, and `onWorkspaceContains` are still landing — for
> `mcp-tools` today, use `onStartupFinished` so the plugin is live when the
> control socket is asked to call it.

## Disk layout

A plugin lives under `~/.local/share/thurbox/admin/plugins/<plugin-name>/`:

```text
~/.local/share/thurbox/admin/plugins/<plugin-name>/
├── thurbox-plugin.toml     # required manifest
├── skills/<name>/SKILL.md  # one directory per [[contributes.skills]] row
├── roles/<name>.toml       # one TOML file per [[contributes.roles]] row
├── mcp/<name>.toml         # one TOML file per [[contributes.mcp_servers]]
├── themes/<name>.toml      # (planned)
├── bin/<exec>              # if [process] declared
└── README.md               # optional
```

`<plugin-name>` must pass `validate_safe_name`: 1–64 chars, no leading `.`,
no `/` `\` or `..`.

## Manifest reference — `thurbox-plugin.toml`

```toml
name = "my-plugin"               # required, must match the directory name
version = "0.1.0"                # required
description = "..."              # optional
author = "..."                   # optional
thurbox_plugin_api = 1           # required; bumps = breaking

# ── Content contributions (all optional) ────────────────────────
[contributes]

[[contributes.skills]]
name = "publish"
path = "skills/publish"          # dir must contain SKILL.md

[[contributes.roles]]
name = "reviewer"
path = "roles/reviewer.toml"     # TOML matching RoleConfig

[[contributes.mcp_servers]]
name = "gh"
path = "mcp/gh.toml"             # TOML matching McpServerConfig

[[contributes.themes]]
name = "midnight"
path = "themes/midnight.toml"

# Static MCP tool declarations — lets `list_plugin_tools` answer
# without waking a dormant plugin. Omit to discover at runtime
# via the `mcp.list_tools` op.
[[contributes.mcp_tools]]
name = "echo"
description = "Echo back the input as {\"echoed\": <args>}."
input_schema = { type = "object", additionalProperties = true }

# ── Configuration schema (optional) ─────────────────────────────
[[contributes.configuration]]
key = "poll_interval"
type = "duration"                # string | int | bool | duration | path | enum
default = "15s"
description = "How often to poll for new items"

[[contributes.configuration]]
key = "max_workers"
type = "int"
default = 3
min = 1
max = 16
description = "Maximum parallel worker sessions"

# ── Process section (optional, runtime forthcoming) ─────────────
[process]
exec = "bin/my-plugin"           # relative to plugin dir
capabilities = ["backend", "mcp-tools"]

# Declarative activation — omitted ⇒ ["onStartupFinished"]
activation_events = [
    "onStartupFinished",
    "onBackendSelected:my-backend",
    "onCommand:my-tool-id",
    "onRole:reviewer",
    "onWorkspaceContains:.beads/",
]
args = []
env = { MY_PLUGIN_MODE = "thurbox" }
```

The parser uses `#[serde(deny_unknown_fields)]` on the top-level table, on
`[process]`, and on every `[[contributes.*]]` row — typos surface immediately
as plugin errors instead of silent no-ops.

### Configuration types

| `type` | Manifest default form | Notes |
|--------|----------------------|-------|
| `string` | `"text"` | Any string |
| `int` | `42` | Optional `min` / `max` (inclusive); rejects out-of-range |
| `bool` | `true` / `false` | |
| `duration` | `"15s"` | Suffixes: `ms`, `s`, `m`, `h` |
| `path` | `"/abs/or/relative"` | String — no resolution at validate time |
| `enum` | `"info"` | Requires non-empty `values = ["info", "warn", ...]` |

Validation runs both for the manifest's declared default and for any user
override coming through `set_plugin_setting`.

### Activation events

Each event in `activation_events` describes when a process plugin should be
spawned. Multiple events OR together — any match triggers a spawn. Once
spawned, the plugin stays running until `disable_plugin` or shutdown.

| Event | When it fires | Typical use |
|-------|---------------|-------------|
| `onStartupFinished` | After thurbox boot + session restore | Eager plugins, status monitors |
| `onBackendSelected:<name>` | A session is created on this backend | Backend plugins — lazy spawn only when needed |
| `onCommand:<tool-id>` | Inbound `thurbox-mcp` tool call with this id | `mcp-tools` plugins — zero cost when idle |
| `onRole:<name>` | A session is created with this role | Role-scoped plugins |
| `onWorkspaceContains:<path>` | Session opens on a repo containing this relative path | Repo-scoped plugins |

Omitting `activation_events` defaults to `["onStartupFinished"]`.

## Discovery and precedence

Plugins reach thurbox through two sources:

- **Disk** — `~/.local/share/thurbox/admin/plugins/<name>/`. Auto-discovered
  on every read.
- **Registered** — rows in the SQLite `plugins` table. Used for plugins
  installed outside the admin dir, plus *shadow rows* that persist the
  `enabled = false` flag for disk-only plugins across restarts.

For an individual plugin (the row in `list_plugins`), **registered wins** over
disk on name collision.

For an individual content row inside `list_effective_skills` /
`list_effective_roles` / `list_effective_mcp_servers`, the order is
**Registered > Plugin > Disk** — explicit user overrides beat curated plugin
contributions, which beat ambient admin-dir defaults.

Process plugins must have globally unique backend names and MCP tool IDs
across all enabled plugins. Load order defines priority; conflicting names
get a `tracing::warn!` and the second one loses for that name only.

## MCP tools

Twelve tools mirror the skills surface. All are exposed by `thurbox-mcp`.

### Lifecycle

| Tool | Purpose |
|------|---------|
| `list_plugins` | Effective list with `{name, path, version, enabled, source, contributions, process?, error?}` |
| `set_plugins` | Atomically replace the registry |
| `register_plugin` | Upsert one row; manifest must validate |
| `unregister_plugin` | Delete the registry row; never touches disk |
| `enable_plugin` / `disable_plugin` | Toggle enabled flag; creates a shadow row for disk-only plugins |
| `install_plugin` | Copy a source dir into `admin/plugins/<name>/` |
| `uninstall_plugin` | Remove from disk + registry (cascades settings); requires `confirm: true` |

### Configuration

| Tool | Purpose |
|------|---------|
| `list_plugin_settings` | Schema + current values: `[{key, type, default, user_value?, effective_value, description}]` |
| `get_plugin_setting` | Effective value for one key |
| `set_plugin_setting` | Validates against schema; auto-creates a shadow registry row for disk-only plugins so the setting persists |
| `reset_plugin_setting` | Clear a user override |

### Plugin MCP tools

Exposed over the control socket to bridge `thurbox-mcp` into live
process plugins.

| Tool | Purpose |
|------|---------|
| `list_plugin_tools` | For a running `mcp-tools` plugin, returns `{source: "manifest"\|"runtime", tools: [...]}`. Prefers the static `[[contributes.mcp_tools]]` rows and only falls back to an `mcp.list_tools` RPC when the manifest declares none |
| `call_plugin_tool` | Forwards `{plugin_name, tool, args}` to the plugin's `mcp.call` op; proxied through the control socket so the `mcp` module stays isolated from plugin-runtime types |

## Authoring an example

A minimal content bundle:

```text
my-plugin/
├── thurbox-plugin.toml
├── skills/publish/SKILL.md
├── roles/reviewer.toml
└── mcp/gh.toml
```

```toml
# thurbox-plugin.toml
name = "my-plugin"
version = "0.1.0"
thurbox_plugin_api = 1

[[contributes.skills]]
name = "publish"
path = "skills/publish"

[[contributes.roles]]
name = "reviewer"
path = "roles/reviewer.toml"
```

Drop the directory under `~/.local/share/thurbox/admin/plugins/`, restart
thurbox, and `list_plugins` will show it as `source: "disk"`. The skill
appears in `list_skills` with `source: "plugin"` and the role in
`list_roles` (effective).

A working fixture lives at
[`tests/fixtures/plugins/sample-content-bundle/`](../tests/fixtures/plugins/sample-content-bundle/).

## Wire protocol

A process plugin is a child whose stdin/stdout speak line-delimited JSON-RPC
and whose stderr is captured to the per-plugin log file.

- Request: `{"id": N, "op": "<name>", "params": {...}}\n`
- Response: `{"id": N, "ok": true, "result": {...}}\n` or
  `{"id": N, "ok": false, "error": "..."}\n`

Every request is a single line; responses must echo the request `id` so
overlapping calls demultiplex correctly. Notifications (currently only
`config.updated`) use `id: 0` and the plugin should not respond.

### Op set

| op | direction | purpose |
|----|-----------|---------|
| `handshake` | thurbox → plugin | first call after spawn; declares api version + delivers effective configuration |
| `config.updated` | thurbox → plugin | fire-and-forget notification when a `plugin_settings` value changes |
| `backend.spawn` | thurbox → plugin | open a new session — see below |
| `backend.adopt` | thurbox → plugin | re-attach to an existing `backend_id` after a thurbox restart |
| `backend.discover` | thurbox → plugin | list known sessions for restoration |
| `backend.resize` | thurbox → plugin | window resize |
| `backend.is_dead` | thurbox → plugin | liveness probe |
| `backend.kill` | thurbox → plugin | terminate a session |
| `backend.detach` | thurbox → plugin | release a session without killing it |
| `backend.pane_pid` | thurbox → plugin | best-effort pid of the foreground process for the session |
| `mcp.list_tools` | thurbox → plugin | dynamic tool discovery for the `mcp-tools` capability |
| `mcp.call` | thurbox → plugin | invoke one of the plugin's tools |
| `stop` | thurbox → plugin | graceful shutdown request; plugin should respond then exit |

`backend.*` ops are required when the manifest declares
`capabilities = ["backend"]`. `mcp.*` ops are required when the manifest
declares `capabilities = ["mcp-tools"]`, though `mcp.list_tools` is skipped
if the manifest already provides `[[contributes.mcp_tools]]` rows.

### Handshake

Thurbox sends:

```json
{
  "id": 1,
  "op": "handshake",
  "params": {
    "api_version": 1,
    "capabilities": ["backend"],
    "effective_configuration": { "echo_greeting": true }
  }
}
```

`effective_configuration` is always a JSON object — empty (`{}`) when the
plugin declares no `[[contributes.configuration]]` rows. The plugin must
respond with the same `id` and at minimum:

```json
{ "id": 1, "ok": true, "result": { "api_version": 1, "capabilities": ["backend"] } }
```

Mismatched `api_version` aborts the spawn and the plugin is marked errored.

### `config.updated` notification

When `set_plugin_setting` (or `reset_plugin_setting`) changes a value, the
runtime sends:

```json
{ "id": 0, "op": "config.updated", "params": { "key": "echo_greeting", "value": false } }
```

The plugin can react immediately or defer until its next call — stateless
plugins can ignore the notification and re-read on demand. The runtime
poll is gated by `PRAGMA data_version`, so changes made through a separate
`thurbox-mcp` process propagate within one TUI tick.

### Backend ops

The contributed backend owns one Unix domain socket per session. Thurbox
talks to the plugin over stdio for control, then connects to the
plugin-provided socket for the session's bidirectional byte stream. Using a
socket per session lets `std::os::unix::net::UnixStream` slot directly into
[`SpawnedSession`] / [`AdoptedSession`] without ad-hoc multiplexing.

#### `backend.spawn`

Request `params`:

```json
{
  "window_name": "my-session",
  "command": "/bin/bash",
  "args": [],
  "cwd": "/home/me/repo",
  "env": { "FOO": "bar" },
  "rows": 24,
  "cols": 80
}
```

Response `result`:

```json
{ "backend_id": "sample-0", "socket_path": "/run/user/1000/thurbox-sample/sample-0.sock" }
```

The plugin must `bind()` and `listen()` on `socket_path` *before* responding,
because thurbox connects to it as soon as the response arrives.

#### `backend.adopt`

`{ "backend_id": "<id>", "rows": 24, "cols": 80 }` → same response shape as
`spawn`. Used at thurbox startup to re-attach to a session that survived a
restart.

#### `backend.discover`

`params: {}` → result is a JSON array:

```json
[{ "backend_id": "sample-0", "name": "sample-0", "is_alive": true }]
```

#### `backend.resize` / `backend.kill` / `backend.detach`

`{ "backend_id": "<id>" }` (resize also carries `rows` and `cols`). Result
may be `null`.

#### `backend.is_dead`

`{ "backend_id": "<id>" }` → `{ "dead": true|false }`.

#### `backend.pane_pid`

`{ "backend_id": "<id>" }` → `{ "pid": 12345 }` or `{ "pid": null }`. Used
for "open in editor" and process-tree introspection.

### MCP-tools ops

Used by plugins declaring `capabilities = ["mcp-tools"]`. Traffic is
driven from the control socket rather than the TUI itself: `thurbox-mcp`
dials `$XDG_RUNTIME_DIR/thurbox/control.sock`, and the TUI-side dispatcher
forwards calls to the running plugin.

#### `mcp.list_tools`

`params: {}` → result is a JSON array of tool descriptors, one per
tool. The runtime only issues this op when the manifest contributes
no `[[contributes.mcp_tools]]` rows.

```json
[
  {
    "name": "echo",
    "description": "Echo back the input as {\"echoed\": <args>}.",
    "input_schema": {
      "type": "object",
      "properties": {},
      "additionalProperties": true
    }
  }
]
```

#### `mcp.call`

Request `params`:

```json
{ "name": "echo", "params": { "hello": "world" } }
```

Response `result` is whatever JSON the tool returns — no envelope:

```json
{ "echoed": { "hello": "world" } }
```

Errors surface as the standard `{ "ok": false, "error": "..." }` frame
and are forwarded verbatim to the `thurbox-mcp` caller.

### `stop`

`params: {}` → `null`. The runtime sends `stop`, waits for the response,
then closes stdin. After a 5 s grace it sends SIGTERM, then SIGKILL.

The wire shape is deliberately aligned with gastown's exec-provider protocol
so the same binary can serve both hosts.

## Reference process plugin

[`examples/sample_backend_plugin.rs`](../examples/sample_backend_plugin.rs)
is the canonical reference for backend plugins. It implements the full op
set above and backs each session with a small echo loop on a Unix socket
under `$XDG_RUNTIME_DIR/thurbox-sample/`.

Build it once and the integration test
[`tests/plugin_backend.rs`](../tests/plugin_backend.rs) drives it end-to-end:

```bash
cargo build --example sample-backend-plugin
cargo nextest run --test plugin_backend
```

To use it as a real installed plugin, copy
[`tests/fixtures/plugins/sample-backend-plugin/thurbox-plugin.toml`](../tests/fixtures/plugins/sample-backend-plugin/thurbox-plugin.toml)
into `~/.local/share/thurbox/admin/plugins/sample-backend-plugin/`, place
the compiled binary at `bin/sample-backend-plugin`, and restart thurbox.
The `sample` backend will appear in session creation.

## Reference MCP plugin

[`examples/sample_mcp_plugin.rs`](../examples/sample_mcp_plugin.rs) is the
canonical reference for the `mcp-tools` capability. It declares
`capabilities = ["mcp-tools"]` and exposes a single `echo` tool that
returns its arguments wrapped in `{"echoed": <args>}`. The same process
handles both paths: static tools (populated from the manifest) and
dynamic ones (answered on `mcp.list_tools`).

Build it once and the integration test
[`tests/mcp_tools_integration.rs`](../tests/mcp_tools_integration.rs)
exercises the full stack — manifest discovery, spawn + handshake,
control-socket framing, static-vs-runtime tool listing, and
round-tripped `mcp.call`:

```bash
cargo build --example sample-mcp-plugin
cargo nextest run --test mcp_tools_integration
```
