# Thurbox

<div align="center">
  <img src="./website/assets/logo.svg" alt="thurbox" width="340">
</div>

Run several coding agents at once — Claude Code, Codex, Antigravity, opencode,
aider, or any CLI you describe yourself — side by side in one terminal. Each
gets its own persistent tmux session and its own git worktree, so they never
fight over your checkout, and they survive crashes, restarts and reboots. Quit
thurbox and every agent keeps working; relaunch and they are all still there.

Thurbox is agent-neutral. It launches the vendor CLI unmodified and knows
nothing about its model, prompts or tools, so you get new agent features the day
the CLI ships them. And the interface is not compiled in: every pane you see,
the session list included, is a Lua file in a directory you own — move it, turn
it off, rewrite it, or install one somebody else wrote.

[![CI](https://github.com/Thurbeen/thurbox/workflows/CI/badge.svg)](https://github.com/Thurbeen/thurbox/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Website](https://img.shields.io/badge/Website-thurbox.thurbeen.eu-blue)](https://thurbox.thurbeen.eu/)
[![Discord](https://img.shields.io/discord/1542644702984142928?label=Discord&logo=discord&logoColor=white&color=5865F2)](https://discord.gg/fGumcHaxFY)
[![Quality Gate Status](https://sonarcloud.io/api/project_badges/measure?project=Thurbeen_thurbox&metric=alert_status)](https://sonarcloud.io/summary/new_code?id=Thurbeen_thurbox)

![Thurbox Demo](./media/thurbox-demo.gif)

## Installation

**Linux / macOS:**

```bash
curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.ps1 | iex
```

That installs both binaries — `thurbox` (the TUI) and `thurbox-cli` (the
headless one) — with checksum verification and platform auto-detection.

You also need **tmux ≥ 3.2** (or [psmux](https://github.com/psmux/psmux) on
native Windows), **git**, and at least one coding-agent CLI.

Homebrew, the AUR, winget, Chocolatey, building from source, pinning a version,
changing the install directory and uninstalling are all on the
[Installation page](https://thurbox.thurbeen.eu/docs/installation.html).

## Your first session

```bash
thurbox
```

First launch seeds `~/.config/thurbox/` — the agents thurbox knows, the themes,
and the interface itself — then draws a session list on the left and an agent
terminal on the right.

1. **`Ctrl+N`** opens the repo picker. `Space` toggles a repo, `w` puts it in
   worktree mode (you are asked for a base branch and a new branch name),
   `Enter` confirms. Name the session, then pick an agent.
2. **Talk to it.** The right pane is a live agent CLI; every key goes to it.
3. **`Ctrl+N` again** for a second agent on a second branch. `Ctrl+J` / `Ctrl+K`
   move between sessions — the one you are not looking at keeps working.
4. **`Ctrl+Q`** leaves the TUI without killing anything. Run `thurbox` again and
   both agents are still mid-task, or attach raw with `tmux -L thurbox attach`.

That is the whole product in four steps: parallel work that keeps going when you
stop watching it.

`F1` lists every key and is rendered from the live registry, so it cannot drift
from what is running; `Ctrl+P` is a command palette over the same actions.
For the same walkthrough with screenshots, see the
[tutorial](https://thurbox.thurbeen.eu/docs/tutorial.html).

![Session creation](./media/thurbox-session-creation.gif)

## What else it does

- **[Global search](https://thurbox.thurbeen.eu/docs/features.html#global-search)**
  (`Ctrl+/`) — find a session by name, agent, branch or repo, *and* by the text
  on its screen, which is the half that finds it by the error in it.
- **[Fork and lead/worker trees](https://thurbox.thurbeen.eu/docs/features.html#session-forking)**
  (`Ctrl+F`) — branch a conversation; children nest under their lead.
- **[Worktrees and multi-repo sessions](https://thurbox.thurbeen.eu/docs/features.html#git-worktrees)**
  — one session can span several repos, each on its own worktree of a shared
  branch. `Ctrl+S` syncs them with their base.
- **[Remote SSH and WSL sessions](https://thurbox.thurbeen.eu/docs/features.html#remote-ssh-sessions)**
  — declare hosts in `hosts.toml` and sessions run there while the TUI stays
  local.
- **[A headless CLI](https://thurbox.thurbeen.eu/docs/features.html#headless-cli)**
  — `thurbox-cli` creates, drives, captures and tears down sessions from a
  script, over the same database the TUI is reading.
- **[Automations and tasks](https://thurbox.thurbeen.eu/docs/features.html#automations)**
  — scheduled agent runs and a todo list whose items can be handed to an agent.
  A tmux heartbeat fires them whether or not thurbox is open.
- **[Inter-session messages](https://thurbox.thurbeen.eu/docs/features.html#inter-session-messages)**
  — an agent-neutral mailbox so one agent hands another a payload instead of
  scraping its terminal.
- **[Extensions](https://thurbox.thurbeen.eu/docs/extensions.html)** — opt-in
  add-ons that are data, not code: a manifest declares the agents, files,
  sessions and automations it wants, and one command installs and self-heals
  them.
- **[Session lifecycle hooks](https://thurbox.thurbeen.eu/docs/configuration.html#hooks-toml)**,
  **[OS notifications](https://thurbox.thurbeen.eu/docs/configuration.html#notifications)**,
  **[36 themes](https://thurbox.thurbeen.eu/docs/features.html#themes)**
  (`Ctrl+Y`) and full mouse support.

Wiring a fleet of agents at once — operators, developers, reviewers, from one
script — is the
[monorepo recipe](https://thurbox.thurbeen.eu/docs/recipes.html#monorepo).

## Make it yours

The binary boots a kernel, reads a directory, and draws whatever Lua it finds
there. Panes live in `ui/plugins/*.lua`, the arrangement in `ui/layout.lua`, and
`F10` reloads both from disk. Agents, hosts, hooks, themes, chords and settings
are each a file under `~/.config/thurbox/`.

You do not have to learn Lua for any of it. Thurbox ships a built-in `ui-skill`
extension that teaches whichever CLI you run how the interface is put together,
so from inside any session you can just ask:

> *Add a pane on the left with CPU and RAM usage. Move the search strip to the
> bottom and make the session column 30% wide.*

Press `F10` and the change is on your screen. If a pane breaks, `Ctrl+,` then
`]` is chrome rather than a pane, so it cannot be edited away — `r` restores a
shipped file, `space` turns yours off.

[The Interface →](https://thurbox.thurbeen.eu/docs/interface.html) ·
[Writing a pane →](docs/PLUGINS.md) ·
[Configuration →](https://thurbox.thurbeen.eu/docs/configuration.html)

![Turning a pane off from the Interface tab](./media/thurbox-interface.gif)

## Documentation

[**thurbox.thurbeen.eu/docs**](https://thurbox.thurbeen.eu/docs/) is the manual:
[installation](https://thurbox.thurbeen.eu/docs/installation.html),
[tutorial](https://thurbox.thurbeen.eu/docs/tutorial.html),
[features](https://thurbox.thurbeen.eu/docs/features.html),
[keybindings](https://thurbox.thurbeen.eu/docs/keybindings.html),
[configuration](https://thurbox.thurbeen.eu/docs/configuration.html),
[agents](https://thurbox.thurbeen.eu/docs/agents.html),
[the interface](https://thurbox.thurbeen.eu/docs/interface.html),
[orchestration](https://thurbox.thurbeen.eu/docs/orchestration.html),
[architecture](https://thurbox.thurbeen.eu/docs/architecture.html),
[a comparison with similar tools](https://thurbox.thurbeen.eu/docs/comparison.html)
and an [FAQ](https://thurbox.thurbeen.eu/docs/faq.html).

The rationale behind the decisions is in this repository, under
[`docs/`](docs/): [CONSTITUTION](docs/CONSTITUTION.md) (non-negotiable
principles), [ARCHITECTURE](docs/ARCHITECTURE.md), [FEATURES](docs/FEATURES.md),
[CONFIG](docs/CONFIG.md), [V2-KERNEL](docs/V2-KERNEL.md),
[PLUGINS](docs/PLUGINS.md), [ORCHESTRATION](docs/ORCHESTRATION.md),
[DEVELOPMENT](docs/DEVELOPMENT.md) and [RELEASING](docs/RELEASING.md).

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
