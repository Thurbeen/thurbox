# Configuration

## Initialization and the VEGA starter

`fleet init` creates the native config and the bundled, editable VEGA
workspace. `fleet up` starts VEGA and keeps every due Task reconciling.
This page owns their behaviour. The [native Fleet TOML](#native-fleet-configuration-fleettoml)
reference owns the config fields.

### Prerequisites

`init` works offline. To run real sessions, install Thurbox (`thurbox-cli`
on PATH), configure its local multiplexer, and install and authenticate the
chosen harness first. Fleet does not install prerequisites or change
login state. `up` checks host availability and reports unavailable or unknown
prerequisites. Harness login/readiness failures remain visible in Task status;
inspect `fleet status task vega --json` and Thurbox's session before retrying.

### Choosing the harness

The harness is the coding agent that a Task's session runs, such as `claude`
or `codex`. `fleet init` writes one harness into the workspace's `task.json`:

- `fleet init --harness NAME` writes `NAME`.
- With no flag, `init` asks at a terminal if it never installed a
  `task.json` in this workspace. An empty answer, or no terminal, keeps the
  bundled default, `codex`.
- With no flag and an installed `task.json`, `init` keeps its harness.

VEGA's brief tells VEGA to give each worker Task the same harness as
`Task/vega`. A user or a repository can ask for another harness for one
worker.

The first `fleet up` creates `Task/vega` from `task.json`. Before that,
`init --harness NAME` rewrites an unedited `task.json`. If you edited
`task.json`, `init` keeps your file. It writes its own version as a conflict
copy, as `init --update` does.

After the first `up`, the harness of `Task/vega` is immutable.
`init --harness` refuses a different harness at that point.

Fleet trusts the `codex`, `claude` and `claude-code` harnesses in their
workspaces by default. For another harness, set
[`runtime.workspaceTrust`](#native-fleet-configuration-fleettoml) before the first `up`. Without that
entry, `Task/vega` waits and starts no session.

### Running `fleet up`

`up` runs in the foreground and continuously reconciles every due Task, even
while VEGA is busy or idle. Keep it running in a terminal or your own service
supervisor. Ctrl-C (also SIGTERM on Unix) finishes the bounded sweep, releases
the manager lease and retains sessions. Run `fleet up` again to reconnect to
the same ordinary `Task/vega`; a simultaneous manager exits with code 6.
After a crash, wait for the recorded lease to expire before restarting. Unknown
session observations never trigger a blind replacement. Stalled, completed or
deleting VEGA Tasks require inspection and deliberate recovery, not another
automatic Task identity.

Polling defaults to one bounded sweep per second; `up --poll-ms` accepts 100
through 10000 ms. Each unfinished Briefed Task costs two bounded session
lookups per sweep: one by key, and one that asks whether its window is gone.
Unchanged Task status is not rewritten. A manual `fleet reconcile` remains
available for bounded diagnostics while `up` is stopped.

After a host reboot, `fleet up` revives the sessions of unfinished Tasks and
resumes their briefs. See
[after a host reboot](controllers/task.md#after-a-host-reboot).
This works only when `fleet up` starts again after the reboot. A service
supervisor must start it at boot, without a login. For a systemd user unit
that has `WantedBy=default.target` in its `[Install]` section:

```sh
loginctl enable-linger "$USER"
systemctl --user enable fleet-vega.service
```

### Configuration and customization

Reference: [native Fleet TOML](#native-fleet-configuration-fleettoml),
[Task](resources/task.md), [TaskConfig](resources/task-config.md),
[EnvProvider](resources/env-provider.md), and
[environment JSON](#environment-json-file-and-command-output).

Config selection is `--config PATH`, then `FLEET_CONFIG`, then exactly one
native per-user path: `$XDG_CONFIG_HOME/fleet/config.toml` (fallback
`~/.config/fleet/config.toml`). Fleet never searches the current directory or
its parents. Explicit paths must exist for commands other than `init`;
`init --config PATH` creates a separate configuration and a default
data/workspace root at `<native-data>/fleet/instances/<config-path-hash>/`.
Relative paths inside TOML resolve beside that file. With no native data-home
variables, explicit `--config` uses a `fleet-data` directory beside the config
as its data root. An explicit store must be separate from the config and
workspace.

Default init writes a workspace at `$XDG_DATA_HOME/fleet/vega` (fallback
`~/.local/share/fleet/vega`) and a fresh resource store under the same Fleet
directory at `store-v1`. The config records absolute paths and a generated
per-instance session key prefix. It keeps the starter's
`runtime.callTimeoutMs = 10000`, below the built-in default of `15000`;
edit it to change the bound. `[starter] workspace` selects the workspace.
Its `task.json` is a runnable JSON array containing one Task. Before first
`up`, edit its brief and customize `AGENTS.md`, prompts and skills.
To set `spec.model`, first configure the matching `[runtime.modelEnv]`
harness-to-variable mapping; the starter does not assume a Codex model
environment variable. After creation, reconnect keeps the stored immutable
Task inputs. Updated instructions can be read from the workspace; changing
Task inputs requires deliberate deletion/recreation after session release.
VEGA's rendered brief names the exact config file for all Task operations and
child-worker handoffs.

Repeated `init` preserves existing files and config. After updating the binary,
run `fleet init --update` to offer its bundled content. Unedited managed
workspace files advance; edits, deletions and private files survive. Config is
user-owned after creation and is never replaced by an update. Review proposed
conflicts under `vega/.fleet/conflicts/<content-hash>/`; merge them manually.
The `.fleet` directory holds instance identity, baseline hashes, manifest and
source provenance; preserve it with the workspace. Malformed metadata is
refused. A symlink at or above the path of a managed file is refused. The
`.claude/skills` link below is not a managed file. Initializers serialize
with OS locks; a crash releases the locks and a repeat init completes missing
files conservatively.

VEGA's skills have one canonical copy under `vega/.agents/skills`, where
Codex finds them. Claude Code finds project skills only under
`.claude/skills`. `init` and `init --update` therefore create the relative
symlink `vega/.claude/skills` to `../.agents/skills`. This link is the only
symlink that init manages. Init never replaces an existing `.claude/skills`
that is not this link; it reports that path instead. When the link cannot
be created, init reports the failure and completes. Claude Code sessions then
do not find VEGA's skills.

The bundled content is MIT licensed at exact source commit
`3c4798dbdb33672576958cf8f1849c16e519d95d`. The build verifies every allowlisted
asset against [the provenance hashes](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/crates/fleet-cli/bundle/vega/provenance.json)
and embeds the assets, source manifest and license in the executable. Runtime
init/update never fetches a branch or downloads content.

### Resource commands

`apply -f -` reads JSON from stdin. A file can hold one object, an array of
objects, or an object with `items`. Config uses versioned TOML; resource
manifests use `apiVersion: fleet/v1alpha1`. Use `fleet --help` and each
command's `--help` for the flags this build actually serves.

A worker writes its answer with `fleet result TASK --generation N --outcome
shipped --summary TEXT`; the generation must match the delivered brief, and
required outputs must match the Task's declared schema. A result is a claim,
not proof of an artifact landing. `fleet send TASK TEXT` queues a message
for a running or concluded Task's session; the next reconcile delivers it
once, when the session is ready.

Tasks can reference a passive `TaskConfig` for harness, host, model and
environment defaults. `EnvProvider` resources supply stored string maps and
explicit manager-local JSON files or bounded commands. Explicit Task values
win; launch values are frozen privately before the first effect and reused
across retries and restarts. See [execution configuration](resources/task-config.md)
for runnable manifests, precedence, security boundaries and snapshot recovery.

Register trusted protocol-1 controller programs under `[plugins.<name>]` in
the config. Their manifests add schema-validated resource kinds; `fleet apply`,
`get`, `list`, `status` and `delete` accept those kinds. Inspect registration
with `fleet list plugins --json`, and run their controllers with
`fleet reconcile` or `fleet up`. The
[Plugin controller guide](controllers/plugin.md) defines the
protocol, admitted operations, effect recovery and environment separation.
Plugins can create Tasks and, by default, steer only Tasks carrying their
creator label. Worker results remain the worker's responsibility. Plugin
programs run as the manager user; controller environment scrubbing is a
purity contract, not a sandbox.

## Native Fleet configuration (fleet.toml)

The native versioned TOML file selects a resource store, runtime, optional
VEGA workspace and the forges ChangeRequests are read from. It is separate
from the JSON [Task](resources/task.md),
[TaskConfig](resources/task-config.md),
[EnvProvider](resources/env-provider.md) and
[ChangeRequest](resources/change-request.md) resources applied into that
store.
`fleet apply` takes no TOML as input, and `--config` applies no resources.
There is no `[taskConfig]`, `[envProvider]` or task-template section in TOML.
There is no telemetry section either: the standard `OTEL_*` variables
configure [OpenTelemetry export](#opentelemetry-export).

### File selection and paths

Selection order is `--config PATH`, then `FLEET_CONFIG`, then one native path:

| Platform | Native configuration path |
| --- | --- |
| Linux | `$XDG_CONFIG_HOME/fleet/config.toml`, falling back to `~/.config/fleet/config.toml`. |
| Windows | `%APPDATA%\fleet\config.toml`. |

Fleet does not search the current directory or parents. Explicit config paths
must exist, except `fleet init --config PATH`, which creates an isolated
instance without replacing an existing config. `fleet init` creates the
native config and bundled workspace; see
[initialization/customization](#configuration-and-customization)
for data roots, managed files, conflict handling and per-instance paths.

Relative `data.dir`, `runtime.dir`, `starter.workspace`, a fake forge host's
`dir` and a fake gateway account's `dir` resolve beside the TOML file. A
runtime executable should be a PATH name or absolute path;
`runtime.cli` is passed to process execution without rebasing it beside TOML.
Provider source paths are always absolute and manager-local, even for remote
Tasks; they never inherit TOML's relative-path rule.

### Real-runtime example

```toml
version = 1

[data]
dir = "store"

[runtime]
kind = "thurbox"
cli = "thurbox-cli"
callTimeoutMs = 15000
keyPrefix = "fleet"

[runtime.modelEnv]
claude-code = "ANTHROPIC_MODEL"

[starter]
workspace = "vega"
```

Set `starter.workspace` to an initialized VEGA workspace when using `fleet up`.
For ordinary `apply`/`reconcile`, the starter section is optional. A second
Fleet installation talking to the same Thurbox hosts needs a distinct
`keyPrefix`. Store, workspace and config must remain separate paths.

### Fields

| Field | Required/default | Meaning |
| --- | --- | --- |
| `version` | Required, `1` | Native config schema; unsupported versions and unknown fields are refused. |
| `data.dir` | Optional | Resource store. `--data-dir` overrides it; otherwise Fleet uses its native data directory. |
| `runtime.kind` | Required when configuring runtime | `thurbox` or `fake`; needed for reconciliation/session operations. |
| `runtime.cli` | Thurbox: PATH `thurbox-cli` (`thurbox-cli.exe` on Windows) | Bounded Thurbox CLI executable. |
| `runtime.callTimeoutMs` | Thurbox: `15000` | Bound for each Thurbox call; manager lease must outlast the runtime's reported worst case. |
| `runtime.keyPrefix` | Thurbox: `fleet` | Session identity prefix; at most 32 lowercase letters/digits/`-`, starting/ending with letter/digit. |
| `runtime.modelEnv` | Thurbox: Claude mappings below | Harness name to supported model environment-variable name. |
| `runtime.repositories` | Thurbox: empty | Repository URL to its source checkout, an absolute path on the hosts where thurbox or the `worktree` adapter creates worktrees. A relative path is refused at load. A Task's `url` must equal a key byte for byte: Fleet does not normalise a trailing `/`, `.git`, case or an SSH form. A Task whose `workspace.repositories` names an unmapped URL waits in Pending with `RepositoryUnmapped`; the refusal spends no spawn or materialize attempt. A running `fleet up` reloads this table; see [Reloading](#reloading-runtimerepositories). |
| `runtime.workspaceCleanup` | Thurbox: `delete` | What deleting a session does to the worktrees thurbox made. A workspace Fleet made through `[workspaces]` is cleaned after the release whatever this says. `delete` removes them with the session (`session delete --force`). `keep` leaves them, restorable in thurbox (`session delete --soft`). Any other value is refused at load. The setting also applies to `dir` Tasks: `delete` overrides thurbox's own `soft_delete`, so a released session cannot be restored. |
| `runtime.workspaceTrust` | Thurbox: `codex` is `codex`; `claude` and `claude-code` are `claude` | Harness name to its workspace-trust interface: `codex`, `claude` or `none`. Any other value is refused at load. A Task whose harness is not listed starts no session and waits in Pending with `TrustUnmapped`; the refusal spends no spawn attempt. A running `fleet up` applies a new entry after a restart. See [workspace trust](contracts.md#workspace-trust). |
| `runtime.dir` | Required for `fake` | Isolated fake-host persistence, relative to this config file. |
| `workspaces.kind` | Optional | `worktree` or `fake`. With this section, Fleet makes each `repositories` workspace before the spawn and cleans it after the release. Without it, the runtime makes the workspace. See [the workspace port](contracts.md#workspace-contract). |
| `workspaces.root` | Required for `worktree` | The directory that holds every workspace on the local host, relative to this config file. Sources come from `runtime.repositories`. |
| `workspaces.git` | Worktree: PATH `git` | The `git` executable. |
| `workspaces.callTimeoutMs` | Worktree: `30000` | Bound for each `git` command. The manager lease must outlast the adapter's reported worst case. |
| `workspaces.branchPrefix` | Worktree: `fleet` | The first part of each workspace branch, `<branchPrefix>/<session key>`. Lowercase letters, digits and `-`. |
| `workspaces.dir` | Required for `fake` | The fake host's directory, relative to this config file. Use `runtime.dir` to share records with the fake runtime. |
| `starter.workspace` | Required for `up` | Initialized VEGA workspace, relative to this config file. |

`--data-dir` is for explicit store operations. `fleet up` rejects a differing
override because its workers must share the store selected by the persisted
config. Persist `data.dir` before using `up` with a different store. All
resource commands in one workflow should use the same `--config` path.

#### Reloading `runtime.repositories`

`fleet up` reads this file again before each sweep. It applies
`runtime.repositories` only, so a new mapping needs no restart:

- A valid file replaces the map. The next spawn uses it, and a Task waiting
  with `RepositoryUnmapped` spawns at its next retry.
- An unreadable, unparsable or invalid file changes nothing. The last valid
  map stays in force. `fleet up` reports the refusal once on standard error.
- A change to any other setting is not applied. `fleet up` reports it once
  and asks for a restart. A running process never switches its store, lease
  holder, runtime or key prefix.

`runtime.modelEnv` extends/overrides the built-in `claude` and `claude-code`
entries, both `ANTHROPIC_MODEL`. An explicit/inherited Task model requires a
mapping for its selected harness; Fleet does not invent a Codex variable.
Choose the variable supported by the installed harness. Model selection wins
over the same environment key from providers or Task overrides. For configured
Tasks, both variable selection and value freeze with the private launch
snapshot; later TOML mapping changes do not alter that execution.

`runtime.workspaceTrust` extends/overrides the built-in entries. The keys are
thurbox agent names, so map each alias of an agent, such as
`codex-coder = "codex"`. Map `codex`
only to a harness that runs Codex: the interface puts Codex's `-c` option on
the harness's command line, and another harness reads `-c` differently.
Claude Code reads it as `--continue`. Set `none`
only for a harness that has no workspace-trust gate. `none` approves nothing
for a harness that has one: that harness can then receive its brief inside a
trust prompt. Codex keeps only the last `-c projects=` it is given. Keep any
other roots you trust in the `[projects]` table of Codex's `config.toml`,
which merges with Fleet's override.

Runtime-specific fields apply only to that runtime. Keep the store and native
configuration user-private; they contain literal manifests and private launch
snapshots. Windows snapshots inherit the store's ACL, while Unix launch
storage enforces owner-only permissions. See
[privacy and retention](resources/task-config.md#resolution-and-freezing).

### Offline fake-runtime example

```toml
version = 1

[data]
dir = "data"

[runtime]
kind = "fake"
dir = "fake-host"
```

This is the executable [example config](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/examples/fleet.toml). Fake
sessions run no agent, although explicitly configured external EnvProvider
commands still execute locally. Do not treat the fake runtime as a sandbox
for untrusted helpers.

```sh
fleet --config examples/fleet.toml apply -f examples/resources.json
fleet --config examples/fleet.toml list tasks --json
fleet --config examples/fleet.toml reconcile
```

For the new resources, save the manifests in their individual references,
then use the same explicit config for each operation:

```sh
fleet --config fleet.toml apply -f env-provider.json
fleet --config fleet.toml apply -f task-config.json
fleet --config fleet.toml apply -f task.json
fleet --config fleet.toml reconcile
```

Apply uses JSON resources. File/command provider output uses a third format:
[environment JSON maps](#environment-json-file-and-command-output). Keep source credentials out of
resource manifests. Config schema `1`, resource API `fleet/v1alpha1` and store
format `1` are separate version contracts; `fleet version --json` reports
them. No live store/config migration or service restart occurs through this
feature. `fleet init --update` preserves the user-owned native config.

### The `[forge]` section

The forges that [ChangeRequests](resources/change-request.md) are
mirrored from. Without this section a ChangeRequest is refused at apply, and
one already stored is reported by the manager and never polled.

```toml
[forge]
callTimeoutMs = 15000
closedGraceMs = 604800000     # one week; a ChangeRequest spec may override
settledTtlMs = 2592000000     # thirty days; a ChangeRequest spec may override
relayNotes = true             # defaults a Task's spec.forge.relay may override
relayChecks = true
relayBots = false
noCheckGraceMs = 300000       # the checksPassed predicate's wait for a check
draftPolicy = "checks"        # omit to never take a draft out of draft

[forge.hosts]
"github.com"         = { kind = "github", cli = "gh" }
"gitlab.com"         = { kind = "gitlab", cli = "glab" }
"gitlab.example.com" = { kind = "gitlab", cli = "glab" }
"dev.azure.com"      = { kind = "azure-devops", cli = "az" }
"bitbucket.org"      = { kind = "bitbucket-cloud", cli = "curl" }

[forge.readyPolicies.checks]
checksPassed = true

[forge.readyPolicies.checks-and-bots]
botReviewsAddressed = true
checksPassed = true

[forge.readyPolicies.strict]
botReviewsAddressed = true
checksPassed = true
noUnresolvedThreads = true
```

| Field | Required/default | Meaning |
| --- | --- | --- |
| `forge.callTimeoutMs` | `15000` | Bound for each forge CLI call. One observation of a change may make up to six such calls, and the manager lease is raised to outlast the port's reported worst case. |
| `forge.closedGraceMs` | `604800000` | How long a closed change stays watched before it settles. |
| `forge.settledTtlMs` | `2592000000` | How long a settled ChangeRequest is retained before its controller deletes it. |
| `forge.relayNotes`, `forge.relayChecks` | `true` | Whether the [Forge controller](controllers/forge.md) relays new notes, and passed or failed checks, to a Task's worker. A Task's `spec.forge.relay` overrides each. |
| `forge.relayBots` | `false` | Whether bot-authored notes are relayed to the worker. A selected policy that waits on bots relays their blockers regardless. |
| `forge.noCheckGraceMs` | `300000` | How long `checksPassed` waits, from a head's first observation, for a check to appear before treating that checkless head as passed. |
| `forge.draftPolicy` | Absent | The ready policy applied to a Task that sets none; it must name a policy below. Absent: a draft is never taken out of draft unless the Task names a policy. |
| `forge.readyPolicies.<name>` | Three written by `fleet init` | A set of the predicates the [Forge controller](controllers/forge.md#ready-policies) defines (`checksPassed`, `botReviewsAddressed`, `noUnresolvedThreads`, `forgeMergeable`; `workerDone` and `workerIdle` are always on), under a name that is only a key. Any other key is refused at load. |
| `forge.hosts.<host>` | One per forge host | `kind` is `azure-devops`, `bitbucket-cloud`, `github`, `gitlab` or `fake`; `cli` is a PATH name or absolute path (`az`, `curl`, `gh` or `glab` by default); a `fake` host takes `dir`, its directory, relative to this file. Configure `dev.azure.com` once for hosted Azure DevOps Services; its `{organization}.visualstudio.com` aliases route to that adapter. `bitbucket-cloud` is accepted only for `bitbucket.org`; Server/Data Center is a different API. A URL on any other unlisted host is refused at apply. |

Credentials stay with each external client's own configuration. Fleet passes
none to the forge and stores none. Azure DevOps login is resolved separately
for each organization; Fleet never reads or writes a PAT, token, subscription
or CLI default.
Bitbucket Cloud has no official CLI boundary here: Fleet runs bounded HTTPS
GETs through `curl --netrc-optional` and relies on curl's externally managed
config, netrc or credential helper. Fleet never constructs an Authorization
header, username, password or token. The client must be able to read
`/2.0/user`, pull requests, comments, pull-request commit statuses and
mergeability checks. Draft observation is supported, but mark-ready fails
closed because Bitbucket's `PUT draft=false` has no atomic head-and-state
precondition.
`fleet init` writes the three `readyPolicies` tables above into
the generated config and no `draftPolicy`: a Task is marked ready only once
it, or the operator's config, names a policy. The shipped
[example config](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/examples/fleet.toml) adds a fake host,
`forge.example`, with `draftPolicy = "checks"` and the same three policies,
so a declared draft can be taken out of draft offline.

### `[plugins]` section

The [Plugin controller](controllers/plugin.md) registers each
plugin here, operator-owned.

```toml
[plugins.github-issues]
program = "/usr/local/bin/node"
args = ["/opt/fleet-plugins/github-issues/dist/plugin.js"]
cwd = "/opt/fleet-plugins/github-issues"
callTimeoutMs = 15000
tasks = "own"                 # or "any"
passEnv = ["GH_TOKEN"]
envFrom = [{ name = "github-issues-env" }]
```

| Field | Required/default | Meaning |
| --- | --- | --- |
| `plugins.<name>` | One per plugin | The key is the plugin's name; the manifest's `name` must equal it. |
| `program` | yes | Absolute path of the program on the manager, as for an EnvProvider command source: no PATH search, no shell, no rebasing beside TOML. |
| `args` | `[]` | Arguments before the call name the host appends. |
| `cwd` | yes | Absolute working directory on the manager. |
| `callTimeoutMs` | `15000` | Bound for each plugin process, 1 to 300000; the tree is killed past it, and the manager lease is raised to outlast the port's reported worst case. |
| `tasks` | `"own"` | Which Tasks the plugin may patch, delete, send to or declare on: `own`, those carrying its `fleet.dev/plugin` label, or `any`. Creating a Task needs no permission beyond this entry. |
| `passEnv` | `[]` | Variables copied from the manager's environment into the plugin's observer calls only. |
| `envFrom` | `[]` | EnvProviders whose merged map reaches the observer calls only, resolved per sweep through the provider machinery TaskConfig uses; a provider's command source runs under the same bounds and lease rule. |

The controller calls, `manifest`, `probes` and `reconcile`, receive none of
these variables. Values never reach the store, a status or argv. Programs run
on the manager with the manager's user; register only programs you intend
Fleet to execute, as for EnvProvider command sources.

### The `[gateway]` section

The installed binary reads and validates this section, reads each route's
brief with it, and opens its accounts before a manager run;
ConversationController starts Tasks from its routes, and GatewayController
ties, relays, posts, logs and reacts. The
[Gateway controller](controllers/gateway.md) reads its chat
accounts, people and routes from here, as the Forge controller reads
`[forge.hosts]`.

```toml
[gateway]
callTimeoutMs = 15000
pollMs = 300000               # history backstop for tied threads
activeMs = 86400000           # ... while the thread had a message this recently
retainMs = 604800000          # seven days: an idle thread's retention, then it retires
beatMs = 60000                # a listener beats at least this often; stale after three
questions = "eng"             # the route whose first channel id takes Questions from untied Tasks
logs = { account = "slack-main", channel = "C0456EFGH" }   # every Task's Drafts, result and change news
reactions = { queued = "hourglass_flowing_sand", working = "eyes", replied = "white_check_mark" }   # the defaults; `reactions = false` turns them off

[gateway.accounts.slack-main]
platform = "slack"
team = "T0123ABCD"            # the team, or the enterprise id in an Enterprise org
bot = "U0BOT0001"             # Fleet's bot user id, checked against the platform's answer
inbound = "socket-mode"       # how the listener receives and authenticates deliveries
cli = "curl"
curlConfig = "/etc/fleet/slack-curl.conf"   # operator-owned; holds the bot token header

[gateway.accounts.discord-main]
platform = "discord"
team = "1200000000000000000"  # the guild id
bot = "1210000000000000000"   # the bot user's snowflake
inbound = "discord-gateway"   # the listener holds the Gateway WebSocket
messageContent = false        # true only when the application holds MESSAGE_CONTENT
curlConfig = "/etc/fleet/discord-curl.conf"   # holds "Authorization: Bot <token>"

[gateway.people.alex]
slack-main = "U0123ABCD"      # one platform id per account
discord-main = "1220000000000000000"          # a user snowflake, never a username

[gateway.people.sam]
slack-main = "U0456EFGH"

[gateway.routes.eng]
account = "slack-main"
channels = ["C0123ABCD"]
mention = "required"          # a new thread needs a mention of Fleet's bot
allow = ["alex", "sam"]       # names under [gateway.people]
taskConfig = "conversation"   # a TaskConfig resource: harness, host, model, env
brief = "/etc/fleet/briefs/conversation.md"
workspace = { dir = "/work/conversations" }   # a base: each thread gets /work/conversations/<conversation name>
refusal = "Sorry, I only take requests from people on this channel's list."

[gateway.routes.alerts]
account = "slack-main"
channels = ["C0789IJKL"]
mention = "top-level"         # every top-level post starts a Task
allow = ["alex", "sam"]
allowBots = ["B0123ABCD"]     # the alerting app whose posts are requests
taskConfig = "conversation"
brief = "/etc/fleet/briefs/triage.md"
workspace = { dir = "/work/triage" }   # a base: each alert's thread gets its own directory

[gateway.routes.anywhere]
account = "slack-main"
channels = ["*", "dm:*"]      # the wildcard: every other channel the bot is in; the DM entry: every DM with the bot
allow = ["alex", "sam"]       # mention = "required", the only mention a wildcard route takes; a DM needs none
taskConfig = "conversation"
brief = "/etc/fleet/briefs/conversation.md"
workspace = { dir = "/work/anywhere" }
```

| Field | Required/default | Meaning |
| --- | --- | --- |
| `gateway.callTimeoutMs` | `15000` | Bound for each adapter call, 1 to 300000; the manager lease is raised to outlast the port's reported worst case. |
| `gateway.pollMs` | `300000` | How often a Tied Conversation reads its history as a backstop for events a listener missed, where the account has history. A Conversation's `spec.pollMs` overrides it for that thread. It is also the interval at which a create waiting under `WorkspaceBusy`, for a thread's directory that another Task holds, is planned again, the cap on the recovery-read backoff, and how often a Listener asks `whoami` and `capabilities`. |
| `gateway.activeMs` | `86400000` | How long after a Tied thread's last message its history backstop keeps polling. |
| `gateway.retainMs` | `604800000` | How long a thread is kept with no message, counted from the later of its last message and its orphaning. Past it, a thread that is Untied, Orphaned or tied to a Concluded Task retires: Fleet deletes its Task, the thread directory that the manager created, and its Conversation. A thread with a running Task is kept. |
| `gateway.beatMs` | `60000` | The longest a listener goes between beats. An account's `InboundReady` turns False when three are missed. |
| `gateway.questions` | Absent | The route whose first channel id takes a new thread for a Question declared by a Task with no tie and no parent. A Task's `gateway.fleet.dev/route` label overrides it. Absent and unlabelled: such a Question is not posted. Refused when the route lists no channel id, as a route with only `"*"` or `"dm:*"` does. |
| `gateway.logs` | Absent | The log destination: `account`, a name under `[gateway.accounts]`, and `channel`, one channel id on it. Fleet posts every Task's Drafts, result and change news there, as top-level posts. Absent: Fleet posts them nowhere; a tied thread's ready notice is posted either way. Refused when the account is not configured, when the account's adapter cannot read the channel id, or when a route of that account lists the channel. A wildcard route never covers the log channel. |
| `gateway.reactions` | On: `queued = "hourglass_flowing_sand"`, `working = "eyes"`, `replied = "white_check_mark"` | The reactions that mark a person's message: `queued` while admission holds the request for its Task, `working` once the Task exists or Fleet relayed the message to it, when `queued` is removed, and `replied` once Fleet posted in the thread, when `working` is removed. Each is a platform emoji name, which each adapter maps as its [adapter page](controllers/gateway-adapters.md#capabilities) says. A name left out keeps its default. `false` turns reactions off, and `true` keeps the defaults. Refused when a name is empty or holds whitespace, when two name one emoji, or for another key. |
| `gateway.accounts.<name>` | One per chat account | `platform` is `slack`, `discord`, or `fake` with a `dir`, in this build, and later `telegram` or `teams`; a platform the build lacks is refused when the manager runs; `team` the platform's workspace identity (Slack: the team or enterprise id, optional; Discord: the guild id, required); `bot`, Fleet's bot id there, which the adapter's pure event reading uses to tell Fleet's own posts and mentions apart (Discord: a snowflake, as is `team`); `inbound`, required, the one way its listener receives deliveries, which fixes how each is authenticated (Slack: `socket-mode`, an authenticated connection, or `events-api`, a signed HTTPS delivery; Discord: `discord-gateway`, an authenticated connection; `fake`: `connection`), and an envelope claiming another is refused; `messageContent`, Discord only, default `false`, whether the application holds the privileged message content intent, which sets the `intents` that `fleet gateway account` prints; `cli`, default `curl`, and the platform's credential reference, here `curlConfig`, required for Slack and Discord, an absolute path Fleet passes to `curl -K` and never reads. The [Slack](controllers/gateway-adapters.md#configuration) and [Discord](controllers/gateway-adapters.md#configuration-1) adapter pages list each platform's keys and errors. |
| `gateway.people.<name>` | Optional | A person, with one platform id per account name. Ids only: never a display name, username or email. |
| `gateway.routes.<name>` | One per set of channels | `account`; `channels`, platform channel ids, `"*"`, the wildcard, or `"dm:*"`, the DM entry (see below); `mention`, `required` or `top-level`; `allow`, people names; `allowBots`, bot or app ids whose messages count as a person's; `taskConfig`, the TaskConfig a new conversation Task references; `brief`, an absolute path the manager reads when it loads the config and copies into each Task it creates; `workspace`, the Task's workspace as a Task spec gives it: a `dir`, an absolute base directory, below which each thread's Tasks get `<dir>/<conversation name>`, created by the manager with the Task on the TaskConfig's host (the thurbox runtime creates it on the local host only), recorded on the Conversation, and removed when the thread retires; Fleet never removes a directory it did not create or the base directory; a thread waits only while another Task holds its own directory, and no two routes' base directories may equal or contain each other; or `repositories`, which the runtime materializes for each Task, so a route's Tasks share nothing; `refusal`, the one polite refusal. A channel id belongs to at most one route. A route lists at least one channel and names at least one person in `allow` or one bot in `allowBots`. |

A wildcard route has `"*"` in `channels`. It covers every channel of its
account that the account's listener delivers, with these exceptions:

- A channel id that another route of the account lists belongs to that
  route.
- The `gateway.logs` channel belongs to no route.

These rules apply to a wildcard route:

- An account has at most one wildcard route.
- A wildcard route needs `mention = "required"`. Reason: `top-level` makes
  every top-level post a request, which suits one alert channel only.
- `allow`, `allowBots` and `refusal` apply as on any route.
- A wildcard route can also list channel ids. The first id is the channel
  where the route opens a thread for a Question.

A DM route has `"dm:*"`, the DM entry, in `channels`. The DM entry covers
every one-to-one direct message between the bot and a user of the account,
with these exceptions:

- A channel id that a route of the account lists belongs to that route.
- The `gateway.logs` channel belongs to no route.

A DM is a one-to-one direct message only when the account's adapter says
so. A group DM is not one. A group DM is a channel, and the wildcard covers
it under the mention rule.

These rules apply to a DM route:

- An account has at most one DM route.
- In a thread that the DM entry covers, a message that starts the thread
  starts a Task without a mention. A message that mentions Fleet also
  starts one. The route's `mention` applies only to its channels.
- In such a thread, a top-level message from a person who is not on
  `allow` earns the route's refusal, once per thread.
- `allow`, `allowBots` and `refusal` apply as on any route.
- A route can hold both `"*"` and `"dm:*"`, and also list channel ids.
- A Discord account refuses the DM entry. Reason: the Discord adapter
  ignores every DM.

The [adapter pages](controllers/gateway-adapters.md) say which
conversations each platform delivers, and which are DMs.

Credentials stay with the external client: Fleet passes the curl config's
path and never constructs an Authorization header. The listener holds the
platform's connection token itself. TaskConfig defines no brief or
workspace, so a route carries both.

## Environment JSON file and command output

An [EnvProvider](resources/env-provider.md) file source reads a UTF-8 JSON
object whose values are strings. Command sources must emit this same format
on stdout and exit zero. This is environment data, separate from Fleet's
[native TOML configuration](#native-fleet-configuration-fleettoml) and JSON resource manifests.

Save this example as `agent-env.json` in a private manager-local directory:

```json
{
  "PROJECT_MODE": "review",
  "GREETING": "bonjour = 🌍",
  "MULTILINE": "first line\nsecond line"
}
```

| Rule | Accepted input |
| --- | --- |
| Shape | One JSON object of string values; empty `{}` is valid. |
| Names | `[A-Za-z_][A-Za-z0-9_]*`; no case collisions such as `Path` and `PATH`. |
| Values | Strings, including empty strings, Unicode, spaces, `=` and newlines; never NUL. |
| Source size | At most 64 KiB of file/command bytes. |
| Map/merged size | Total UTF-8 key and value bytes at most 64 KiB. |

Arrays, nested objects, numbers, booleans and null values are refused. The
same key/value rules apply to literal EnvProvider `data`, TaskConfig `env`
and Task `env`. Across maps, later values replace case variants and retain
the winning spelling on every platform. See
[the complete precedence](resources/task-config.md#referencing-and-overriding-defaults).

A file must be a regular file at an absolute path on the manager. There is
no lookup beside the TOML config, resource manifest, Task workspace or remote
host. Use a native path; Windows backslashes must be escaped inside the
provider's JSON manifest. A missing, unreadable, nonregular, malformed or
oversized file prevents launch. Source parse errors never echo its content.

External files may contain credentials: restrict their access and exclude
them from public repositories. Fleet freezes their resolved values in a
private plaintext launch snapshot; protect store backups as well. Editing
the file affects future resolutions, not already frozen Tasks. Follow
[the snapshot recovery rules](resources/task-config.md#resolution-and-freezing)
for an existing execution; there is no background refresh.

## OpenTelemetry export

Fleet can export traces and metrics to an OpenTelemetry collector over OTLP.
Export is off by default. The standard `OTEL_*` environment variables turn
it on and configure it. `fleet.toml` has no telemetry section.

This page owns the telemetry interface: the variables Fleet reads, the
spans and metrics it exports, and the guarantees of the export.

### Turning export on

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

### Variables

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

### Traces

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

### Metrics

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

### What Fleet never exports

Fleet exports names, identities and outcomes only. These values never
leave the process as telemetry:

- A brief, an inbox message or a worker's result.
- An environment value, or a value that an EnvProvider resolved.
- The text of an error or of an Unknown observation.

### Guarantees

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
