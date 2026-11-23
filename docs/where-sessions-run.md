# Where sessions run

Two choices decide where a session runs:

1. **The Fleet**: which Fleet manages the session.
2. **The place**: where that Fleet's runtime runs the session's process.

The two are separate. A Fleet can run on another machine, and its runtime
can run a session on yet another machine.

```text
                    laptop
  ┌────────────────────────────────────────┐
  │ thurbox: one list of sessions          │
  └──────┬──────────────────────┬──────────┘
         │ runs fleet           │ runs fleet over ssh or wsl.exe
         ▼                      ▼
  Fleet "laptop"          Fleet "box"  (on box; fleet service's user unit runs fleet up)
         │                      │
         ▼                      ├──────────────────┐
  runtime mux             runtime mux          runtime vm
         │                 │          │            │
         ▼                 ▼          ▼            ▼
  place local       place local  place wsl-dev  place sandbox
  (the laptop)      (box)        (WSL on box)   (dynamic: a new VM per session)
```

## Terms

| Term | Meaning |
| --- | --- |
| Fleet | The engine that manages sessions: one Fleet installation and its store. thurbox reaches it by running `fleet`, on this machine or over ssh or WSL. |
| runtime | A program that a Fleet runs to start and stop processes, such as `runtime-mux`. A Fleet can have several runtimes. |
| place | Where a runtime runs a process: a machine, a WSL distro or a VM. Its name is `<runtime>/<place>`, such as `mux/wsl-dev`. |
| `local` | The place on the Fleet's own machine. For Fleet "box", `local` is box, not the laptop. |
| dynamic place | A place that makes a new machine, such as a VM, for each session. |

Older pages and Fleet's `spec.host` field use "host" for a place.

## Today and planned

| Part | Status |
| --- | --- |
| The runtime protocol carries a place, and `manifest` reports places | implemented |
| Places in Fleet's config, `[runtimes.<name>.places.<place>]`, named `<runtime>/<place>` | implemented |
| `runtime-mux` runs sessions on `local`, on an ssh place and in a WSL distro: start, stop, input, screen and state | implemented |
| Several Fleets in thurbox's `settings.toml`, `[fleets.<name>]`, over ssh or WSL; `thurbox-cli doctor` checks each | implemented |
| The session list follows every listed Fleet; a row's id is `<fleet>:<task>`, and an unreachable Fleet's rows stay, marked unreachable | implemented |
| Send, rename and delete go to the session's own Fleet | implemented |
| A remote Fleet's session panes: `fleet attach` and its stream over the link | implemented |
| The new-session modal's Fleet step, then the chosen Fleet's places and agents; creation goes to that Fleet | implemented |
| The modal browses a remote Fleet's folders and lists a repository's branches there, through Fleet | implemented |
| A Fleet whose `fleet version --json` lacks `fleet/v1alpha1` is refused: no watch, no command | implemented |
| The runtime protocol's `shell` verb opens a shell beside a session on its place; `runtime-mux` answers it | implemented |
| `fleet shell <task>` names the stream that opens that shell | implemented |
| Every session's companion shell opens through `fleet shell` on its Fleet, at the session's place, and the runtime keeps it | implemented |
| thurbox keeps no file of machines of its own: `hosts.toml` is retired | implemented |
| On a POSIX ssh place or a WSL place: a workspace cloned there, `browse`, `branches`, and the session's pane and shell | implemented |
| The new-session modal browses folders and branches on a POSIX ssh place or a WSL place, of any Fleet; on a native-Windows place it takes a typed path | implemented |
| A VM runtime | planned |

## What thurbox does on another machine

thurbox reaches another machine only through a Fleet. It keeps no file of
machines of its own. For a session that runs on another machine:

- The agent's pane is the stream `fleet attach` names, and the companion
  shell (`Ctrl+T`) is the stream `fleet shell` names. Both run over the
  session's Fleet link.
- The new-session modal reads the folders and branches of a place on another
  machine through its Fleet (`fleet runtime browse`, `fleet runtime
  branches`). That place is any place of a remote Fleet, or a place of this
  machine's Fleet other than `local`. A native-Windows place has no `sh`, so
  its runtime refuses `browse` and `branches`, and the modal takes a typed
  path there.
- Diffs, git stats and plugin runs refuse the session. They read this
  machine's disk (`SessionRow::workspace_reachable`).
- An image dropped onto the session is refused with a message: the path
  names a file on this machine only.

## Common setups

### 1. Laptop only

Status: implemented.

1. Run the installer, `scripts/install.sh` (`install.ps1` on Windows). It
   installs thurbox, `fleet` and `runtime-mux` together. Also install a
   multiplexer and a coding agent.
2. Run `fleet init`. It writes the multiplexer runtime and its `local`
   place.
