# Native Fleet configuration (fleet.toml)

The native versioned TOML file selects a resource store, runtime, optional
VEGA workspace and the forges ChangeRequests are read from. It is separate
from the JSON [Task](../resources/task.md),
[TaskConfig](../resources/task-config.md),
[EnvProvider](../resources/env-provider.md) and
[ChangeRequest](../resources/change-request.md) resources applied into that
store.
`fleet apply` takes no TOML as input, and `--config` applies no resources.
There is no `[taskConfig]`, `[envProvider]` or task-template section in TOML.
There is no telemetry section either: the standard `OTEL_*` variables
configure [OpenTelemetry export](telemetry.md).

## File selection and paths

Selection order is `--config PATH`, then `FLEET_CONFIG`, then one native path:

| Platform | Native configuration path |
| --- | --- |
| Linux | `$XDG_CONFIG_HOME/fleet/config.toml`, falling back to `~/.config/fleet/config.toml`. |
| Windows | `%APPDATA%\fleet\config.toml`. |

Fleet does not search the current directory or parents. Explicit config paths
must exist, except `fleet init --config PATH`, which creates an isolated
instance without replacing an existing config. `fleet init` creates the
native config and bundled workspace; see
[initialization/customization](init.md#configuration-and-customization)
for data roots, managed files, conflict handling and per-instance paths.

Relative `data.dir`, `runtime.dir`, `starter.workspace`, a fake forge host's
`dir` and a fake gateway account's `dir` resolve beside the TOML file. A
runtime executable should be a PATH name or absolute path;
`runtime.cli` is passed to process execution without rebasing it beside TOML.
Provider source paths are always absolute and manager-local, even for remote
Tasks; they never inherit TOML's relative-path rule.

## Real-runtime example

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

## Fields

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
| `runtime.workspaceTrust` | Thurbox: `codex` is `codex`; `claude` and `claude-code` are `claude` | Harness name to its workspace-trust interface: `codex`, `claude` or `none`. Any other value is refused at load. A Task whose harness is not listed starts no session and waits in Pending with `TrustUnmapped`; the refusal spends no spawn attempt. A running `fleet up` applies a new entry after a restart. See [workspace trust](../CONTRACTS.md#workspace-trust). |
| `runtime.dir` | Required for `fake` | Isolated fake-host persistence, relative to this config file. |
| `workspaces.kind` | Optional | `worktree` or `fake`. With this section, Fleet makes each `repositories` workspace before the spawn and cleans it after the release. Without it, the runtime makes the workspace. See [the workspace port](../CONTRACTS.md#workspace-contract). |
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

### Reloading `runtime.repositories`

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
[privacy and retention](../resources/task-config.md#resolution-and-freezing).

## Offline fake-runtime example

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
[environment JSON maps](environment-json.md). Keep source credentials out of
resource manifests. Config schema `1`, resource API `fleet/v1alpha1` and store
format `1` are separate version contracts; `fleet version --json` reports
them. No live store/config migration or service restart occurs through this
feature. `fleet init --update` preserves the user-owned native config.

## The `[forge]` section

The forges that [ChangeRequests](../resources/change-request.md) are
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
| `forge.relayNotes`, `forge.relayChecks` | `true` | Whether the [Forge controller](../controllers/forge/README.md) relays new notes, and passed or failed checks, to a Task's worker. A Task's `spec.forge.relay` overrides each. |
| `forge.relayBots` | `false` | Whether bot-authored notes are relayed to the worker. A selected policy that waits on bots relays their blockers regardless. |
| `forge.noCheckGraceMs` | `300000` | How long `checksPassed` waits, from a head's first observation, for a check to appear before treating that checkless head as passed. |
| `forge.draftPolicy` | Absent | The ready policy applied to a Task that sets none; it must name a policy below. Absent: a draft is never taken out of draft unless the Task names a policy. |
| `forge.readyPolicies.<name>` | Three written by `fleet init` | A set of the predicates the [Forge controller](../controllers/forge/interface.md#ready-policies) defines (`checksPassed`, `botReviewsAddressed`, `noUnresolvedThreads`, `forgeMergeable`; `workerDone` and `workerIdle` are always on), under a name that is only a key. Any other key is refused at load. |
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

## `[plugins]` section

The [Plugin controller](../controllers/plugin/README.md) registers each
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

## The `[gateway]` section

The installed binary reads and validates this section, reads each route's
brief with it, and opens its accounts before a manager run;
ConversationController starts Tasks from its routes, and GatewayController
ties, relays, posts, logs and reacts. The
[Gateway controller](../controllers/gateway/README.md) reads its chat
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
| `gateway.reactions` | On: `queued = "hourglass_flowing_sand"`, `working = "eyes"`, `replied = "white_check_mark"` | The reactions that mark a person's message: `queued` while admission holds the request for its Task, `working` once the Task exists or Fleet relayed the message to it, when `queued` is removed, and `replied` once Fleet posted in the thread, when `working` is removed. Each is a platform emoji name, which each adapter maps as its [adapter page](../controllers/gateway/adapters/README.md#capabilities) says. A name left out keeps its default. `false` turns reactions off, and `true` keeps the defaults. Refused when a name is empty or holds whitespace, when two name one emoji, or for another key. |
| `gateway.accounts.<name>` | One per chat account | `platform` is `slack`, `discord`, or `fake` with a `dir`, in this build, and later `telegram` or `teams`; a platform the build lacks is refused when the manager runs; `team` the platform's workspace identity (Slack: the team or enterprise id, optional; Discord: the guild id, required); `bot`, Fleet's bot id there, which the adapter's pure event reading uses to tell Fleet's own posts and mentions apart (Discord: a snowflake, as is `team`); `inbound`, required, the one way its listener receives deliveries, which fixes how each is authenticated (Slack: `socket-mode`, an authenticated connection, or `events-api`, a signed HTTPS delivery; Discord: `discord-gateway`, an authenticated connection; `fake`: `connection`), and an envelope claiming another is refused; `messageContent`, Discord only, default `false`, whether the application holds the privileged message content intent, which sets the `intents` that `fleet gateway account` prints; `cli`, default `curl`, and the platform's credential reference, here `curlConfig`, required for Slack and Discord, an absolute path Fleet passes to `curl -K` and never reads. The [Slack](../controllers/gateway/adapters/slack.md#configuration) and [Discord](../controllers/gateway/adapters/discord.md#configuration) adapter pages list each platform's keys and errors. |
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

The [adapter pages](../controllers/gateway/adapters/README.md) say which
conversations each platform delivers, and which are DMs.

Credentials stay with the external client: Fleet passes the curl config's
path and never constructs an Authorization header. The listener holds the
platform's connection token itself. TaskConfig defines no brief or
workspace, so a route carries both.
