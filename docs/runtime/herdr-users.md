# Fleet for herdr users

This guide is for a person who uses herdr: herdr-core (`hc`) and herdr's
desktop app. It sets up Fleet to run coding agents as Tasks in herdr-core
terminals, so the agents show in herdr's app. thurbox is optional.
How the runtime works inside is [runtime-herdr](herdr.md).

![herdr's app while a request is typed into VEGA and Fleet runs the three worker Tasks it applies, each in a space of its own: backend, frontend and docs](https://github.com/Thurbeen/thurbox/releases/download/media/herdr-fleet-vega.gif)

A request is typed into VEGA. VEGA applies one worker Task per repository,
and each one appears in herdr's app as a space of its own, `backend`,
`frontend` and `docs`, when Fleet starts it. The
[video](https://github.com/Thurbeen/thurbox/releases/download/media/herdr-fleet-vega.mp4)
is the same recording, sharper.

## What Fleet adds to herdr

herdr-core keeps terminals alive and herdr's app shows them. Fleet decides
what runs in them:

| You get | From |
| --- | --- |
| Terminals that outlive the app and a core restart | herdr-core |
| A space in the sidebar for each Task, with its agent and its shell | herdr's app |
| One agent session per Task, started from a JSON file or from thurbox | Fleet |
| A git worktree per Task, made before the agent starts and removed when you delete the Task | Fleet, through `runtime-herdr` |
| A brief as the agent's first prompt, messages later, a recorded result | Fleet |
| A session revived on its conversation when its process ends | Fleet |
| A Task's pull request followed, and marked ready when a policy you choose holds | Fleet's forge controller |

```text
fleet apply ─▶ Fleet ─▶ runtime-herdr ─▶ herdr-core
                                             │ runs
                                             ▼
          herdr's app ◀── labels ─── a terminal per Task
```

`runtime-herdr` is the bridge. It starts each Task's agent in a herdr-core
terminal with the labels herdr's app reads. Fleet's other runtime,
`runtime-mux`, uses tmux instead. You can keep both.

## Quick setup

On Linux or macOS, these commands install herdr-core and Fleet, whose
installer ships `runtime-herdr`, and make herdr Fleet's default runtime.
The steps below explain each one.

```bash
curl -fsSL https://releases.herdr.dev/core/install.sh | sh
curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh -s -- --fleet-only
export PATH="$HOME/.local/bin:$PATH"
fleet init

cat >> "${XDG_CONFIG_HOME:-$HOME/.config}/fleet/config.toml" <<'EOF'

[runtimes.herdr]
program = "runtime-herdr"
default = true

[runtimes.herdr.places.local]
EOF

fleet runtime places
fleet doctor
```

Both installers put their programs in `~/.local/bin` and do not change the
shell's `PATH`, so the `export` lets this shell find `fleet` and `hc`. Add
the same line to your shell's profile.

`local` then names `herdr/local`, and `mux/local` stays available. On a
Fleet that already runs Tasks on `local`, keep `runtime-mux` the default
instead ([step 2](#herdr-beside-runtime-mux)).

On Linux with a systemd user session, the Fleet installer runs `fleet init`
and starts `fleet up` as a user service, which read the config before the
herdr lines were there. Run `fleet service restart`. Otherwise, start
`fleet up` ([step 3](#3-check-it-and-restart-fleet-up)).

### Or ask an agent

Send this prompt to a coding agent on the machine:

```text
Set up Fleet to run my coding agents as Tasks in herdr, following
https://thurbox.thurbeen.eu/docs/runtime/herdr-users.html.
Install herdr-core if it is missing, and install or reinstall Fleet with its
installer, which ships runtime-herdr. Add a [runtimes.herdr] runtime with a local place to my Fleet
config. If Fleet already runs Tasks on another runtime, keep that runtime
and its default. Check the result with `fleet runtime places`,
`fleet doctor` and `fleet runtime check -- runtime-herdr`, then restart
`fleet up` (or `fleet service restart`). Do not delete or change any
existing Task. Tell me what you changed and what each check printed.
```

## Prerequisites

- Linux or macOS. `runtime-herdr` does not run on Windows yet.

- herdr-core 0.1.22 or newer. CI tests with the version that
  `nix/herdr-core.nix` pins, 0.1.22, which is also herdr-core's latest
  stable release. Check yours with `hc --version`. To install or update it:

  ```bash
  curl -fsSL https://releases.herdr.dev/core/install.sh | sh
  hc update    # later, to update it
  ```

- herdr's desktop app, to see the Tasks. The herdr 1 terminal interface (the
  `herdr` program) does not share herdr-core's terminals, so it does not
  show them.

- A coding-agent CLI, installed and logged in.

## 1. Install Fleet

Fleet's installer ships `runtime-herdr` beside `fleet`, `runtime-mux` and
`workspace-git`:

```bash
curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh -s -- --fleet-only
fleet init
command -v runtime-herdr
```

Without `--fleet-only`, the installer also installs thurbox. A Fleet
installed before `runtime-herdr` shipped does not have it: run the installer
again. [Install Fleet](https://thurbox.thurbeen.eu/docs/fleet-install.html)
covers the service and a source build.

## 2. Add herdr to Fleet's config

Fleet's config is `~/.config/fleet/config.toml` (`fleet init` wrote it). Add
one of the two shapes below.

### herdr as the only runtime

Replace the `[runtimes.mux]` table with the lines below. Keep it while a
Task still runs on `mux/local`: Fleet reaches each Task through its
runtime.

```toml
[runtimes.herdr]
program = "runtime-herdr"

[runtimes.herdr.places.local]
```

`local` then names `herdr/local`, because herdr is the only runtime.

### herdr beside runtime-mux

Keep `[runtimes.mux]` and add `[runtimes.herdr]`. With two runtimes, mark
exactly one of them `default = true`. Fleet refuses a config with several
runtimes and no single default.

```toml
[runtimes.mux]
program = "runtime-mux"
default = true

[runtimes.mux.places.local]

[runtimes.herdr]
program = "runtime-herdr"

[runtimes.herdr.places.local]
```

Here `local` still names `mux/local`. A Task runs in herdr when it names
`herdr/local`. Move `default = true` to `[runtimes.herdr]` to make
`local` name `herdr/local`.

### A place's keys

A herdr place takes two keys: `session`, the herdr-core session
(`HC_SESSION`), and `space`. Without `space`, each Task is a space of its
own in herdr's app. With it, every Task of the place is a tab in that one
space.

```toml
# spec.host = "herdr/work": the herdr-core session "work", app space "agents"
[runtimes.herdr.places.work]
session = "work"
space = "agents"
```

Their defaults and rules are in [Fleet's
configuration](../fleet/configuration.md#places-on-runtime-herdr).

## 3. Check it and restart fleet up

```bash
fleet runtime places    # herdr/local, and whether it is ready
fleet doctor            # each runtime's places, each agent's program
```

A place is ready when its herdr-core answers. `runtime-herdr` starts the core
when it does not run.

`fleet up` reads the config only when it starts. Restart it after each change
to the config:

```bash
fleet service restart   # when `fleet service install` runs it
```

Or press `Ctrl+C` in the terminal that runs `fleet up`, and start it again.

A service does not read your login `PATH`. `fleet service install` records
the `PATH` of the shell it runs in, after the directory of `fleet` itself,
which holds `runtime-herdr`. If the directory of `herdr-core` was not on it,
run `fleet service install` again from a shell whose `PATH` has it.

## 4. Run a first Task in herdr

### From the CLI

A Task is a JSON file. `spec.host` names the place:

```json
{
  "apiVersion": "fleet/v1alpha1",
  "kind": "Task",
  "metadata": {"name": "fix-login"},
  "spec": {
    "harness": "claude",
    "host": "herdr/local",
    "brief": "Fix the login redirect and run the tests.",
    "workspace": ["git+file:///home/me/app"],
    "dispatch": true
  }
}
```

```bash
fleet apply -f fix-login.json
fleet list task          # its phase
fleet watch --json       # one event per change, until Ctrl+C
```

With herdr as the default runtime, `"host": "local"` and no `host` at all
mean the same place. `fleet version --json` lists the harnesses you can
name.

### From thurbox

Press `Ctrl+N`. The Place step lists each place of the Fleet. Pick
`this machine (herdr)`, which is `herdr/local`. The next steps offer this
machine's repositories, as on `mux/local`. A place that is not ready is
dimmed, and its reason shows under the list.

## 5. Find it in herdr

The runtime knows a Task by its key, not by its name. `fleet status task`
prints the key on its `session` line:

```text
$ fleet status task fix-login
...
session fleet.u5-e5fb0263df286e78 on herdr/local Running
```

In herdr's app, the Task is a space of its own, named after the folder it
works in, such as the repository's name, with the branch of its worktree
beside it. Its agent and its `fleet shell` are inside. On a place that
sets `space`, the Task is a tab in that space, named after the key.

With `hc`, select the Task's terminals by label. `hc ls` does not print
labels:

```bash
hc ls --label fleet.place=local       # every Task's terminal at the place local
hc ls --label fleet.key=<key>         # one Task's terminals
hc get <terminal> --json              # one terminal, with its labels
```

The labels are `fleet.key` and `herdr.tab` (the key), `fleet.place` (the
place's name), `herdr.space` (the key, or the place's `space`), and `fleet.role=shell` on
the shell that `fleet shell` opens ([runtime-herdr](herdr.md#how-it-reaches-herdr-core)).

For a place with a `session`, set `HC_SESSION` to that session before you
run `hc`.

## 6. Work with the Task

| To | Run |
| --- | --- |
| Type to the agent | Its tab in herdr's app, `hc attach <terminal>`, or thurbox |
| Open a shell in its directory | `fleet shell <task>` |
| Send it a message | `fleet send <task> "Run the tests too"` |
| Stop its process and keep the conversation | `fleet pause <task>` |
| Start it again | `fleet attach --wake <task>`, or `fleet send` |
| Stop it for good and remove its worktree | `fleet delete task <task>` |

- `fleet attach` and `fleet shell` print the command that opens the
  terminal as a [terminal stream](README.md#the-terminal-stream), one
  argument per line: `runtime-herdr view <terminal>`. thurbox runs it for
  you. In a shell, use `hc attach` instead.
- The shell is a second herdr-core terminal, in the Task's tab, labelled
  `fleet.role=shell`. A second `fleet shell` opens the same one.
- Fleet pauses on its next pass, once the agent has reported its
  conversation. The pause ends the Task's terminals. The Task stays, and
  its next start resumes the same conversation. Fleet also pauses an idle
  Task by itself after its [TaskConfig](../fleet/resources/task-config.md)'s
  `pauseAfterMs`. Either way, the Task leaves herdr's sidebar until it
  starts again: [Fleet Lens](#see-every-task-fleet-lens) still lists it.
- Fleet has no separate stop. `fleet delete task` waits until the agent is
  at rest, at most 5 minutes. Then it ends the Task's terminals, the shell
  among them, removes the worktree, and removes the Task.
- A terminal that you end in herdr's app, or with `hc kill`, reads as a lost
  session. Fleet starts the Task again in a new terminal. To stop a Task,
  use Fleet.
- A herdr-core restart or update keeps the Task's process running.

### See every Task: Fleet Lens

[Fleet Lens](https://github.com/Thurbeen/herdr-fleetlens) is a mod for
herdr's app. It shows Fleet itself, not only the terminals Fleet runs:

- **The Lens.** A float that browses every Fleet kind, plugin kinds included.
  Tasks with an open Question or a problem come first, and the selected Task's
  Questions, result and pull requests show in full.
- **The Fleet Sessions sidebar.** It replaces herdr's agents list. It lists
  every Task's session, paused ones included, and a click wakes a paused Task
  (`fleet attach --wake`).

![Fleet Lens: a request typed into VEGA becomes three worker Tasks in herdr, the Lens shows the one asking a Question first and the shipped ones with their result under VEGA, Fleet pauses the idle sessions, and w in the Lens wakes one](https://github.com/Thurbeen/herdr-fleetlens/releases/download/media/fleet-lens.gif)

Install it into herdr's app, then switch to its layout:

```bash
git clone https://github.com/Thurbeen/herdr-fleetlens
cd herdr-fleetlens
./install.sh --home "${HERDR_APP_HOME:-$HOME/.config/herdr-app}" --store <data-dir>
herdr-app do layout.switch --name fleet
```

`<data-dir>` is the `dir` under `[data]` in Fleet's `config.toml`. With it,
the mod follows the store's changes. Without `--store`, it polls Fleet every
5 seconds. Its [README](https://github.com/Thurbeen/herdr-fleetlens#readme)
covers the keys and what each view shows.

## 7. Run VEGA in herdr

[VEGA](https://github.com/Thurbeen/vega) is a mission-control Task. You send
it a request. It applies one worker Task per piece of work, follows them, and
reports their results. Fleet runs VEGA and its workers like any other Task,
so they can run in herdr too.

Install VEGA from its repository. It reads its instructions from that
checkout, so the clone stays:

```bash
git clone https://github.com/Thurbeen/vega
cd vega
./install.sh
```

VEGA's Task names no place, and it gives its workers none unless you ask:

- With herdr as the default runtime, VEGA runs on `herdr/local`. In herdr's
  app, VEGA is a space, and each worker it applies appears as a space of its
  own, named after the repository it works in.
- With `runtime-mux` as the default, VEGA runs in tmux. Ask it for herdr in
  your request, such as "run the workers on `herdr/local`", and the workers
  run in herdr.

Talk to VEGA with `fleet send vega "..."`, or type in its tab. Each tab is
named after its Task's key: `fleet status task vega` prints VEGA's.

## Limits

- Linux and macOS only. On another system, each herdr place is not ready.
- A herdr place is a herdr-core session on Fleet's own machine. herdr-core's
  remote machines (`hc machine`) are not places. For a remote machine, use
  `runtime-mux`'s ssh places.
- herdr-core waits while a person has an unfinished draft in the terminal,
  up to its `draft_wait` (60 s by default). Fleet then holds the message
  until the agent's hooks report its state.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| Fleet refuses the config | Several runtimes and no `default = true`, or two defaults. |
| `herdr/local` is not ready | `fleet doctor` and `fleet runtime places` print the reason, such as `herdr-core is not installed`. |
| The place works in your shell but not under the service | The service's `PATH`: see [step 3](#3-check-it-and-restart-fleet-up). |
| A new place or runtime is not used | `fleet up` still runs the old config: restart it. |
| The Task is not in herdr's app | `hc ls --label fleet.key=<key>` shows whether the terminal exists, and `hc get <terminal> --json` its labels. Look for a space named after the Task's folder, or in the place's `space`. |
| The runtime itself misbehaves | `fleet runtime check -- runtime-herdr` runs the runtime protocol's conformance suite against it. |
