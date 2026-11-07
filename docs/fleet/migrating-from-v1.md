# Migrating from Fleet v1 to v2

Fleet v1 is the frozen Python release `v1.0.0`, the last 1.x tag. Fleet v2 is
the Rust binary, released from `v2.0.0` onwards. This page maps v1 concepts,
files and commands to v2.

V2 does not read v1 queue records or v1 configuration. You recreate your
configuration and your work in v2. The [README](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/README.md) owns
installation and the
[deliberate cutover procedure](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/README.md#data-and-deliberate-cutover).
This page tells you what to recreate, and how.

## Who needs to migrate

- You run a v1 checkout with `uv run fleet ...`, Mission Control and the
  Kanban board, and you want v2.
- You do not have to migrate. A v1 checkout pinned to `v1.0.0` keeps working.
  Its guard reads the root `FLEET_MAJOR` file and refuses to move onto v2. See
  [Frozen Python v1.0.0](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/README.md#frozen-python-v100).
- A new user has nothing to migrate. Start at the README's
  [Setup](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/README.md#setup).

## What changed

V1 is a control-plane checkout. It keeps a queue of topics and tasks as files,
and a reconciler loop and a lead session act on them. V2 is one installed
binary. It keeps typed resources in a versioned store, and controllers act on
them under a manager lease.

```text
v1: Python checkout                       v2: fleet binary
------------------                        ----------------
orchestration/queue/                      resource store (data.dir)
  <topic>/<NN-task>/                        Task, TaskConfig, EnvProvider,
    task.yaml, BRIEF.md,  -- recreate -->   ChangeRequest, Conversation,
    result.md                 by hand       Listener
        ^                                       ^
        |                                       |
uv run fleet reconcile                    fleet up (manager lease)
  watch, collect, shepherd, refuel          Task, ChangeRequest and
Mission Control (thurbox extension)         Forge controllers
                                          Task/vega (VEGA workspace)
```

| v1 concept | v2 equivalent |
| --- | --- |
| The control-plane checkout, run with `uv run fleet` | The `fleet` binary on PATH. |
| The queue directory, `orchestration/queue/` | The resource store, `data.dir`. |
| A topic and its `PROMPT.md` | None. Group Tasks with `metadata.labels` if you want. |
| A task: `task.yaml` and `BRIEF.md` | A [Task](resources/task.md): `spec.brief` and the other spec fields. |
| `result.md`, read by `collect` | `status.result`, written by `fleet result`. |
| `blocked_by` and `plan` | None. Keep `spec.dispatch` false until the Task may start. |
| The reconciler loop | `fleet up`, the manager. |
| Mission Control and the conductor | `Task/vega`, which `fleet up` starts from the VEGA workspace. |
| `--profile` and `session-profiles.yaml` | A [TaskConfig](resources/task-config.md) and [EnvProviders](resources/env-provider.md). |
| `--repo`, `--base`, `--branch`, `--add-repo` | `spec.workspace.repositories`, mapped by `runtime.repositories`. |
| Publish methods and `collect` verification | A worker claim. `fleet declare --change-request URL` makes a [ChangeRequest](resources/change-request.md) that the Forge controller follows. |
| `shepherd` and its auto-merge | None. Fleet v2 never merges. It can take a draft out of draft under a ready policy. |
| `owners.conf` origins and relays | The Gateway: [Conversation](resources/conversation.md) and [Listener](resources/listener.md) mirrors, with one Task created per admitted thread. |
| The Kanban board and the queue pane | None. Use `fleet list` and `fleet status`. |

### Workspaces

A v1 task names a repository path, a base branch and a branch. A v2 Task
names repositories by URL. The thurbox runtime creates the worktrees and
chooses the branch `<keyPrefix>/<session key>`.

- Use `{"repositories": [{"name", "url", "ref"}]}` for work on repositories.
  The first entry is the primary repository. Put more entries for v1's
  `--add-repo`.
- Map each URL to its local source checkout in
  [`[runtime.repositories]`](configuration.md#fields).
- Use `{"dir": PATH}` for a directory that exists before the Task. V2 has no
  equivalent of v1's `--add-dir`.
- `runtime.workspaceCleanup` is the closest equivalent of v1's `ON_LANDED`
  and `ON_CLOSED`. It applies when a Task is deleted, not when its change
  merges. `delete` removes the worktrees with the session. `keep` keeps them.

See [Workspaces](resources/task.md#workspaces) for the rules.

## Configuration

V1 reads `KEY=value` files in `orchestration/` and `registry/` inside the
checkout. V2 reads one TOML file. The selection order is `--config PATH`,
then `FLEET_CONFIG`, then `$XDG_CONFIG_HOME/fleet/config.toml`, falling back
to `~/.config/fleet/config.toml`. `fleet init` writes that file. V2 refuses
unknown fields, so you cannot copy v1 keys into it.
[Native Fleet TOML](configuration.md#native-fleet-configuration-fleettoml) owns every field.

| v1 file and key | v2 equivalent |
| --- | --- |
| `fleet.conf` `NAME` | `runtime.keyPrefix`; `fleet init` generates a unique one. Use a separate `--config` for a second instance. |
| `fleet.conf` `CONDUCTOR` | None. |
| `agent.conf` `AGENT` | `spec.harness` on a TaskConfig or a Task. |
| `agent.conf` `CONDUCTOR_AGENT`, `FUEL_PROVIDER` | None. |
| `agent-policy.conf`, `flow.conf`, `peers.conf` | None. |
| `auto-merge.conf` | None. `[forge] draftPolicy` and `[forge.readyPolicies]` only mark a draft ready. |
| `publish.conf` `METHOD` | None. Write the publishing instructions in the brief. |
| `reconcile.conf` `*_SECS` | `fleet up --poll-ms`. Forge timings are in `[forge]`. |
| `reconcile.conf` `ON_LANDED`, `ON_CLOSED` | `runtime.workspaceCleanup`. |
| `owners.conf` | `[gateway.routes]` and `[gateway.people]`, for chat threads. |
| `session-profiles.yaml` | A TaskConfig for harness, host and model, with EnvProviders for its `env`. |
| `session-glyphs.conf`, `telemetry.conf` | None. |
| `voice.conf`, `queue/OPERATOR.md` | Edit `AGENTS.md` and the prompts in the VEGA workspace. |
| `registry/owners.txt`, `registry/repos.generated.yaml` | `[runtime.repositories]`, written by hand. |
| `registry/context/*.md` | None. Put the facts in the brief or in the repository's own agent instructions. |
| `gh` or `glab` login | `[forge.hosts]` names each forge and its CLI. The credentials stay with that CLI. |

This fragment maps one repository and keeps worktrees after release:

```toml
[runtime]
kind = "thurbox"
keyPrefix = "fleet-v2"
workspaceCleanup = "keep"

[runtime.repositories]
"https://github.com/example/app" = "/srv/src/app"
```

## Commands

V1 commands run from the checkout as `uv run fleet GROUP ...`. V2 commands
run as `fleet COMMAND ...`. Run `fleet COMMAND --help` for every flag.

| v1 command | v2 equivalent |
| --- | --- |
| `fleet install`, `install.sh`, `install.ps1` | The release installer, then `fleet init`. |
| `fleet preflight` | None. `fleet up` reports unavailable prerequisites. |
| `fleet sync-checkout` | Run the installer again, then `fleet init --update`. |
| `fleet queue topic add` | None. |
| `fleet queue add` | Write a Task manifest, then `fleet apply -f FILE`. |
| `fleet queue block`, `fleet queue plan` | None. Apply the Task with `"dispatch": false`. |
| `fleet queue dispatch` | Apply the Task again with `"dispatch": true`. `fleet up` spawns it. |
| `fleet queue prompt` | None. The Task controller sends the brief once. |
| `fleet queue send` | `fleet send TASK TEXT`. |
| `fleet queue watch`, `fleet queue collect` | `fleet up`, or `fleet reconcile` while `up` is stopped. |
| `fleet queue declare` (`paused`, `needs-input`) | `fleet declare TASK --json OBJECT` records a fact. Fleet does not act on these two states. |
| Writing `result.md` | `fleet result TASK --generation N --outcome OUTCOME --summary TEXT`. |
| `fleet queue list` | `fleet list task`. |
| `fleet queue show` | `fleet get task NAME` and `fleet status task NAME`. |
| `fleet queue prs` | `fleet list changerequest`. |
| `fleet queue reap`, `fleet queue abandon`, `fleet queue reviewed` | `fleet delete task NAME`. The controller releases the session first. |
| `fleet queue shepherd`, `fleet queue refuel`, `fleet queue review record` | None. |
| `fleet queue attach`, `run`, `archive`, `unarchive`, `find`, `check`, `root` | None. |
| `fleet status` | `fleet list KIND` and `fleet status KIND NAME`. |
| `fleet reconcile start`, `serve`, `ensure` | `fleet up`, in the foreground or under your own supervisor. |
| `fleet reconcile stop` | Ctrl-C on `fleet up`. |
| `fleet install-extension`, `place-pane`, `pane-ask` | None. V2 installs no thurbox extension. |
| `fleet discover-owners`, `add-owner`, `sync-registry` | None. Edit `[runtime.repositories]`. |
| `fleet context`, `telemetry`, `peers`, `watch`, `voice-ask` | None. |
| `fleet sessions`, `session-flags`, `session-name`, `session-trust`, `trust-thurbox-dir`, `paths` | None. |
| `fleet check` | None in the binary. Contributors run `cargo xtask check`. |

## Recreating your work

V2 cannot import v1 data. Recreate each v1 task that must continue as a Task
manifest. A v1 task like this one:

```text
uv run fleet queue add report-status drop-idle-default \
  --repo /srv/src/app --branch drop-idle-default --agent codex
```

becomes a TaskConfig and a Task:

```json
[
  {
    "apiVersion": "fleet/v1alpha1",
    "kind": "TaskConfig",
    "metadata": { "name": "worker" },
    "spec": { "harness": "codex", "host": "local" }
  },
  {
    "apiVersion": "fleet/v1alpha1",
    "kind": "Task",
    "metadata": {
      "name": "drop-idle-default",
      "labels": { "topic": "report-status" }
    },
    "spec": {
      "configRef": { "name": "worker" },
      "workspace": { "repositories": [
        { "name": "app", "url": "https://github.com/example/app", "ref": "main" }
      ] },
      "brief": "Drop the idle default. Open a draft pull request and declare it.",
      "dispatch": true
    }
  }
]
```

Copy the v1 `BRIEF.md` text into `spec.brief`. Tell the worker how to publish
and how to report, because v2 has no publish method. The worker reports with
`fleet result`, and declares a pull request with
`fleet declare TASK --change-request URL`.

Rehearse the manifests offline first. The fake runtime runs no agent. Save
this as `fleet.toml` in a new directory:

```toml
version = 1

[data]
dir = "data"

[runtime]
kind = "fake"
dir = "fake-host"
```

Then apply the manifests, saved as `tasks.json`, and drive one pass:

```sh
fleet --config fleet.toml apply -f tasks.json
fleet --config fleet.toml reconcile
fleet --config fleet.toml list task
fleet --config fleet.toml status task drop-idle-default
```

The fake runtime does not check `runtime.repositories`. Check the mapping
against your real config before `fleet up`.

## Upgrade steps

Follow the README's
[deliberate cutover](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/README.md#data-and-deliberate-cutover). For a v1
operator, its steps are:

1. In the v1 checkout, finish or explicitly keep every task. Inspect them with
   `uv run fleet queue list`.
2. Stop the v1 reconciler with `uv run fleet reconcile stop`. Keep the
   checkout, its queue, its configuration and its thurbox extension for
   rollback.
3. Install v2 with the README's [Setup](https://github.com/Thurbeen/fleet/blob/961da39903338f3fb0493ec5a34aa8417cb98d89/README.md#setup) command. Run
   `fleet version --json`, then `fleet init`.
4. Edit the generated config: `[runtime.repositories]`, `[forge.hosts]`, and
   `[runtime.modelEnv]` if your Tasks set a model.
5. Edit the VEGA workspace and its `task.json` before the first `fleet up`.
   See [Initialization and the VEGA starter](configuration.md).
6. Rehearse your recreated manifests with the fake runtime.
7. Start `fleet up` in its own terminal. Apply the manifests.

Never point v1 and v2 at the same work. Each v2 store is fresh: a nonempty
directory without the v2 store marker is refused.

## Mission Control and VEGA

V1's lead session is Mission Control, which the v1 thurbox extension
creates. V2's lead is `Task/vega`, an ordinary Task. `fleet up` starts it
from the VEGA workspace that `fleet init` writes.

- The VEGA starter uses the `codex` harness by default. Choose another with
  `fleet init --harness NAME`, then install and log in to it.
- VEGA's skills are `task-intake`, `task-status` and `status-digest`. They
  create, inspect and summarize v2 Tasks. V1's skills, such as
  `fleet-intake` and `fleet-queue`, use v1 commands and do not work with v2.
- Run `fleet init --update` after each binary update. It keeps your edits and
  writes conflicting bundled files under `vega/.fleet/conflicts/`.

## Breaking changes

- The command is `fleet`, not `uv run fleet`. V2 needs no Python, uv or
  checkout.
- V2 releases Linux x86_64 and aarch64 binaries, and an experimental macOS
  Apple Silicon binary. V1 also supported Windows. V2 has no Windows or
  Intel Mac release.
- The configuration, resource and store formats are new. There is no
  importer.
- V2 has no topics, no task dependencies, no auto-merge and no artifact
  verification. A result is the worker's claim.
- V2 installs no thurbox extension, Kanban board or queue pane.
- V2 passes a session only the environment that its Task, TaskConfig and
  EnvProviders resolve, and the model variable. Credentials stay with the
  agent and its host.

## Rollback

1. Stop `fleet up` with Ctrl-C. The sessions stay.
2. Keep the v2 store for diagnosis. A binary rollback does not downgrade a
   store schema.
3. In the v1 checkout, run `uv run fleet reconcile start`.
4. To remove the v2 binary, run `sh install-release.sh --uninstall`.
   Uninstall removes the binary only.
