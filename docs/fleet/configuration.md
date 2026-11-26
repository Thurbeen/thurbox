# Configuration

## Initialization and `fleet up`

`fleet init` writes the native config. `fleet up` keeps every due Task
reconciling. This page owns their behaviour. The [native Fleet TOML](#native-fleet-configuration-fleettoml)
reference owns the config fields.

### Prerequisites

`init` works offline. To run real sessions, put `runtime-mux` on PATH,
install tmux, and install and authenticate each harness your Tasks name.
The [fleet-only install](README.md#install) puts `fleet`, `runtime-mux`,
`runtime-herdr` and `workspace-git` on PATH.
Fleet does not install prerequisites or change login state. `up` asks the
runtime of each configured host for its manifest. When no host answers, `up`
exits and names the runtime it could not start. When some hosts answer, `up`
reports each other host and starts. Harness login and readiness failures stay
visible in Task status: inspect `fleet status task NAME --json` before you
retry.

Fleet starts the `codex` harness with a launch override that trusts its
workspace. Claude Code can show its trust dialog in a new workspace. Fleet
never answers that dialog: trust the folder once in the session, and the
brief follows. See [workspace trust](contracts.md#workspace-trust).

### Running `fleet up`

`up` runs in the foreground and continuously reconciles every due Task. It
creates no Task of its own: Tasks come from `fleet apply`, from an interface
such as thurbox, or from a gateway route. Keep `up` running in a terminal or
your own service supervisor. Ctrl-C (also SIGTERM on Unix) finishes the
bounded sweep, releases the manager lease and retains sessions. Run
`fleet up` again to reconnect.

`fleet up` holds an exclusive OS lock on `<data dir>/manager.lock` for its
whole life. The OS releases the lock when the manager exits, however it
exits. A second `fleet up` on the same store waits while the lock is held,
whatever `lease.json` says. It prints the holder once, and it takes over
when the holder exits. The lease keeps its fencing role:
every controller write checks its token. Each new lease takes its token
from the store's `resourceVersion` counter, so a token never goes back,
even when `lease.json` is lost.

`fleet up` tells a supervisor its state through systemd's notify protocol
when `NOTIFY_SOCKET` is set:

- `STATUS=waiting for …` while another manager holds the lock.
- `READY=1` after it takes the lease and finishes its first pass. A unit
  with `Type=notify` counts as started only then.
- `STATUS=paused: disk full at <path>` while the disk of the data
  directory has no room. A pause sends no `READY=1`: a manager that
  pauses before its first pass stays not ready until it resumes.
- `STOPPING=1` when it stops.

A full disk pauses `fleet up`. It does not stop it:

1. A store write fails with no room left (`ENOSPC` or `EDQUOT`). The sweep
   stops at that write, so no effect runs without its intent.
2. The manager keeps `manager.lock`, records `state: paused` and prints
   the path that was full.
3. It tries again after 5 s, then doubles the wait up to 60 s.
4. When a write succeeds again, it renews its own lease, with an unchanged
   token, even when the lease expired during the pause. It records
   `state: serving` and resumes.

A stop during a pause cannot release the lease. The next manager takes
it over, because its holder is gone.

`fleet doctor` warns when less than 256 MiB is free on the disk of the
data directory.

`fleet up` exits with code 78 on a refusal that no retry can fix: a config
that does not parse, or a store whose format this build does not read. A
unit stops restarting it with `RestartPreventExitStatus=78`.

The manager writes its status to `<data dir>/manager.json`: `pid`, `boot`,
`version`, `exe`, `config`, `startedAt` and `state` (`starting`,
`serving`, `paused` or `stopped`). The manager records `serving` after
the first pass under its lease, and after the first pass once a full disk
has room again, not after a whole sweep. On a full disk
the manager overwrites the record in place, so `paused` reaches it. The
record is status, never authority: a manager that crashed leaves its last
state behind.

### The service unit: `fleet service`

`fleet service` writes and drives the one supervisor of `fleet up`, under
the system's own service manager:

- Linux: a systemd user unit, driven through `systemctl --user`.
- macOS: a launchd agent, driven through `launchctl` in the user's
  `gui/<uid>` domain.
- Windows: a Task Scheduler task that starts at the user's logon, driven
  through `schtasks`.

The commands below name the systemd verbs. The other two supervisors are
described after the systemd unit.

| Command | What it does |
| --- | --- |
| `fleet service install [--name NAME] [--now]` | Writes `~/.config/systemd/user/NAME.service` (`fleet` by default) for the config, runs `daemon-reload` and enables the unit. `--now` also starts it. |
| `fleet service start`, `stop`, `restart` | Runs `systemctl --user` with that verb on the store's unit. `stop` keeps the sessions. |
| `fleet service restart --expect VERSION [--timeout SECONDS]` | Restarts the store's unit with `try-restart` when it runs, and waits until a manager that started since then serves `VERSION`. Exits 0 when it serves, and 0 when the unit was not running. Exits 1 when the unit fails first, for example on exit code 78, and 7 when the wait ends. The wait is twice `TimeoutStopSec`, plus one minute, by default. |
| `fleet service status` | Prints `systemctl --user status` of the store's unit, and exits with its code: 0 when the unit is active, 3 when it is not. |
| `fleet service uninstall` | Stops and disables the store's unit, removes its file and runs `daemon-reload`. Its drop-ins stay. |

`install --now`, `start` and `restart` queue the job and return. They do not
wait for `READY=1`: a new `fleet up` waits while another manager holds the
store's lock. `fleet service status` shows when the unit is active.
`restart --expect` reads the manager record (`manager.json`): its
`version`, its `state` and its `startedAt`. The update of an install
(`thurbox-cli update`) runs it after it installs a release.

`--config` names the config, as for every command. The unit sets
`FLEET_CONFIG` to that config's absolute path. `--data-dir` is refused:
`fleet up` reads the data directory from the config. Each verb other than
`install` acts on the unit of the config's store. `--name` names another
unit.

The unit that `install` writes:

```ini
[Unit]
Description=Fleet manager (fleet up)
X-Fleet-Store=<data dir>

[Service]
Type=notify
Environment="FLEET_CONFIG=<config>"
Environment="PATH=<fleet's directory>:<PATH at install>"
ExecStart="<fleet's directory>/fleet" --config ${FLEET_CONFIG} up
Restart=on-failure
RestartSec=2s
RestartSteps=6
RestartMaxDelaySec=2min
RestartPreventExitStatus=78
TimeoutStartSec=infinity
TimeoutStopSec=<the manager lease>
KillMode=mixed

[Install]
WantedBy=default.target
```

- `<fleet's directory>` is `<prefix>/lib/thurbox/current/bin` when `fleet`
  runs from a release under `<prefix>/lib/thurbox/releases/`. A restart
  then runs the current release.
- `X-Fleet-Store` names the store. systemd ignores the key. Fleet reads it
  to find the unit of a store.
- A unit written by hand has no `X-Fleet-Store`. On Linux, `start`,
  `stop`, `restart` and `status` without `--name` drive it when no unit
  names the store, its `ExecStart` runs `fleet ... up`, and that manager
  serves the store. Fleet reads the store from `--data-dir`, else from the
  data directory of the config that `--config` or `FLEET_CONFIG` names,
  else from the default config. It reads the unit and its drop-ins
  (`NAME.service.d/*.conf`).
- Fleet skips a hand-written unit when it cannot tell the store: a
  template unit, an `EnvironmentFile=`, a unit that sets its own `HOME`,
  `XDG_CONFIG_HOME` or `XDG_DATA_HOME`, a config or data directory that is
  not an absolute path or holds a `%` specifier or a `$` variable, or a
  named config that does not load. `install` and `uninstall` never take a hand-written unit.
  `fleet version` names it as the supervisor.
- `install` refuses a second unit for the same store, with exit code 5.
  Uninstall the first unit, or install it again under its own name.
- `install` with the name of an existing unit replaces the unit file. The
  unit's drop-ins (`NAME.service.d/`) stay. Put local changes, such as more
  directories on `PATH`, in a drop-in: `systemctl --user edit NAME.service`.
- `TimeoutStopSec` is the lease that `fleet up` takes: twice the longest
  runtime, forge, gateway or plugin operation, and 30 seconds at least. A
  stop lets the current sweep finish.
- `KillMode=mixed` signals `fleet up`, and then stops every process that
  is left in the unit's cgroup. `runtime-mux` starts the multiplexer's
  server in a scope of its own ([runtime-mux](../runtime/README.md)), so
  the server, and every session on it, stays when the unit stops or
  restarts.
- A tmux server that started before `runtime-mux` used a scope is in the
  unit's cgroup. The first stop or restart of the unit stops that server.
  Fleet then starts each Task's session again, as
  [after a host reboot](controllers/task.md#after-a-host-reboot).

#### The launchd agent (macOS)

`install` writes `~/Library/LaunchAgents/NAME.plist`. The agent's label is
`NAME`. The property list holds the same command and environment as the
systemd unit, and these keys:

- `XFleetStore` names the store. launchd ignores the key.
- `RunAtLoad`, and `KeepAlive` with `SuccessfulExit` false: launchd starts
  the agent when it loads it, and starts it again after a failure, 10
  seconds apart at most (`ThrottleInterval`).
- `ExitTimeOut` is the lease that `fleet up` takes, as `TimeoutStopSec`.
- `AbandonProcessGroup` keeps the multiplexer's server when the agent
  stops. Every other process that the manager started also stays.
- The manager's output goes to `~/Library/Logs/NAME.log`.

| Command | `launchctl` |
| --- | --- |
| `install` | `enable gui/<uid>/NAME`. `--now` also loads the agent with `bootstrap` when launchd has not loaded it. Otherwise launchd loads it at the next login. |
| `start` | `kickstart`, or `bootstrap` when the agent is not loaded |
| `restart` | `kickstart -k`, or `bootstrap` when the agent is not loaded |
| `stop` | `bootout`: it stops and unloads the agent |
| `status` | `print`. Exit 0 while the agent runs, 3 while it does not |
| `uninstall` | `bootout` when the agent is loaded, and removes the property list |
| `restart --expect` | `kickstart -k`. The agent fails first when it stops with exit code 78 |

launchd has no `RestartPreventExitStatus`: it starts a manager that exits
78 again after `ThrottleInterval`. A loaded agent keeps the property list
it was loaded with. Run `fleet service stop` and then `start` to load a
rewritten one.

#### The Task Scheduler task (Windows)

`install` writes the task's XML to
`%LOCALAPPDATA%\fleet\service\NAME.xml` and registers it as the task
`NAME` with `schtasks /Create /XML`. Fleet keeps that file to find the task
of a store. The task:

- names its store in `Documentation` (`X-Fleet-Store=<data dir>`);
- starts at the user's logon, as that user, with no extra privilege;
- runs `"<fleet>" --config "<config>" up` with no time limit;
- starts again after a failure, one minute apart, at most 999 times;
- ignores a second start while it runs.

| Command | `schtasks` |
| --- | --- |
| `install` | `/Create /TN NAME /XML <file> /F`. `--now` also runs `/Run` |
| `start` | `/Run` |
| `restart` | `/End` when the task runs, then `/Run` |
| `stop` | `/End` when the task runs |
| `status` | `/Query /V /FO LIST`. Exit 0 while the task runs, 3 while it does not |
| `uninstall` | `/End`, then `/Delete`, and removes the XML file |
| `restart --expect` | `/End`, then `/Run`. The task fails first when its last result is 78 |

- `/End` ends the manager at once. It does not wait for the current sweep.
  The next manager takes over the lease of a manager that is dead.
- Fleet reads `Status` and `Last Result` from the report of `schtasks`,
  which names them in English.
- The task runs in a console window of its own.

#### The supervisor in `fleet version`

`fleet version --json` names the service in `manager.supervisor`:
`{"kind": "systemd", "unit": "fleet.service"}`, `{"kind": "launchd",
"unit": "fleet"}` or `{"kind": "task-scheduler", "unit": "fleet"}`, or
`null` when no service runs the store. An interface reads the service there
and does not guess it.

The install script (`scripts/install.sh`) runs `fleet init` and
`fleet service install --now` on Linux when a systemd user manager answers.
`THURBOX_NO_SERVICE=1` skips that step.

The lease holder is `fleet-up-<pid>@<boot id>`. On Linux, after a reboot,
a manager takes over at once a lease whose holder names another boot:
neither that manager nor any process it started still runs.

A lease holder records each process group it starts (a runtime, forge,
gateway or plugin call) in `<data dir>/children/<pid>@<boot id>/`, until
the call returns. When the holder's process is gone on the same boot, for
example after a SIGKILL, the next `fleet up` or `fleet reconcile` does
these steps:

1. It kills each process group that the dead holder recorded.
2. It waits until each group is gone, then removes its record.
3. It takes the lease at once.

When a group cannot be stopped, the record stays and the next manager
waits for the lease to expire. The check reads `/proc`, so every process
that uses one data directory must run in the same PID namespace. A holder that left no record directory,
such as one from an older Fleet, also waits for expiry.
`fleet version --json` reports the manager lease and the manager record
under `manager`, and only reads them:

```json
{"manager": {"held": true, "holder": "fleet-up-4242@6780a1c8-c1de-4350-9421-b5ff90278de2", "expiresAt": 1763120000000, "why": null, "state": "serving", "version": "3.4.0", "exe": "/usr/bin/fleet", "config": "/home/me/.config/fleet/config.toml", "supervisor": {"kind": "systemd", "unit": "fleet.service"}}}
```

- `held` is `true` while the recorded lease is unexpired and the next
  manager cannot take it at once: a manager runs.
- `held` is `false` when no lease is recorded, the lease expired or was
  released, its holder names another boot, or its holder is gone on this
  boot and left a record directory. `holder` and `expiresAt` then name the
  last lease, if any.
- `held` is `null` when the lease cannot be read. `why` says why. An
  unreadable lease never reads as `false`.
- `state`, `version`, `exe` and `config` come from `manager.json`. They
  are `null` when no manager ran. When the record cannot be read, they are
  `null` and `why` says why.
- `cli.version` is the version of the `fleet` that answers. When it
  differs from `manager.version`, the manager runs another release than the
  one installed. Restart the manager through its supervisor.
- `skew` lists the manager when it runs another release than the CLI, as
  `manager <version> is not cli <version>`. It is `[]` when the manager
  that holds the lease runs the CLI's version, when no manager runs, and
  when the lease cannot be read. `version` starts no runtime: `fleet
  doctor` also compares each `runtime-mux` (`version` area).
- `supervisor` names the unit that runs `fleet up` for the store
  ([the service unit](#the-service-unit-fleet-service)), or the unit written
  by hand that serves it. It is `null` when no unit runs the store. Fleet
  finds a hand-written unit on Linux only.
- The report takes no lease and renews none, takes no lock, and it creates
  no store.
Unknown session observations never trigger a blind replacement. Each sweep
reports a Task that is blocked at startup or that needs attention, so no
separate status query is needed to see why.

Polling defaults to one bounded sweep per second; `up --poll-ms` accepts 100
through 10000 ms. A wake request (`fleet attach --wake`, `fleet shell --wake`,
or a view of a lost session) ends the wait between sweeps at once, so a resume
waits for no poll. Each unfinished Briefed Task costs one bounded `inspect`
of its session per sweep. The lookup by key, the check for a gone window and
the check for a pause share that answer for 500 ms, until a verb changes the
session's process.
Unchanged Task status is not rewritten. A manual `fleet reconcile` remains
available for bounded diagnostics while `up` is stopped.

After a host reboot, `fleet up` revives the sessions of unfinished Tasks and
resumes their briefs. See
[after a host reboot](controllers/task.md#after-a-host-reboot).
This works only when `fleet up` starts again after the reboot. A service
supervisor must start it at boot, without a login. `fleet service install`
enables its unit for `default.target`. Let the user manager run without a
login:

```sh
loginctl enable-linger "$USER"
```

`runtime-mux` starts the multiplexer's server in a scope of its own, so the
server is not in the unit's cgroup. A unit that you write yourself can use
`KillMode=mixed`, as the unit that `fleet service install` writes does.
Without a systemd user manager, `runtime-mux` starts the server in the
cgroup of `fleet up`. Then a unit needs `KillMode=process` to keep the
sessions.

### Configuration and customization

Reference: [native Fleet TOML](#native-fleet-configuration-fleettoml),
[Task](resources/task.md), [TaskConfig](resources/task-config.md),
[EnvProvider](resources/env-provider.md), and
[environment JSON](#environment-json-file-and-command-output).

Config selection is `--config PATH`, then `FLEET_CONFIG`, then exactly one
native per-user path: `$XDG_CONFIG_HOME/fleet/config.toml` (fallback
`~/.config/fleet/config.toml`). Fleet never searches the current directory or
its parents. Explicit paths must exist for commands other than `init`;
`init --config PATH` creates a separate configuration and a default data
root at `<native-data>/fleet/instances/<config-path-hash>/`. Relative paths
inside TOML resolve beside that file. With no native data-home variables,
explicit `--config` uses a `fleet-data` directory beside the config as its
data root. An explicit store must be separate from the config.

Default init writes a fresh resource store path at
`$XDG_DATA_HOME/fleet/store-v1` (fallback `~/.local/share/fleet/store-v1`).
The config records absolute paths and a session key prefix derived from the
config's path, so two configs never share a session. It writes
`runtimes.mux.callTimeoutMs = 10000`, below the built-in default of `15000`;
edit it to change the bound. No runtime under `[runtimes]` passes a model to
its harness: it refuses a Task that sets `spec.model`. Give the model in the
harness's own configuration instead.

`init` prints what it set up and what comes next: the config path, the places
sessions run in, the coding agents found on PATH, and the commands to run.
thurbox runs `init` itself on its first start when no config exists.

The config is yours once written. A repeated `init` validates it and keeps
it as it is; a malformed config is an error, never replaced. A symlink at the
config's path is refused; a symlinked directory above it is not. Initializers serialize with an OS lock,
so two concurrent `init` runs write one config.

### Migrating a Fleet 2 config: `fleet migrate`

Fleet 3 refuses a Fleet 2 config by name. `fleet migrate` turns the Fleet 2
config and store into Fleet 3's. `fleet migrate --dry-run` prints the same
report and changes nothing.

1. It writes a journal, `config.toml.migrating`, beside the config.
2. It writes the new store beside the 2.x store, as `store-v1.v3`.
3. It writes a copy of the 2.x config, `config.toml.v2`.
4. It renames the 2.x store to `store-v1.v2`, and moves the new store into
   place.
5. It replaces the config with the new one in one step.
6. It removes the journal.

The config path always holds the 2.x config or the new one. It deletes no
2.x file. It removes only its own journal and an unfinished
`store-v1.v3.partial`. It holds the lock that `fleet init` takes.

It refuses to run in these cases:

- A Fleet 2 manager holds the store's lease.
- With no journal, `config.toml.v2`, `store-v1.v2` or `store-v1.v3`
  already exists.

A run that stopped part way leaves the journal, and the next run completes
from there. A run that finds a Fleet 3 config changes nothing.

A Fleet 2 config with no `[data] dir` names the data directory itself as its
store, for example `~/.local/share/fleet`.

`fleet migrate` reads a Fleet 2 config only at the config path. If you moved
the 2.x files aside by hand and ran `fleet init`, it finds a Fleet 3 config
and changes nothing.

The new config is what `fleet init` writes, with these sections of the 2.x
config:

- `[forge]` and `[plugins]`, as they are. `[forge.readyPolicies]` gets the
  three shipped policies when the 2.x config has none.
- `[gateway]`, when Fleet 3 accepts it. Otherwise the report says to copy it
  by hand.

The report names each key and table it does not copy, with the reason:
`[runtime]`, `[starter]` and `[workspaces]`.

The new store holds each Task, TaskConfig and EnvProvider of the 2.x store,
with their names and labels, and without their status:

- A Task's `spec.workspace` becomes a list of URLs. `{dir}` becomes a
  `file://` URL. Each repository becomes a `git+` URL: `git+file://` on the
  checkout that `[runtime.repositories]` maps it to, else `git+` on its URL.
  A Task that had a session continues the branch that 2.x made, with
  `?branch=`.
- A host other than `local` becomes the place `mux/<host>`. The report says
  to add `[runtimes.mux.places.<host>]`.
- `spec.model` is removed, and the report says so.
- `spec.dispatch` is `false`: no session starts until you set it to `true`.
- A concluded Task, a Task that was being deleted, and the objects of other
  kinds are not imported. The report names each one. Its record stays in
  the 2.x store.

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
once, when the session is ready. `fleet event publish` adds an event to a
[Board](resources/board.md) that every Task can read. `fleet event list`
reads a Board's events after a cursor, `fleet event wait` waits for the
next match, and `fleet event watch` follows the Board.

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

The native versioned TOML file selects a resource store, runtimes and hosts,
and the forges ChangeRequests are read from. It is separate
from the JSON [Task](resources/task.md),
[TaskConfig](resources/task-config.md),
[EnvProvider](resources/env-provider.md) and
[ChangeRequest](resources/change-request.md) resources applied into that
store.
`fleet apply` takes no TOML as input, and `--config` applies no resources.
There is no `[taskConfig]`, `[envProvider]` or task-template section in TOML.
The `[telemetry]` section configures
[OpenTelemetry export](#the-telemetry-section).

### File selection and paths

Selection order is `--config PATH`, then `FLEET_CONFIG`, then one native path:

| Platform | Native configuration path |
| --- | --- |
| Linux, macOS | `$XDG_CONFIG_HOME/fleet/config.toml`, falling back to `~/.config/fleet/config.toml`. |
| Windows | `%APPDATA%\fleet\config.toml`. |

Fleet does not search the current directory or parents. Explicit config paths
must exist, except `fleet init --config PATH`, which creates an isolated
instance without replacing an existing config. `fleet init` creates the
native config; see
[initialization/customization](#configuration-and-customization)
for data roots and per-instance paths.

Relative `data.dir`, `runtime.dir`, a fake forge host's
`dir` and a fake gateway account's `dir` resolve beside the TOML file. A
runtime executable should be a PATH name or absolute path;
`runtimes.<name>.program` is passed to process execution without rebasing it
beside TOML.
Provider source paths are always absolute and manager-local, even for remote
Tasks; they never inherit TOML's relative-path rule.

### Runtimes and their places

[Where sessions run](../where-sessions-run.md) explains Fleets and places, and
the setups they allow.

A runtime is a program that speaks the
[runtime protocol](../runtime/README.md#the-runtime-protocol). Each one is a
`[runtimes.<name>]` table. A place is where a runtime runs a process,
written `<runtime>/<place>`, and a Task names one in `spec.host`
([ADR-42](../thurbox/architecture.md#adr-42-hosts-belong-to-runtimes)):

```toml
version = 1

[data]
dir = "/home/me/.local/share/fleet/store-v1"

[runtimes.mux]
program = "runtime-mux"
callTimeoutMs = 10000
keyPrefix = "f0123456789abcdef01234567"
```

This is the runtime part of what `fleet init` writes: an absolute store
path, a `keyPrefix` derived from the config's path, and `runtime-mux` and
nothing else. `init` also writes the three shipped `[forge.readyPolicies]`,
and the TaskConfig `default` and the [Boards](resources/board.md) `shared`
and `fleet` into the store
([the default TaskConfig](resources/task-config.md#the-default-taskconfig)).
A runtime with no `places` table has one place, `local`, so this config has
one place, `mux/local`, also named `local`: this machine.
Fleet never falls back to another runtime. With no multiplexer on the
machine, the first start fails, naming the missing binary, and the Task's
`Spawned` condition says so. Another runtime is opt-in: add its table.
`fleet runtime check -- <program>` tells whether a program speaks the
protocol before a config names it.

A runtime declares its places inline, one table each:

```toml
[runtimes.mux]
program = "runtime-mux"
default = true

# this machine; spec.host = "local" or "mux/local"
[runtimes.mux.places.local]

# a machine over ssh; spec.host = "mux/devbox"
[runtimes.mux.places.devbox]
kind = "ssh"
destination = "me@devbox"

# a dynamic place: one new VM per key; spec.host = "vm/sandbox"
[runtimes.vm]
program = "runtime-vm"

[runtimes.vm.places.sandbox]
image = "agent-base"
```

- Fleet passes a place's table to the runtime in each request
  ([places](../runtime/README.md#places-protocol-version-2)) and does not
  read it. The keys are the runtime's own. `runtime-mux` takes `local` with
  no table, and [remote places](#remote-places-on-runtime-mux) of
  `kind = "ssh"` or `kind = "wsl"`.
- `local` is the short form of `<runtime>/local` on the default runtime: the
  one with `default = true`, or the only runtime. Fleet refuses a config with
  several runtimes and no single default at load.
- A default runtime whose `places` table has no `local`, such as one of VMs
  alone, leaves `local` naming no place. Fleet refuses to apply a Task on
  `local`, and says why. A Task with no `spec.host` takes
  its TaskConfig's, else `local`, by the rule the manager launches it by; one
  whose TaskConfig is not there yet is not checked. `fleet doctor`
  warns, and `fleet runtime places --json` has `"local": null` and a
  `localWhy`.
- A runtime or place name is 1 to 253 lowercase letters, digits, `-` or `.`.
  It starts and ends with a letter or a digit, and holds no `..`.
- A place's table comes only from this file. The runtime trusts it.
- A runtime of protocol version 1 serves only `local`, with no config: a
  Task on another of its places waits, unknown.
- `fleet doctor` fails a place its runtime reports not ready, with the
  runtime's why. It reads readiness by the same rule as `fleet runtime
  places`.
- `fleet runtime places --json` lists every place with what its runtime's
  manifest says of it: `{"places": [{place, runtime, name, alias, platform,
  dynamic, ready, why}]}`. `place` is the id, `<runtime>/<place>`, and
  `alias` is `"local"` on the place `local` names.
  `ready` is `null` when the runtime did not answer: its places are unknown,
  never absent. `local` is the id `local` names, or `null` with
  `localWhy` saying why. With the fake `[runtime]`, `places` is `null`.
- `fleet runtime hosts --json` lists the place ids a Task may name, `local` first
  and then each other place. thurbox's new-session flow reads it.
- `fleet runtime browse <place> [dir] --json` reads a place's folders
  through its runtime's `browse` verb: `{"dir", "entries": [{name, repo}]}`,
  each subdirectory of `dir` by name, `repo` true on the top of a git work
  tree. Without `dir` it reads the place's home. `fleet runtime branches
  <place> <repo> --json` reads a repository's branches there through
  `branches`: `{"names", "default"}`. `<place>` is `local` or
  `<runtime>/<place>`. Both only read, and a place the config does not
  define, a runtime whose manifest does not offer the verb, and a directory
  the runtime cannot read are errors that say why
  ([browse and branches](../runtime/README.md#browse-and-branches)).

#### Remote places on runtime-mux

A remote place of `runtime-mux` is a machine over ssh or a WSL distro. The
agent runs there, on that machine's multiplexer:

```toml
[runtimes.mux]
program = "runtime-mux"

[runtimes.mux.places.local]

# spec.host = "mux/nixos-hp"
[runtimes.mux.places.nixos-hp]
kind = "ssh"
destination = "nixos-hp"
ssh_opts = ["-o", "ControlMaster=auto", "-o", "ControlPersist=10m"]
path_prepend = ["~/.cargo/bin"]

# spec.host = "mux/ubuntu"; runtime-mux runs on Windows
[runtimes.mux.places.ubuntu]
kind = "wsl"
distro = "Ubuntu-24.04"
```

| Key | Default | Meaning |
| --- | --- | --- |
| `kind` | required | `ssh` or `wsl`. |
| `destination` | required for `ssh` | The ssh destination, such as `me@devbox`. `~/.ssh/config` applies. |
| `ssh_opts` | `[]` | `ssh` options before the destination. |
| `distro` | the place's name | The WSL distro, for `wsl`. |
| `multiplexer` | `tmux` | The multiplexer on the place: `tmux`, `psmux` or `rmux`. |
| `platform` | `posix`, or `windows` with `psmux` | The place's operating system. |
| `socket` | `runtime-mux` | The multiplexer's socket on the place. |
| `session` | the default | The multiplexer's session name on the place. |
| `path_prepend` | `[]` | Directories put first on the agent's `PATH` on the place, before its login `PATH`. Absolute or `~`-rooted. |

- `runtime-mux` refuses any other key, and names it.
- The place needs `ssh -o BatchMode=yes <destination> true` to succeed,
  tmux 3.2 or newer (or psmux), and `fleet` and the agent on its `PATH`.
  A `git+` workspace also needs `workspace-git` and `git` there, and a WSL
  place needs `flock`.
  `fleet runtime places` and `fleet doctor` say which one is missing, with
  the command that shows or fixes it.
- `start`, `inspect`, `stop`, `pause`, `list`, `input`, `capture` and `exec`
  run on the place. A harness's hook files, such as pi's extension, are
  copied to the place at each start that names them.
- A native-Windows place (`multiplexer = "psmux"`) runs the same verbs
  through PowerShell, and Fleet reads its agents' state there with the
  place's own `fleet`. `browse`, `branches` and workspace setup steps need
  `sh`, and are refused there by name.
- A Task asks the runtime about its own place only. A place that does not
  answer is reported not ready after 8 s, and never holds up a Task on
  another place.
- A workspace on a remote place is made on the place by its own
  `workspace-git`, under `~/.local/share/runtime-mux/workspaces`. A `git+`
  URL names a repository as the place reaches it, and a `file://` URL a
  directory on the place. A release removes the worktree there and keeps
  its work as on `local`.
- A WSL place stays running while its multiplexer's server runs:
  `runtime-mux` keeps one `wsl.exe` waiter attached to it
  ([runtime-mux's places](../runtime/README.md#runtime-muxs-places)).
- `fleet attach`, `fleet shell`, `fleet runtime browse` and
  `fleet runtime branches` read the place, so thurbox's new-session dialog
  picks a folder and a branch there.

#### Places on runtime-herdr

[`runtime-herdr`](../runtime/herdr.md) runs each Task in a herdr-core
terminal, which herdr's desktop app shows. [Fleet for herdr
users](../runtime/herdr-users.md) walks through the setup:

```toml
[runtimes.herdr]
program = "runtime-herdr"

# herdr-core's default session; spec.host = "herdr/local"
[runtimes.herdr.places.local]

# the herdr-core session "work"; spec.host = "herdr/work"
[runtimes.herdr.places.work]
session = "work"
space = "agents"
```

| Key | Default | Meaning |
| --- | --- | --- |
| `session` | herdr-core's default session | The herdr-core session (`HC_SESSION`). |
| `space` | none: each Task has a space of its own | One herdr app space for every Task of the place, each Task a tab in it, as their `herdr.space` label. |

- `runtime-herdr` refuses any other key, and names it.
- With `default = true` on `[runtimes.herdr]`, `local` names
  `herdr/local`.
- A place needs `herdr-core` on the `PATH`. `fleet runtime places` and
  `fleet doctor` say when it is missing.

A second Fleet installation on the same runtime needs a distinct
`keyPrefix`. Store and config must remain separate paths.

### Fake runtime

`[runtime]`, a single table, configures the offline fake runtime for tests
and trials. It excludes `[runtimes]`: a file with both is refused. See the [offline fake-runtime example](#offline-fake-runtime-example).

### Fields

| Field | Required/default | Meaning |
| --- | --- | --- |
| `version` | Required, `1` | Native config schema; unsupported versions and unknown fields are refused. |
| `data.dir` | Optional | Resource store. `--data-dir` overrides it; otherwise Fleet uses its native data directory. |
| `runtimes.<name>.program` | Required for each runtime | The runtime's executable, a path or a bare name. Fleet looks for a bare name beside its own executable first, then on PATH, so a manager runs its own release's runtime. Fleet runs `<program> [args] <verb>` through its bounded spawner. |
| `runtimes.<name>.args` | `[]` | Arguments before the verb. |
| `runtimes.<name>.callTimeoutMs` | `15000` | Bound for each call but `workspace`, 1 to 300000; the manager lease must outlast the runtime's reported worst case. |
| `runtimes.<name>.workspaceTimeoutMs` | `600000` | Bound for a `workspace` call, which can clone a repository. Unknown on timeout concludes nothing: the next pass asks again under the same key. |
| `runtimes.<name>.keyPrefix` | `fleet` | The first part of every key Fleet gives the runtime, `<keyPrefix>.<uid>`; at most 32 lowercase letters, digits and `-`. |
| `runtimes.<name>.default` | `false` | The runtime whose `local` place `local` names. Required on one runtime when there are several. |
| `runtimes.<name>.places.<place>` | one place, `local` | A place of the runtime: a table Fleet passes to it in each request and does not read. A Task names it as `spec.host = "<name>/<place>"`; one naming a place no runtime has is unreachable and waits. |
| `runtime.kind` | Required when configuring `[runtime]` | `fake`; excludes `[runtimes]`. |
| `runtime.dir` | Required for `fake` | Isolated fake-host persistence, relative to this config file. |

`--data-dir` is for explicit store operations. `fleet up` rejects a differing
override because its workers must share the store selected by the persisted
config. Persist `data.dir` before using `up` with a different store. All
resource commands in one workflow should use the same `--config` path.

`fleet up` reads this file once, when it starts. A change to it applies at
the next start: a running process never switches its store, lease holder,
runtime or key prefix.

Each runtime makes the workspaces of the hosts it serves, from the URLs a
Task names ([workspaces](../runtime/README.md#workspaces)).

Every session Fleet starts gets `FLEET_SIGNAL_FILE`, `FLEET_HARNESS` and
`FLEET_HARNESS_DIR`, the directory below
([signal contract](signal-contract.md)). On `local`, the signal file is
under `<data dir>/signals`. On any other place, it is
`~/.local/state/fleet/signals/<key>.jsonl` on that place, and Fleet reads
it through the runtime's `exec`.

The `harnesses/` directory beside the config file holds the user's
[harness profiles](controllers/task.md#harness-profiles), one `*.toml` file
each. Every command that loads the config reads them at start, and a file
that does not parse stops the command with its path. `fleet signal` and
`fleet version` skip such a file instead. A user profile replaces the
bundled profile of the same `name`, or adds a harness. A Task whose launch
is frozen (it names a TaskConfig or an `env`) keeps the profile's
`[launch] env` as it was when the launch was resolved.

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

This is the executable [example config](../../examples/fleet/fleet.toml). Fake
sessions run no agent, although explicitly configured external EnvProvider
commands still execute locally. Do not treat the fake runtime as a sandbox
for untrusted helpers.

```sh
fleet --config examples/fleet/fleet.toml apply -f examples/fleet/resources.json
fleet --config examples/fleet/fleet.toml list tasks --json
fleet --config examples/fleet/fleet.toml reconcile
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
them. A repeated `fleet init` keeps the user-owned native config, the
TaskConfig `default` and the Boards `shared` and `fleet` when the store holds
them.

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
[example config](../../examples/fleet/fleet.toml) adds a fake host,
`forge.example`, with `draftPolicy = "checks"` and the same three policies,
so a declared draft can be taken out of draft offline.

### Supported forges

Fleet has one adapter for each forge in this table.
To follow a change, the adapter reads its checks, review threads, notes and
merge state. The [Forge controller](controllers/forge.md#relay) relays news
from them to the worker. Mark ready takes a draft out of draft. Fleet does not
check a minimum client version.

| Forge | Client | Follow | Mark ready | `[forge.hosts]` entry | Self-hosted |
| --- | --- | --- | --- | --- | --- |
| GitHub | `gh api graphql` | Yes | Yes | `"github.com" = { kind = "github", cli = "gh" }` | Yes: GitHub Enterprise Server, under its own hostname |
| GitLab | `glab api` | Yes | Yes | `"gitlab.com" = { kind = "gitlab", cli = "glab" }` | Yes: a self-hosted GitLab, under its own hostname |
| Azure DevOps | `az devops invoke`, REST 7.1 | Yes | No | `"dev.azure.com" = { kind = "azure-devops", cli = "az" }` | No: Azure DevOps Services only. Azure DevOps Server is not supported. |
| Bitbucket Cloud | `curl`, REST 2.0 | Yes | No | `"bitbucket.org" = { kind = "bitbucket-cloud", cli = "curl" }` | No: `bitbucket.org` only. Bitbucket Data Center is not supported. |

- Azure DevOps and Bitbucket Cloud have no atomic head-and-state check on the
  draft update. Fleet refuses mark ready on them.
- GitLab marks a merge request ready when Fleet removes the draft marker from
  its title. Fleet refuses a draft whose title has no marker.
- On Azure DevOps and Bitbucket Cloud, reviewer votes set only the review
  decision. Azure DevOps marks no note as a bot's.

The [adapter truth tables](controllers/change-request.md#adapter-truth-tables)
give each field's mapping.

Fleet ships these four.

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
workspace = { base = "/work/conversations" }   # a base: each thread gets /work/conversations/<conversation name>
refusal = "Sorry, I only take requests from people on this channel's list."

[gateway.routes.alerts]
account = "slack-main"
channels = ["C0789IJKL"]
mention = "top-level"         # every top-level post starts a Task
allow = ["alex", "sam"]
allowBots = ["B0123ABCD"]     # the alerting app whose posts are requests
taskConfig = "conversation"
brief = "/etc/fleet/briefs/triage.md"
workspace = { base = "/work/triage" }   # a base: each alert's thread gets its own directory

[gateway.routes.anywhere]
account = "slack-main"
channels = ["*", "dm:*"]      # the wildcard: every other channel the bot is in; the DM entry: every DM with the bot
allow = ["alex", "sam"]       # mention = "required", the only mention a wildcard route takes; a DM needs none
taskConfig = "conversation"
brief = "/etc/fleet/briefs/conversation.md"
workspace = { base = "/work/anywhere" }
```

| Field | Required/default | Meaning |
| --- | --- | --- |
| `gateway.callTimeoutMs` | `15000` | Bound for each adapter call, 1 to 300000; the manager lease is raised to outlast the port's reported worst case. |
| `gateway.pollMs` | `300000` | How often a Tied Conversation reads its history as a backstop for events a listener missed, where the account has history. A Conversation's `spec.pollMs` overrides it for that thread. It is also the interval at which a create waiting under `WorkspaceBusy`, for a thread's directory that another Task holds, is planned again, the cap on the recovery-read backoff, and how often a Listener asks `whoami` and `capabilities`. |
| `gateway.activeMs` | `86400000` | How long after a Tied thread's last message its history backstop keeps polling. |
| `gateway.retainMs` | `604800000` | How long a thread is kept with no message, counted from the later of its last message and its orphaning. Past it, a thread that is Untied, Orphaned or tied to a Concluded Task retires: Fleet deletes its Task, the thread directory that the manager created, and its Conversation. A thread with a running Task is kept. |
| `gateway.beatMs` | `60000` | The longest a listener goes between beats. An account's `InboundReady` turns False when three are missed. |
| `gateway.questions` | Absent | The route whose first channel id takes a new thread for a Question declared by a Task with no tie and no parent. A Task's `gateway.fleet.dev/route` label overrides it. Absent and unlabelled: such a Question is not posted. Refused when the route lists no channel id, as a route with only `"*"` or `"dm:*"` does. |
| `gateway.logs` | Absent | The log destination: `account`, a name under `[gateway.accounts]`, and `channel`, one channel id on it. Fleet posts every Task's Drafts, result and change news there, as top-level posts. Absent: Fleet posts them nowhere; a tied thread's ready and answered notices are posted either way. Refused when the account is not configured, when the account's adapter cannot read the channel id, or when a route of that account lists the channel. A wildcard route never covers the log channel. |
| `gateway.reactions` | On: `queued = "hourglass_flowing_sand"`, `working = "eyes"`, `replied = "white_check_mark"` | The reactions that mark a person's message: `queued` while admission holds the request for its Task, `working` once the Task exists or Fleet relayed the message to it, when `queued` is removed, and `replied` once Fleet posted in the thread, when `working` is removed. Each is a platform emoji name, which each adapter maps as its [adapter page](controllers/gateway-adapters.md#capabilities) says. A name left out keeps its default. `false` turns reactions off, and `true` keeps the defaults. Refused when a name is empty or holds whitespace, when two name one emoji, or for another key. |
| `gateway.accounts.<name>` | One per chat account | `platform` is `slack`, `discord`, or `fake` with a `dir`; any other platform is refused when the manager runs; `team` the platform's workspace identity (Slack: the team or enterprise id, optional; Discord: the guild id, required); `bot`, Fleet's bot id there, which the adapter's pure event reading uses to tell Fleet's own posts and mentions apart (Discord: a snowflake, as is `team`); `inbound`, required, the one way its listener receives deliveries, which fixes how each is authenticated (Slack: `socket-mode`, an authenticated connection, or `events-api`, a signed HTTPS delivery; Discord: `discord-gateway`, an authenticated connection; `fake`: `connection`), and an envelope claiming another is refused; `messageContent`, Discord only, default `false`, whether the application holds the privileged message content intent, which sets the `intents` that `fleet gateway account` prints; `cli`, default `curl`, and the platform's credential reference, here `curlConfig`, required for Slack and Discord, an absolute path Fleet passes to `curl -K` and never reads. The [Slack](controllers/gateway-adapters.md#configuration) and [Discord](controllers/gateway-adapters.md#configuration-1) adapter pages list each platform's keys and errors. |
| `gateway.people.<name>` | Optional | A person, with one platform id per account name. Ids only: never a display name, username or email. |
| `gateway.routes.<name>` | One per set of channels | `account`; `channels`, platform channel ids, `"*"`, the wildcard, or `"dm:*"`, the DM entry (see below); `mention`, `required` or `top-level`; `allow`, people names; `allowBots`, bot or app ids whose messages count as a person's; `taskConfig`, the TaskConfig a new conversation Task references; `brief`, an absolute path the manager reads when it loads the config and copies into each Task it creates; `workspace`, the Task's workspace: `{base}`, an absolute base directory, below which each thread's Tasks get `["file://<base>/<conversation name>"]`, the directory created by the manager with the Task on the TaskConfig's host, recorded on the Conversation, and removed when the thread retires; Fleet never removes a directory it did not create or the base directory; a thread waits only while another Task holds its own directory, and no two routes' base directories may equal or contain each other; or a list of absolute URLs, which each Task gets as it is and its runtime makes for it, so a route's Tasks share nothing (a `git+` URL gives each Task its own worktree); `refusal`, the one polite refusal. A channel id belongs to at most one route. A route lists at least one channel and names at least one person in `allow` or one bot in `allowBots`. |

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
[the complete precedence](resources/task-config.md#spec).

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

The `[telemetry]` section and the standard `OTEL_*` environment variables
turn on OpenTelemetry export. An `OTEL_*` variable that is set wins over the
section's value for that variable.
[Observability](../observability.md) owns the keys, the variables, the
spans, the metrics and the guarantees of the export, for Fleet and every
other program.

### The `[telemetry]` section

```toml
[telemetry]
endpoint = "http://localhost:4318"
headers = { file = "/run/user/1000/otel-headers" }   # holds key=value,...; never the secret here
resource = { "service.namespace" = "fleet", "deployment.environment.name" = "local" }
```

Each `fleet` command reads the section: `fleet up`, `fleet reconcile`, and a
one-shot command that an agent's hook or a shell runs. Fleet gives the
values in force to each `runtime-mux` call and to each agent that it
starts ([coding agents](../observability.md#coding-agents)). The keys and
their rules are in [Config files](../observability.md#config-files).
`fleet doctor` shows the export in force, and where each value comes from.

```sh
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 fleet up   # a variable wins over the section
```
