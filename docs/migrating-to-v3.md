# Migrating to v3

thurbox 3.x and Fleet 3 replace thurbox 2.x and Fleet 2. This page lists
what you move, what you write again and what you remove. It is for an
operator who ran thurbox 2.x, Fleet 2, or both, on one machine or more.

[Releasing](thurbox/releasing.md#v2-and-v3) owns why the two lines exist and
what each updater does. [Where sessions run](where-sessions-run.md) owns the
setups that 3.x allows.

## Who needs to migrate

- You run thurbox 2.x or Fleet 2, and you want 3.x.
- You want to stay on 2.x. A 2.x install never updates itself to 3.x.
  Install with `VERSION=v2.x.y` (`THURBOX_VERSION` on Windows) to keep 2.x.
- A new user has nothing to migrate. Start at
  [Install](../README.md#install).

## What changed

Two commands do the move, and neither deletes a 2.x file:

- `fleet migrate` turns the Fleet 2 config and store into Fleet 3's
  ([step 2](#2-migrate-the-fleet-config-and-store)).
- `thurbox-cli migrate` turns the thurbox 2.x sessions and automations into
  Fleet Tasks and CronTasks. It prints the places and harness profiles to add
  to Fleet's config ([step 3](#3-start-your-work-again-as-tasks)).

| 2.x | 3.x |
| --- | --- |
| thurbox keeps sessions and automations in `thurbox.db`. | Every session is a Fleet Task. thurbox keeps no database. `thurbox-cli migrate` reads `thurbox.db` once. |
| Fleet 2's `[runtime]`: `kind = "thurbox"`, `cli`, `keyPrefix`, `repositories`. | `[runtimes.<name>]` tables that name a runtime program, such as `runtime-mux`, and its places ([runtimes and their places](fleet/configuration.md#runtimes-and-their-places)). |
| `[runtime.repositories]` maps a remote URL to a checkout. | No mapping. A Task names `git+` URLs in `spec.workspace`, and the runtime makes a worktree per Task ([Workspace](fleet/resources/workspace.md)). |
| A Task's `spec.workspace` is an object: `dir` or `repositories`. | `spec.workspace` is a list of absolute URLs. `fleet apply` refuses the old shapes, and `fleet migrate` converts them. |
| Sessions run on the `thurbox` tmux socket. | `runtime-mux` runs Task sessions on its own socket, `runtime-mux`. |
| `hosts.toml` lists remote machines. | thurbox lists Fleets under `[fleets.<name>]` in `settings.toml`. A Fleet's runtime lists its places. |
| `agents.toml` defines agents. | Agents are Fleet's harness profiles ([Agents](thurbox/agents.md)). |
| Automations in `thurbox.db`, and `thurbox-cli automation tick`. | Nothing in thurbox. Use a [CronTask](fleet/resources/cron-task.md). |
| `spec.model` and `runtime.modelEnv`. | No runtime passes a model. Fleet refuses a Task that sets `spec.model`. Set the model in the harness's own configuration. |
| VEGA ships inside Fleet 2, and `fleet init` writes its workspace. | VEGA installs from [its own repository](https://github.com/Thurbeen/vega). |

Both lines use the same paths: `~/.config/thurbox`, `~/.local/share/thurbox`,
`~/.config/fleet` and `~/.local/share/fleet` (the native per-user
directories on macOS and Windows). Both install the same program names.
3.x replaces 2.x on a machine. The two lines cannot run side by side.

## Before you install

1. Finish or stop the 2.x work you need. Push every branch that holds work
   you keep.
2. Stop the Fleet 2 manager: its service, or the `fleet up` that you started
   by hand. `fleet migrate` refuses a store that a manager holds.
3. Back up the four directories above.
4. Install `sqlite3` on each machine that ran thurbox 2.x.
   `thurbox-cli migrate` runs it to read `thurbox.db`, and only reads.

## 1. Install 3.x on every machine

Run the install script again. No updater moves 2.x to 3.x:
`thurbox-cli update`, Homebrew, the AUR, Chocolatey and winget stay on 2.x.
A Nix flake input that follows `main`, or a source build, gives 3.x too.

```sh
curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
```

Do this on each machine that runs a Fleet, and on each remote place. A
remote place needs Fleet 3's `fleet` on its `PATH`: the agents' status
hooks run `fleet signal` there. See [step 8](#8-set-up-remote-places).

Then check that no old binary comes first on `PATH`:

```sh
command -v fleet thurbox thurbox-cli runtime-mux workspace-git
fleet --version
```

## 2. Migrate the Fleet config and store

Fleet 3 refuses a Fleet 2 config by name, and names `fleet migrate`. Read
the plan first, then migrate:

```sh
fleet migrate --dry-run
fleet migrate
fleet doctor
```

`fleet migrate` keeps the 2.x config as `config.toml.v2` and the 2.x store
as `store-v1.v2`. It imports each Fleet 2 Task with `spec.dispatch = false`:
no session starts until you set `spec.dispatch` to `true` and apply the
Task again. Its report lists what it imported, what it did not, and why.
[`fleet migrate`](fleet/configuration.md#migrating-a-fleet-2-config-fleet-migrate)
owns the rules.

If you moved the 2.x files aside by hand and ran `fleet init` already, move
the Fleet 3 config and store aside, move the `.v2` files back, and run
`fleet migrate`.

Then do what the report lists under *For you to do*:

- Each remote machine that a Task names: add it as a place under
  `[runtimes.mux.places]`
  ([remote places](fleet/configuration.md#remote-places-on-runtime-mux)).
  See [step 8](#8-set-up-remote-places).
- Each `spec.model` that it removed: set the model in the harness's own
  configuration.
- `[gateway]`, if the report says that Fleet 3 refused it: copy it from
  `config.toml.v2` by hand, and check each key against
  [the configuration reference](fleet/configuration.md).
- `[plugins.<name>]`: see [step 6](#6-reinstall-plugins).

Keep `config.toml.v2` and `store-v1.v2` as the record of your 2.x work.
Fleet 3 never reads or changes them.

## 3. Start your work again as Tasks

`fleet migrate` imported your Fleet 2 Tasks. Now import the thurbox 2.x
sessions and automations. Read the plan first:

```sh
thurbox-cli migrate --dry-run
thurbox-cli migrate
```

`thurbox-cli migrate` reads `thurbox.db` with `sqlite3 -readonly`, and
`agents.toml` and `hosts.toml`. It changes none of them. It applies each
object through `fleet apply` on the Fleet that thurbox uses:

- Each live 2.x session becomes an interactive Task (an empty brief) with
  `spec.dispatch = false`. A worktree becomes
  `git+file://<repository>?branch=<its branch>`, a session with no worktree
  `file://<directory>`. A session on an ssh or WSL host runs on the place
  `mux/<host>`.
- A session whose agent has no Fleet harness profile, such as a 2.x shell
  session, is not imported. A session whose agent gets a printed profile is
  imported. Write that profile before you dispatch the Task.
- A Task cannot carry a 2.x conversation. The agent keeps a conversation
  with the directory it ran in. The report prints the command that resumes
  each one there, for example `cd /src/app-wt && claude --resume <id>`.
- The 2.x worktree keeps its branch checked out. While it exists, the Task's
  worktree gets the branch `<branch>-2`, and uncommitted work stays in the
  2.x worktree. Do these steps in this order:
  1. Resume or finish the 2.x conversation in its 2.x directory.
  2. Commit or push its work.
  3. Remove the 2.x worktree ([step 9](#9-remove-what-2x-left)).
  4. Dispatch the Task.
- Each `cron` automation that starts a session, with its time zone set to
  UTC, becomes a CronTask with `spec.suspend = true`. The report names each
  other automation: one that sends to a session, runs a command, runs once,
  or runs in another time zone. 2.x ran an automation with no time zone in
  the machine's local time. Write a CronTask for each one by hand.
- It removes the [retired keys](thurbox/config.md#retired-keys) from
  `settings.toml`, and keeps the file before as `settings.toml.v2`.
- It prints a `[runtimes.mux.places.<name>]` table for each `hosts.toml` host,
  and a harness profile for each `agents.toml` agent that Fleet has no
  profile for. thurbox does not write Fleet's config: paste each table into
  Fleet's config, and write each profile as the `harnesses/<agent>.toml` file
  it names, beside Fleet's config
  ([step 8](#8-set-up-remote-places), [Agents](thurbox/agents.md)).
  A 2.x agent's `setup` has no harness profile key: run it in a wrapper
  script. The report names each other key that it did not carry over.

A second run imports nothing twice: each object carries the 2.x session's or
automation's id as a label. Without `sqlite3`, it says so and does the rest.
Until a run imports each session and automation that it can import, thurbox
names `thurbox-cli migrate` at startup.

Check each Task with `fleet get task NAME`, then set `spec.dispatch` to
`true` and apply it. For a CronTask, set `spec.suspend` to `false`.

For work that neither command imported, write a Task with the Fleet 3 shape
([Task](fleet/resources/task.md)):

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "Task",
  "metadata": {"name": "fix-login"},
  "spec": {
    "harness": "claude",
    "host": "local",
    "brief": "Fix the login redirect and open a pull request.",
    "workspace": ["git+https://github.com/example/app.git#main"],
    "dispatch": true
  }
}
```

- `spec.workspace` lists URLs. A `git+` URL gets its own worktree, on a
  branch named after the runtime's session key (`<keyPrefix>.<task uid>`
  for `runtime-mux`), under the runtime's folder:
  `~/.local/share/runtime-mux/workspaces/<key>/<repository>/` for
  `runtime-mux`. Add `?branch=NAME` to continue a branch you pushed from 2.x.
- A `git+file://` URL uses a checkout on the place itself. A `file://` URL
  uses a directory as it is.
- `spec.host` names a place, `<runtime>/<place>`. `fleet runtime places`
  lists them.
- Remove `spec.model`.
- Deleting a Task removes its worktree. `workspace-git` keeps the branch,
  and commits the work that exists only in the worktree to
  `refs/fleet/preserved/<key>/<name>`. It prunes those refs after
  `preserve.days`, 30 by default.

In thurbox, `Ctrl+N` writes the Task for you.

## 4. Run one manager per store

`fleet up` holds the store's manager lock and lease. A second `fleet up` on
the same store waits until the first exits ([running `fleet up`](fleet/configuration.md#running-fleet-up)).
While no manager of yours runs, Tasks wait where they stopped, for example
with `WorkspaceReady` Unknown and the reason `Materializing`.

Before you enable a service:

1. Read who holds the lease. `holder` is `fleet-up-<pid>@<boot id>`:

   ```sh
   fleet version --json | jq .manager
   ```

2. List every manager on the machine, and the binary each one runs:

   ```sh
   pgrep -af 'fleet.* up'
   ls -l /proc/PID/exe    # "(deleted)" marks a binary that an install replaced
   tr '\0' '\n' < /proc/PID/environ | grep FLEET_CONFIG
   ```

3. Stop each manager that the service does not run: one you started by
   hand, one from a 2.x install or an old build, one on another config
   directory. Stop it with Ctrl-C or SIGTERM: it then releases the lease
   and keeps the sessions.
4. Start the service. Check that `fleet version --json` shows the service's
   process as the holder.

thurbox never starts `fleet up`. While no manager runs, its status line says
which command starts one.

### The service unit

On Linux, `fleet service install --name UNIT --now` writes the Fleet 3 unit
over a unit that ran Fleet 2, and keeps its drop-ins
([the service unit](fleet/configuration.md#the-service-unit-fleet-service)).
Without `--name`, the unit is `fleet.service`. Then check each drop-in
against the list below. `systemctl --user cat UNIT` shows them all.

If you keep a unit that you wrote yourself, update it and each of its
drop-ins:

- `ExecStart`: the Fleet 3 `fleet`, with `up`. `fleet up --help` lists the
  flags it takes.
- `Environment=PATH`: the directories of `fleet`, `runtime-mux`,
  `workspace-git`, `tmux`, `git` and your agents. A user service does not
  read your login `PATH`.
- `Environment=FLEET_CONFIG` or `--config`: the Fleet 3 config, if it is not
  the per-user one.
- `KillMode=mixed`. `runtime-mux` starts the tmux server in a scope of its
  own, so the server is not in the unit's cgroup. A stop or a restart of
  the unit stops `fleet up` and every process it left, and keeps the
  server and every session on it. A tmux server that started before
  `runtime-mux` used a scope is in the unit's cgroup: the first stop stops
  it, and Fleet starts each session again, as after a host reboot.
- `Description` and other text that names Fleet 2.
- OpenTelemetry variables: Fleet 3 reads the standard `OTEL_*` variables
  ([observability](observability.md)). Fleet sets no `service.namespace`.
  A value that you set in `OTEL_RESOURCE_ATTRIBUTES`, such as a namespace
  that names v2, stays until you change it.

Then reload and restart:

```sh
systemctl --user daemon-reload
systemctl --user restart UNIT
```

To start the manager at boot without a login, see
[running `fleet up`](fleet/configuration.md#running-fleet-up).

## 5. Trust the runtime's folder

Each Task gets a new directory under the runtime's folder. Claude Code asks
once per new directory whether to trust it. Fleet never answers that
dialog: the Task's `WorkspaceTrusted` condition says that the session waits
([workspace trust](fleet/contracts.md#workspace-trust)). Codex and pi get a
launch flag that trusts the workspace.

Kimi Code and GitHub Copilot CLI ask their own folder trust in the same
way. Answer the dialog in the session, and the brief follows. A setting that
trusts the whole runtime folder ahead of time is an open point: see
[Open points](#open-points).

## 6. Reinstall plugins

A Fleet 3 plugin speaks plugin protocol 1. Install the release of each
plugin that supports Fleet 3, as its own documentation says. Then register
it under `[plugins.<name>]` with the Fleet 3 fields: `program` and `cwd`
are absolute paths, and the manifest's `name` must equal the key
([the `[plugins]` section](fleet/configuration.md#plugins-section)).

```sh
fleet list plugins --json
```

Each plugin must show `"available": true`. Restart the manager after you
change the config.

## 7. Clean up `settings.toml`

thurbox 3.x reads `~/.config/thurbox/settings.toml`. It ignores a retired key
and names it at startup with the reason. `thurbox-cli config validate`
passes a retired key and fails any other unknown key. A save from the
settings panel removes the retired keys.

[The retired keys](thurbox/config.md#retired-keys) lists each one with
its reason. `thurbox-cli migrate` removes them
([step 3](#3-start-your-work-again-as-tasks)). Or remove them yourself, or
save once from the settings panel. `thurbox-cli doctor` warns about a
`hosts.toml` that 2.x left, and an `automations.json` that an early 3.x
build left. It names what replaces each one.

## 8. Set up remote places

A 2.x remote machine or WSL distro was a `hosts.toml` entry. In 3.x it is
one of two things ([Where sessions run](where-sessions-run.md)):

- **A place of your Fleet.** Add it under `[runtimes.mux.places.<name>]`,
  with `kind = "ssh"` or `kind = "wsl"`. An ssh place needs
  `ssh -o BatchMode=yes <destination> true` to succeed,
  tmux 3.2 or newer, Fleet 3's `fleet` and your agents on its `PATH`, and
  `workspace-git` and `git` for a `git+` workspace. A WSL place also needs
  `flock`. Its sessions run on its
  `runtime-mux` socket, and its workspaces under its own
  `~/.local/share/runtime-mux/workspaces`. `fleet doctor` names what is
  missing.
- **A Fleet of its own.** Install 3.x there, run `fleet init` and a service
  for `fleet up`, and list it under `[fleets.<name>]` in thurbox's
  `settings.toml`.

A machine that a configuration tool manages, such as NixOS, needs the same
programs at a 3.x release, and the service for a Fleet of its own. See
[Open points](#open-points).

## 9. Remove what 2.x left

Remove each item only after you have checked that it holds no work you keep.

- **2.x sessions.** They run on the `thurbox` tmux socket. thurbox keeps
  that socket under its data directory, so name the directory to tmux.
  Fleet 3 does not see these sessions. List them, then end each one you do
  not need:

  ```sh
  TMUX_TMPDIR=~/.local/share/thurbox/tmux tmux -L thurbox list-sessions
  TMUX_TMPDIR=~/.local/share/thurbox/tmux tmux -L thurbox kill-session -t NAME
  ```

  Do the same on each 2.x remote machine, with the socket its `hosts.toml`
  entry named.
- **The automation heartbeat and timer.** `thurbox-cli doctor` names a
  heartbeat window and an enabled `thurbox-automations.timer` that still run
  `thurbox-cli automation tick`, and prints the command that removes each.
- **2.x worktrees.** 2.x made them under
  `~/.local/share/thurbox/worktrees` by default, on each machine. In each
  repository, `git worktree list` shows them. Push or save their work, then
  run `git worktree remove PATH` and `git worktree prune`.
- **Files 3.x does not read.** `hosts.toml`, `agents.toml`, `hooks.toml`
  and `settings.toml.v2` in `~/.config/thurbox`, and `thurbox.db`,
  `thurbox.db.migrated` and `automation-tick.*` in `~/.local/share/thurbox`.
  Delete `thurbox.db` only after `thurbox-cli migrate` has read it.
- **Hooks in agents' own configs.** 2.x installed status hooks into the
  config directory of some agents. A hook that runs a `thurbox-cli` command
  that 3.x does not have only fails. Remove it. Fleet 3 sets each harness's
  hooks at launch, except Kimi Code's: `fleet integration install kimi`.
- **Old binaries and old builds.** A second copy of `fleet` or `thurbox` in
  another directory, and a manager that runs it ([step 4](#4-run-one-manager-per-store)).
- **The Fleet 2 config and store.** Keep `config.toml.v2`, `store-v1.v2`
  and your saved records while you need the history. Delete them after
  that.

## Check

```sh
fleet doctor
fleet version --json | jq .manager   # held: true, by your service
fleet list task
thurbox-cli doctor
```

`thurbox-cli doctor` reports each Fleet, its runtime and its agents. It
also reports a 2.x `hosts.toml`, an early 3.x `automations.json`, the
automation heartbeat window and the automation timer. It does not check the
other 2.x files.

## Open points

This page does not cover these points yet:

- **Trust the runtime folder ahead of time.** No documented setting makes
  Claude Code trust every directory under the runtime's folder. Until one
  is recorded, answer the dialog once per Task.
- **Plugins.** Each plugin documents its own Fleet 3 install. This page does
  not name a plugin's version.
- **OpenTelemetry after the cutover.** [Observability](observability.md)
  owns the variables. This page names no check of a migrated install's
  export.
- **A remote place under NixOS.** The flake's NixOS and Home Manager
  modules (`programs.thurbox`) install `thurbox` and `thurbox-cli` only.
  They install no `fleet` or `workspace-git`, and no service for
  `fleet up`. Provide those another way.
- **The 2.x update notice.** A 2.x `thurbox-cli version --check` links the
  `v2.x` branch's release notes, not this page.