3. Run `fleet service install --now`. It writes the user service that
   runs `fleet up`, and starts it: a systemd user unit on Linux, a launchd
   agent on macOS, a Task Scheduler task on Windows
   ([the service unit](fleet/configuration.md#the-service-unit-fleet-service)).
   On Linux, the installer does steps 2 and 3 when a systemd user manager
   runs.
4. Start thurbox. thurbox never starts, restarts or sets up a manager. While
   none runs, its status line says `Fleet not running · systemctl --user
   start fleet.service`, with the unit that `fleet version --json` names.
   With no unit, it says `Fleet not running · fleet service install --now`.
   Before step 2, it says `no Fleet config · fleet init`.

Each session runs on `local`: the laptop.

### 2. Laptop thurbox, Fleet on a remote box

Status: implemented. thurbox lists box's Fleet and follows its sessions over
ssh beside the laptop's, `thurbox-cli doctor` checks it, the new-session
modal creates on it, each command for one of box's sessions goes to box,
and a session's pane shows and takes typing over the same ssh. The modal
browses box's folders and lists a repository's branches on box, through
box's Fleet.

This is the most common setup. The agents run on box, so they keep working
when the laptop sleeps.

1. On box, install `fleet`, `runtime-mux`, `tmux` and a coding agent.
2. On box, run `fleet init`.
3. On box, run `fleet service install --now` from a login shell. It writes
   the user systemd unit that runs `fleet up`, and starts it
   ([the service unit](fleet/configuration.md#the-service-unit-fleet-service)).
   thurbox never starts `fleet up`, on any Fleet. A user service does not
   read your login `PATH`, so the unit takes the `PATH` of the shell that
   installs it. Put more directories in a drop-in:
   `systemctl --user edit fleet.service`. The unit has
   `KillMode=mixed`: a stop or a restart of the unit stops `fleet up` and
   every process it left. `runtime-mux` starts the tmux server that holds
   the sessions in a scope of its own, so the unit keeps that server.

   ```sh
   fleet service install --now
   loginctl enable-linger "$USER"   # start it at boot, without a login
   ```

4. On the laptop, list both Fleets in thurbox's `settings.toml`. When the
   file lists a Fleet, thurbox uses only the listed Fleets.

   ```toml
   [fleets.laptop]
   transport = "local"

   [fleets.box]
   transport = { ssh = "me@box" }
   program = "/home/me/.local/bin/fleet"   # ssh does not read your login PATH
   ```

5. Run `thurbox-cli doctor`. It reports each Fleet, and says when box cannot
   be reached. It reads box's `fleet version --json` to report whether box's
   manager runs, and names the command that starts it when it does not. The session list
   follows every listed Fleet, each command goes to its session's own Fleet,
   and the new-session modal asks which Fleet a new session goes to.

### 3. One Fleet, several places

Status: `runtime-mux` runs sessions on an ssh place and in a WSL distro. It
can clone repositories on the place. The session's pane and shell open
there. No VM runtime ships yet.

One Fleet runs sessions on another machine, in a WSL distro or in a VM. Declare each place in
Fleet's config, under its runtime
([runtimes and their places](fleet/configuration.md#runtimes-and-their-places)):

```toml
[runtimes.mux]
program = "runtime-mux"
default = true   # several runtimes need one default

[runtimes.mux.places.local]   # keeps local: a places table replaces the implied one

[runtimes.mux.places.devbox]
kind = "ssh"
destination = "me@devbox"

[runtimes.mux.places.wsl-dev]
kind = "wsl"
distro = "Ubuntu"

[runtimes.vm]
program = "runtime-vm"

[runtimes.vm.places.sandbox]
image = "agent-base"
```

`fleet doctor` reports what each place is missing, and the command that fixes
it. Nothing installs silently.

## The new-session modal

Status: implemented. No VM runtime ships yet, so `vm/sandbox` is an example.

```text
New session · Fleet
  ▸ laptop       local Fleet      ● up
    box          ssh box          ● up
    gpu          ssh gpu          ○ Fleet not running — install the systemd unit (see doctor)

New session · Place on box
  ▸ local        box itself (mux)        ● ready
    mux/wsl-dev  WSL distro on box       ● ready
    vm/sandbox   new VM per session      ● ready

New session · Repository on box/local
  ~/src/▏          (browsing box's folders)
  or paste a clone URL

Rows:  ● fix-auth      box
       ● try-vm        box · vm/sandbox
       ○ old-task      gpu  (Fleet unreachable)
```

- **Fleet**: pick the Fleet. The modal skips this step when only one Fleet is
  listed. Each Fleet shows how thurbox reaches it and its state, read from
  the manager that its `fleet version --json` reports. The modal does not take
  an unreachable Fleet.
- **Place**: pick a place of that Fleet's runtimes, from its `fleet runtime
  places --json`. The cursor opens on the first ready place. A place that
  is not ready is dimmed, shows why on one line under the list, and the
  modal does not take it. The agents that the modal offers next are that
  Fleet's too.
- **Repository**: browse folders on the place, pick a remembered
  repository, or paste a clone URL: the runtime clones it there. On a remote
  Fleet, browsing starts at that machine's home, `~` is that home, and the
  folders come from `fleet runtime browse` through that Fleet. A worktree
  there (`alt+w`) asks for its base branch from `fleet runtime branches`, as
  that machine last fetched them, and the runtime makes the worktree on the
  place. A runtime without `browse` leaves a typed path, and says so.
- **Rows**: each row shows its Fleet, and its place when the place is not
  `local`. A row of an unreachable Fleet stays in the list. Implemented
  today: the session list shows a Fleet on another machine where it shows a
  host (`@ box`, `@ box · vm/sandbox`), and an unreachable Fleet's rows say
  `Fleet box unreachable`.
