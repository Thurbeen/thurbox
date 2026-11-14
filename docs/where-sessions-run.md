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
  Fleet "laptop"          Fleet "box"  (on box; a user systemd unit runs fleet up)
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
| `hosts.toml` | thurbox's own file of ssh and WSL machines. It only gives the companion shell a route. It goes away when places replace it. |

Older pages and Fleet's `spec.host` field use "host" for a place.

## Today and planned

| Part | Status |
| --- | --- |
| One Fleet, named by `[fleet] program` in thurbox's `settings.toml` | implemented |
| Fleet's `[hosts.<name>]` table, which names a runtime for each host | implemented |
| The runtime protocol carries a place, and `manifest` reports places | implemented |
| `runtime-mux` runs sessions on `local` only | implemented |
| Places in Fleet's config, `[runtimes.<name>.places.<place>]`, in place of `[hosts]` | planned |
| Several Fleets in `settings.toml`, `[fleets.<name>]` | planned |
| The Fleet step and the place step in the new-session modal | planned |
| `runtime-mux` places over ssh and WSL, a VM runtime | planned |

## Common setups

### 1. Laptop only

Status: implemented.

1. Install `fleet`, `runtime-mux`, a multiplexer and a coding agent.
2. Run `fleet init`.
3. Start thurbox. thurbox starts `fleet up` when no manager runs.

Each session runs on `local`: the laptop.

### 2. Laptop thurbox, Fleet on a remote box

Status: planned. Today, connect to box over ssh and run thurbox there.

This is the most common setup. The agents run on box, so they keep working
when the laptop sleeps.

1. On box, install `fleet`, `runtime-mux`, `tmux` and a coding agent.
2. On box, run `fleet init`.
3. On box, install a user systemd unit that runs `fleet up`.
   thurbox never starts `fleet up` on a remote Fleet.

   ```ini
   # ~/.config/systemd/user/fleet.service
   [Unit]
   Description=Fleet manager

   [Service]
   ExecStart=%h/.cargo/bin/fleet up
   Restart=on-failure
   RestartSec=15

   [Install]
   WantedBy=default.target
   ```

   ```sh
   loginctl enable-linger "$USER"
   systemctl --user enable --now fleet.service
   ```

4. On the laptop, list both Fleets in thurbox's `settings.toml`. When the
   file lists a Fleet, thurbox uses only the listed Fleets.

   ```toml
   [fleets.laptop]
   transport = "local"

   [fleets.box]
   transport = { ssh = "me@box" }
   program = "/home/me/.cargo/bin/fleet"   # ssh does not read your login PATH
   ```

5. Run `thurbox-cli doctor`. It reports each Fleet, and says when box's
   manager does not run.

### 3. One Fleet, several places

Status: planned. Today, `runtime-mux` runs sessions on `local` only.

One Fleet runs sessions in a WSL distro or a VM. Declare each place in
Fleet's config, under its runtime
([places of a runtime](fleet/configuration.md#planned-places-of-a-runtime)):

```toml
[runtimes.mux.places.wsl-dev]
kind = "wsl"
distro = "Ubuntu"

[runtimes.vm.places.sandbox]
image = "agent-base"
```

`fleet doctor` reports what each place is missing, and the command that fixes
it. Nothing installs silently.

## The new-session modal

Status: planned. Today the modal has a host step that lists the hosts of the
one Fleet.

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
  listed.
- **Place**: pick a place of that Fleet's runtimes. A place that is not ready
  shows why.
- **Repository**: browse folders on the place, or paste a clone URL.
- **Rows**: each row shows its Fleet, and its place when the place is not
  `local`. A row of an unreachable Fleet stays in the list.
