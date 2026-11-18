# Thurbox

<div align="center">
  <img src="./website/assets/logo.svg" alt="thurbox" width="340">
</div>

[![CI](https://github.com/Thurbeen/thurbox/workflows/CI/badge.svg)](https://github.com/Thurbeen/thurbox/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Website](https://img.shields.io/badge/Website-thurbox.thurbeen.eu-blue)](https://thurbox.thurbeen.eu/)
[![Discord](https://img.shields.io/discord/1542644702984142928?label=Discord&logo=discord&logoColor=white&color=5865F2)](https://discord.gg/fGumcHaxFY)
[![Quality Gate Status](https://sonarcloud.io/api/project_badges/measure?project=Thurbeen_thurbox&metric=alert_status)](https://sonarcloud.io/summary/new_code?id=Thurbeen_thurbox)

![Thurbox Demo](https://github.com/Thurbeen/thurbox/releases/download/media/thurbox-demo.gif)

Thurbox runs coding agents side by side in one terminal. Each agent is a session
that keeps working when you quit thurbox, and every pane of the interface is a Lua
file you can change.

- **Several coding agents at once**: Claude Code, Codex, pi, opencode, kilo, omp,
  Qwen Code, Copilot CLI and Kimi Code, launched unmodified.
- **Sessions that outlive the interface**: Fleet runs each session in a
  multiplexer, so a crash, a restart or `Ctrl+Q` stops nothing.
- **One list for every machine**: follow the Fleet on your laptop and a Fleet on
  another machine, over ssh or in WSL, in the same session list.
- **A worktree per session**, so agents never fight over your checkout.
- **An interface you own**: every pane, the session list included, is a Lua file
  in a directory you can edit.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="website/assets/overview-dark.svg">
  <img src="website/assets/overview-light.svg" alt="thurbox lists the sessions of every Fleet; each Fleet's runtime runs a session at a place">
</picture>

<a id="install"></a>

## Install

**Linux / macOS:**

```bash
curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.ps1 | iex
```

The installer puts `thurbox` (the interface), `thurbox-cli`, `fleet`,
`runtime-mux` and `workspace-git` on your `PATH`. You also need:

- **tmux ≥ 3.2**, or [psmux](https://github.com/psmux/psmux) ≥ 3.3.8 on native Windows
- **git**
- at least one coding-agent CLI

**Fleet on its own** needs no thurbox. The installer's fleet-only mode installs
`fleet`, `runtime-mux` and `workspace-git`, and nothing else:

```bash
curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh -s -- --fleet-only
```

On Windows:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.ps1))) -FleetOnly
```

Then run `fleet init`, `fleet doctor` and `fleet up`
([Fleet's install](https://thurbox.thurbeen.eu/docs/fleet-install.html)).

**Upgrading from 2.x:** see [Migrating from v2](#migrating-from-v2).

`cargo build --release` in a clone builds every program from source. Homebrew,
the AUR, Nix, winget, Chocolatey and uninstalling are on the
[Installation page](https://thurbox.thurbeen.eu/docs/installation.html).
Homebrew, the AUR, winget and Chocolatey still carry 2.x: v3 ships through the
install script and Nix.

## First steps

```bash
fleet init                    # writes Fleet's config
fleet service install --now   # Fleet's manager, `fleet up`, as a user service
thurbox-cli doctor            # checks each Fleet, its runtime and its agents
thurbox
```

On Linux the install script runs the first two. thurbox never starts,
restarts or sets up Fleet's manager: a user service runs `fleet up`
([the unit](docs/where-sessions-run.md#2-laptop-thurbox-fleet-on-a-remote-box)).
While none runs, the status line says which command starts it. thurbox draws
the session list on the left and an agent terminal on the right.

1. **`Ctrl+N`** opens the new-session flow: a place, a repository (with
   `Alt+W` for a worktree of its own), a name and a branch, then an agent.
2. **Talk to it.** The right pane is the live agent CLI; every key goes to it.
3. **`Ctrl+N` again** for a second agent. `Ctrl+J` / `Ctrl+K` move between
   sessions; the one you are not looking at keeps working.
4. **`Ctrl+Q`** leaves the interface and stops nothing. Run `thurbox` again and
   both agents are still mid-task. `fleet list task` lists them from any shell.

`F1` lists every key, and `Ctrl+P` is a command palette over the same actions.
The [tutorial](https://thurbox.thurbeen.eu/docs/tutorial.html) walks through it
with screenshots.

To follow a Fleet on another machine, run `fleet service install --now` there and
list it in `~/.config/thurbox/settings.toml`:

```toml
[fleets.laptop]
transport = "local"

[fleets.box]
transport = { ssh = "me@box" }
program = "/home/me/.local/bin/fleet"   # ssh does not read your login PATH
```

[Where sessions run](docs/where-sessions-run.md) explains Fleets and places and
shows how to set up the user service. [`[fleets]`](docs/thurbox/config.md#fleets--the-fleets-that-manage-sessions)
lists every key, the WSL form included.

## Make it yours

Panes live in `ui/plugins/*.lua`, the arrangement in `ui/layout.lua`, and `F10`
reloads both from disk. The interface directory ships an `AGENTS.md`, so from a
session opened there you can just ask your agent:

> *Add a pane on the left with CPU and RAM usage. Move the search strip to the
> bottom and make the session column 30% wide.*

[The interface](https://thurbox.thurbeen.eu/docs/interface.html) ·
[Writing a pane](docs/thurbox/plugins.md) ·
[Plugins](https://thurbox.thurbeen.eu/plugins.html)

## What's new in v3

- **Fleet is the engine.** Every session is a Fleet Task. thurbox keeps no
  database of its own: it reads Fleet and draws what it reads.
- **Several Fleets.** List them in `settings.toml` under `[fleets.<name>]`: one on
  this machine, others over ssh or WSL. A Fleet that cannot be reached keeps its
  sessions in the list, marked unreachable.
- **Places.** A Fleet's runtime runs each session at a place, named
  `<runtime>/<place>`. The shipped runtime, `runtime-mux`, keeps sessions alive
  in tmux, or psmux on Windows; `workspace-git` makes each session's worktree.
- **A new-session flow in that order**: `Ctrl+N` picks a Fleet, then a place,
  then a repository, then an agent.
- **Agents come from Fleet.** The agents on offer are the chosen Fleet's harness
  profiles, so adding an agent is a change to Fleet, not to thurbox.
- **Scripts talk to Fleet.** `fleet apply`, `fleet send` and `fleet delete` drive
  sessions while the interface shows them. `thurbox-cli doctor` checks each Fleet.

## Migrating from v2

Two commands move your 2.x work to v3. Neither deletes a 2.x file.

1. Stop the Fleet 2 manager, back up thurbox's and Fleet's config and data
   directories, and install `sqlite3`.
2. Install v3 with the [install script](#install). No updater moves 2.x to
   3.x.
3. Migrate the Fleet 2 config and store. Fleet keeps the 2.x ones as
   `config.toml.v2` and `store-v1.v2`:

   ```bash
   fleet migrate --dry-run
   fleet migrate
   fleet service install --now   # Fleet 3's manager
   fleet doctor
   ```

4. Import the thurbox 2.x sessions and cron automations as Tasks and
   CronTasks; the report names what it could not import.
   It reads `thurbox.db`, `hosts.toml` and `agents.toml` and changes none
   of them. It removes retired keys from `settings.toml` and keeps the old
   file as `settings.toml.v2`:

   ```bash
   thurbox-cli migrate --dry-run
   thurbox-cli migrate
   ```

5. Add what `thurbox-cli migrate` prints: paste a place for each
   `hosts.toml` host into Fleet's config, and write each harness profile for
   an `agents.toml` agent as the `harnesses/<agent>.toml` file it names,
   beside Fleet's config.
6. Start the work you want. Nothing starts on its own: an imported Task has
   `spec.dispatch = false` and a CronTask `spec.suspend = true`. Set
   `spec.dispatch` to `true` (`spec.suspend` to `false`), then `fleet apply`
   it again. Before you start a Task from a 2.x worktree, finish that
   session's work there and remove the worktree
   ([Start your work again as Tasks](docs/migrating-to-v3.md#3-start-your-work-again-as-tasks)).

`--dry-run` prints the plan and changes nothing.
[Migrating to v3](docs/migrating-to-v3.md) covers each step, services,
remote places and cleaning up what 2.x left.

## Staying on v2

To keep thurbox v2, pin [v2.89.0](https://github.com/Thurbeen/thurbox/releases/tag/v2.89.0),
the latest v2 release.

**Linux / macOS:**

```bash
curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | VERSION=v2.89.0 sh
```

**Windows (PowerShell):**

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.ps1))) -Version v2.89.0
```

thurbox v2 needs no Fleet: ignore the installer's warning that the release
has no Fleet. `thurbox-cli update` keeps v2 on 2.x. If you also use Fleet v2,
install [v2.24.0](https://github.com/Thurbeen/fleet/releases/tag/v2.24.0)
after thurbox v2 (Linux, and experimentally Apple Silicon macOS):

```bash
curl -fsSL https://github.com/Thurbeen/fleet/releases/download/v2.24.0/install-release.sh | sh
```

The [v2.89.0 README](https://github.com/Thurbeen/thurbox/tree/v2.89.0#installation)
and the [Fleet v2.24.0 README](https://github.com/Thurbeen/fleet/tree/v2.24.0#setup)
cover the rest.

## Documentation

[**thurbox.thurbeen.eu/docs**](https://thurbox.thurbeen.eu/docs/) is the manual:
[Fleet](https://thurbox.thurbeen.eu/docs/fleet.html) and its
[features](https://thurbox.thurbeen.eu/docs/fleet-features.html),
[installation](https://thurbox.thurbeen.eu/docs/installation.html),
[tutorial](https://thurbox.thurbeen.eu/docs/tutorial.html),
[thurbox's features](https://thurbox.thurbeen.eu/docs/thurbox-features.html),
[keybindings](https://thurbox.thurbeen.eu/docs/keybindings.html),
[configuration](https://thurbox.thurbeen.eu/docs/configuration.html),
[agents](https://thurbox.thurbeen.eu/docs/agents.html),
[the interface](https://thurbox.thurbeen.eu/docs/interface.html) and
[architecture](https://thurbox.thurbeen.eu/docs/architecture.html).

In this repository, [`docs/`](docs/README.md) covers the whole system: thurbox
([`docs/thurbox/`](docs/thurbox/README.md)), Fleet
([`docs/fleet/`](docs/fleet/README.md)), the runtimes
([`docs/runtime/`](docs/runtime/README.md)), [where sessions
run](docs/where-sessions-run.md) and the [constitution](docs/constitution.md).

## Contributing

[`CONTRIBUTING.md`](CONTRIBUTING.md) is the full guide — the Nix dev shell, the
`just` tasks, the test discipline, and the conventional-commit rules that decide
what gets released.

```bash
git clone https://github.com/Thurbeen/thurbox.git
cd thurbox
nix develop          # or ./scripts/install-dev-tools.sh
just build && just test && just lint
```

## Community

- **[Discord](https://discord.gg/fGumcHaxFY)** — questions, setup help, and
  showing off your interface. Ask in `#help`, one thread per problem.
- **[GitHub Issues](https://github.com/Thurbeen/thurbox/issues)** — confirmed
  bugs and concrete feature requests.

## License

MIT — see [LICENSE](LICENSE). Built on
[ratatui](https://github.com/ratatui-org/ratatui),
[tui-term](https://github.com/a-kenji/tui-term),
[vt100](https://github.com/doy/vt100-rust) and
[tmux](https://github.com/tmux/tmux).
