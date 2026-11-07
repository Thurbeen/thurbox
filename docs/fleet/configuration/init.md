# Initialization and the VEGA starter

`fleet init` creates the native config and the bundled, editable VEGA
workspace. `fleet up` starts VEGA and keeps every due Task reconciling.
This page owns their behaviour. The [native Fleet TOML](fleet-toml.md)
reference owns the config fields.

## Prerequisites

`init` works offline. To run real sessions, install Thurbox (`thurbox-cli`
on PATH), configure its local multiplexer, and install and authenticate the
chosen harness first. Fleet does not install prerequisites or change
login state. `up` checks host availability and reports unavailable or unknown
prerequisites. Harness login/readiness failures remain visible in Task status;
inspect `fleet status task vega --json` and Thurbox's session before retrying.

## Choosing the harness

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
[`runtime.workspaceTrust`](fleet-toml.md) before the first `up`. Without that
entry, `Task/vega` waits and starts no session.

## Running `fleet up`

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
[after a host reboot](../controllers/task/state-machine.md#after-a-host-reboot).
This works only when `fleet up` starts again after the reboot. A service
supervisor must start it at boot, without a login. For a systemd user unit
that has `WantedBy=default.target` in its `[Install]` section:

```sh
loginctl enable-linger "$USER"
systemctl --user enable fleet-vega.service
```

## Configuration and customization

Reference: [native Fleet TOML](fleet-toml.md),
[Task](../resources/task.md), [TaskConfig](../resources/task-config.md),
[EnvProvider](../resources/env-provider.md), and
[environment JSON](environment-json.md).

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

## Resource commands

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
across retries and restarts. See [execution configuration](../resources/task-config.md)
for runnable manifests, precedence, security boundaries and snapshot recovery.

Register trusted protocol-1 controller programs under `[plugins.<name>]` in
the config. Their manifests add schema-validated resource kinds; `fleet apply`,
`get`, `list`, `status` and `delete` accept those kinds. Inspect registration
with `fleet list plugins --json`, and run their controllers with
`fleet reconcile` or `fleet up`. The
[Plugin controller guide](../controllers/plugin/README.md) defines the
protocol, admitted operations, effect recovery and environment separation.
Plugins can create Tasks and, by default, steer only Tasks carrying their
creator label. Worker results remain the worker's responsibility. Plugin
programs run as the manager user; controller environment scrubbing is a
purity contract, not a sandbox.
