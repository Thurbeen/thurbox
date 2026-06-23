# Thurbox

Run any coding-agent CLI in persistent terminal sessions.
Thurbox is a multi-session TUI orchestrator that launches
Claude Code, Codex, Antigravity, opencode, aider — or any agent
you describe — inside persistent tmux panes that survive
crashes, restarts, and reboots. Sessions, agents, and git
worktrees are first-class citizens.

[![CI](https://github.com/Thurbeen/thurbox/workflows/CI/badge.svg)](https://github.com/Thurbeen/thurbox/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Website](https://img.shields.io/badge/Website-thurbox.thurbeen.eu-blue)](https://thurbox.thurbeen.eu/)
[![Quality Gate Status](https://sonarcloud.io/api/project_badges/measure?project=Thurbeen_thurbox&metric=alert_status)](https://sonarcloud.io/summary/new_code?id=Thurbeen_thurbox)

![Thurbox Demo](./docs/media/thurbox-demo.gif)

> **Note:** Thurbox is still **v0.x.x**. While we try hard to avoid
> them, breaking changes may occasionally happen between releases
> until the project reaches 1.0. Pin a version if you need stability.

## Installation

**One-liner:**

```bash
curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
```

Installs the latest release to `~/.local/bin` with checksum
verification and platform auto-detection.

**Options:**

```bash
# Custom directory
INSTALL_DIR=/usr/local/bin curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh

# Pin a version
VERSION=v0.1.0 curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.ps1 | iex
```

Installs the latest `x86_64-pc-windows-msvc` release to
`%LOCALAPPDATA%\Programs\thurbox` (added to your user `PATH`) with checksum
verification. Pin a version or directory with the `THURBOX_VERSION` /
`THURBOX_INSTALL_DIR` env vars. Needs [psmux](https://github.com/psmux/psmux)
as the multiplexer.

**Chocolatey (Windows):**

```powershell
choco install thurbox
```

Installs the prebuilt x86_64 Windows binaries (`thurbox.exe` +
`thurbox-cli.exe`) from the GitHub Release and shims them onto your `PATH`.
Needs [psmux](https://github.com/psmux/psmux) as the multiplexer (installed
separately — there is no Chocolatey package for it).

**Homebrew (macOS / Linux):**

```bash
brew install thurbeen/thurbox/thurbox
```

Installs the prebuilt release binaries (`thurbox` + `thurbox-cli`)
from the [tap](https://github.com/Thurbeen/homebrew-thurbox), with
`tmux` and `git` pulled in as dependencies. Supports macOS arm64
(Apple Silicon) and Linux x86_64.

**Arch Linux (AUR):**

Thurbox is on the AUR as
[`thurbox`](https://aur.archlinux.org/packages/thurbox) (builds
from source) and
[`thurbox-bin`](https://aur.archlinux.org/packages/thurbox-bin)
(prebuilt release binary). Install with your AUR helper:

```bash
paru -S thurbox-bin   # prebuilt binary (fastest)
paru -S thurbox       # build from source
```

`tmux` is pulled in as a dependency. (Swap `paru` for `yay` or
your preferred helper.)

**From source:**

```bash
sudo pacman -S --needed git tmux rust   # Arch deps; use your distro's equivalent
git clone https://github.com/Thurbeen/thurbox.git
cd thurbox
cargo build --release
# binary at target/release/thurbox
```

See [Prerequisites](#prerequisites) for required tooling.

**Contributing / hacking on thurbox?** The dev environment is a reproducible Nix
flake (`nix develop` / `direnv allow`) with `just` tasks and an isolated runtime
sandbox (`scripts/dev/sandbox.sh`) — see
[`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md).

## Why Thurbox

Running a coding agent in several terminals gets you far — until
you want to keep sessions alive across crashes, isolate them
per-branch, or juggle different agents side-by-side. Thurbox
adds:

- **Persistence** — sessions live in tmux and survive Thurbox
  crashes, restarts, and reboots. Reattach from any terminal with
  `tmux -L thurbox attach`.
- **Parallelism** — many agents side-by-side, each on its own
  repo(s) and branch, each running the agent you chose.
- **Any agent** — a session runs one coding-agent CLI selected at
  creation time. Built-ins (claude, codex, antigravity, opencode,
  aider, vibe) are seeded into `~/.config/thurbox/agents.toml`; add your
  own without recompiling.
- **Git worktree isolation** — each session can spawn on a fresh
  worktree; `Ctrl+S` syncs them with their base branch and asks the
  agent to resolve rebase conflicts automatically.

## Main Features

### Sessions

- Persistent tmux-backed panes, parallel agents, per-session
  working dirs.
- Each session runs one coding agent; nothing else is configured
  per session. Each agent runs with its own default config.
- `Ctrl+R` restart preserves the conversation when the agent
  supports resume; `Ctrl+F` forks a session; `Ctrl+T` toggles a
  shell pane.
- Order the list by hand with `Shift+J`/`Shift+K` (move the
  selected session and its children), or `Shift+S` to sort
  alphabetically within each repo group. Manual order wins over
  status and survives restarts.
- Sessions can carry a **parent** (lead/worker): `Ctrl+F` records
  one automatically, and `thurbox-cli session create --parent
  <uuid>` sets it headlessly. The link is informational only —
  deleting a lead never cascades to its workers.
- Global search (`Ctrl+/`), full mouse navigation, clickable URLs,
  automations (`Ctrl+P`), soft-delete with undo (`Ctrl+Z`) and
  restore (`Ctrl+U`).

> **Note:** The session list display is not yet perfect and will
> keep improving. What it can show is heavily dependent on the
> signals each agent CLI exposes.

Create one with `Ctrl+N` — pick a repo, name it, choose an agent:

![Session creation workflow](./docs/media/thurbox-session-creation.gif)

Fork one with `Ctrl+F` — the copy records the source as its
**parent**, and children nest under their parent in the session
list:

![Session forking](./docs/media/thurbox-fork.gif)

### Automations

- Named, scheduled agent runs — one-shot or recurring (cron, with
  friendly `hourly`/`daily`/`weekdays`/`weekly` presets). When one
  fires it either **sends** a prompt to a running session or
  **spawns** a fresh session (optionally on a new worktree) and
  prompts it.
- A dedicated **Automations pane** sits below the session list;
  focus it and press `Ctrl+N` to create (or `Space`/`r`/`e`/`d` to
  toggle/run/edit/delete). The editor needs no cron knowledge — the
  trigger is a selector and the time is set with steppers, with a
  live "next fire" preview.
- Fires even when the TUI is closed via a tmux heartbeat keeper
  (with opt-in systemd/launchd units for reboot-proof firing), and
  is fully scriptable headless: `thurbox-cli automation
  create/list/edit/run/tick`.

> **Note:** Automations are stable and good enough for daily use
> today, but the feature may still evolve.

![Automations demo](./docs/media/automations-demo.gif)

### Tasks

- A built-in **todo list** whose items can be **connected to a
  coding agent** with the same Send/Spawn model as automations:
  **Send** pastes the task title into an existing session,
  **Spawn** creates a fresh session (optionally on a new worktree)
  seeded with the title, and an unconnected task is a plain local
  todo. Triggering a task (`r`) runs its action and advances it to
  *in progress*.
- Tasks live in a **toggleable right-side column** (`Ctrl+W` /
  `F5`) that behaves like the file viewer. Focus it and press `n`
  to create, `e`/`Enter` to edit (an in-pane editor, no popup),
  `Space` to cycle status (☐ todo · ◐ in progress · ☑ done), and
  `d` to delete.
- Fully scriptable headless: `thurbox-cli task` (alias `todo`) —
  `create`/`list`/`show`/`edit`/`remove`/`run`. External
  issue-tracker sync (Jira, GitHub Issues) is scaffolded for a
  later release.

> **Note:** Tasks are a new feature — expect the UX and UI to keep
> evolving in upcoming releases.

![Tasks demo](./docs/media/tasks-demo.gif)

### Inter-session messages

- A general, agent-neutral **message queue** lets one session hand
  another a **structured payload** — addressed to a session, with a
  free-form `kind` tag, a body, and optional sender/task provenance —
  instead of scraping its rendered terminal. It is the channel
  extensions use for agent↔agent coordination.
- A worker **pushes** a clean payload with `thurbox-cli message send`;
  a wake nudge types a short `inbox` token into the recipient's pane
  so it drains immediately. `message inbox --claim` is an atomic,
  exactly-once drain, so the TUI, a cron tick, and a wake nudge can
  read the same inbox concurrently without double-processing.
- Thurbox injects a stable `THURBOX_SESSION` (and `THURBOX_TASK` for
  task-spawned sessions) into each agent, so a CLI call *inside* a
  session sends and reads its own mail passing no ids, and `message
  reply <id>` routes back to a message's original sender. See
  [Headless CLI](#headless-cli-thurbox-cli).

### Extensions

- Opt-in, **agent-agnostic** add-ons that build on `thurbox-cli`
  without touching the core binary. Each extension is **data, not
  code**: it ships an `extension.toml` manifest declaring the agents,
  sessions, automations, and payload files it needs, so thurbox
  installs and **self-heals** it without knowing anything specific
  about it. The agents it registers are plain `agents.toml` aliases
  you can map to claude, codex, antigravity, opencode, vibe, or anything
  else; the behavior lives in a plain context file surfaced to
  whichever CLI you pick.
- One command installs, activates, and (on every TUI start / headless
  tick) re-ensures each extension's resources — idempotent and
  self-healing:

  ```bash
  thurbox-cli extension install <name>     # install + activate
  thurbox-cli extension list               # what's installed
  thurbox-cli extension available          # the built-ins, with install commands
  thurbox-cli extension uninstall <name>   # add --purge to delete its home dir too
  ```

- **Built-in extensions** (fetched, pinned to your binary's release,
  from the official source — `thurbox-cli extension available` lists
  them):
  - **`flow`** — a focus-protecting triage agent. Brain-dump at a
    dedicated cheap **flow session** and it captures everything into
    tasks, dispatches the dispatchable ones to worker sessions (each
    on its own `flow/<slug>` worktree branch, plan-first), monitors
    them via a tick automation, and ends every reply with the single
    next thing to focus on.
  - **`forge`** — a workflow analyst that mines your tasks, sessions,
    and automations for **recurring patterns** and writes
    ready-to-apply `thurbox-cli automation` proposals. It proposes,
    never imposes: nothing is created until you `apply` a proposal.
  - **`ci-shepherd`** — watches your open change requests (GitHub
    PRs / GitLab MRs / Bitbucket PRs) and dispatches a fixer for each
    one with **failing CI**, a **changes-requested review**, or a
    branch that is **behind its target**. Forge-agnostic — the only
    thing baked in is git.
  - **`renovate`** — keeps local repos on up-to-date dependencies.
    Sweeps a watch list, runs **Renovate's `local` platform** (no
    hosted bot, no token), tests the result, and opens a review PR
    per eligible repo.
  - **Task integrations** — `github-issues`, `gitlab-issues`,
    `linear`, `jira`: one extension per provider that **bidirectionally**
    syncs an external issue tracker with the thurbox task list. Issues
    show up as tasks; marking a task done closes/completes the issue
    (and reopens it on revert). A `*-tick` automation runs a deterministic
    sync script over a `trackers.md` watch list every 15 min (no agent,
    no tokens), dedup'd by `(source, external_id)`.

> **Note:** Extensions are a brand-new, **experimental** capability
> under active testing — expect their behavior, specs, and manifests
> to change between releases.

See the [Extensions guide](https://thurbeen.github.io/thurbox/docs/extensions.html)
and each extension's README under
[`extensions/`](./extensions/) ([flow](./extensions/flow/README.md),
[forge](./extensions/forge/README.md),
[ci-shepherd](./extensions/ci-shepherd/README.md),
[renovate](./extensions/renovate/README.md),
[github-issues](./extensions/github-issues/README.md),
[gitlab-issues](./extensions/gitlab-issues/README.md),
[linear](./extensions/linear/README.md),
[jira](./extensions/jira/README.md)).

### Global search

- One key (`Ctrl+/`) opens a **non-modal search strip** that
  searches **every scope at once** — sessions (name/agent/branch
  **and** live terminal-buffer content), tasks, automations, and
  the active session's file tree.
- Matches **highlight live in the panels themselves** (matching
  rows accented, the rest dimmed), with per-scope match counts and
  a grouped result list. `Enter` jumps to the selected result and
  focuses its pane; `Esc` restores exactly what you had before
  searching.

![Global search demo](./docs/media/search-demo.gif)

### Agent definitions

- Agents are declared as data in `~/.config/thurbox/agents.toml`,
  seeded with built-ins on first run and user-extensible. Each
  agent has a `command`, `args` (always passed — bake in flags
  like a model here if you want), and argument-template groups
  (`resume_args`, `fork_args`, `new_session_args`). Each group is
  appended only when its value is present, with `{id}`
  substituted.

### Git worktrees

- Pick "Worktree" in the new-session flow to branch off a base and
  launch the agent inside the worktree. Closing the session removes
  it. `Ctrl+S` syncs all worktree sessions with their base branch.

### Remote SSH sessions

- Run an agent on a **remote machine** over SSH while the TUI stays
  local. Declare hosts in `~/.config/thurbox/hosts.toml` (seeded
  commented-out, so a fresh install has none); each entry becomes a
  selectable backend named `ssh:<name>`. The new-session flow shows a
  **host picker** first, and remote sessions are marked with a `☁`
  glyph in the list.
- The agent process, its tmux window, and any git worktrees all live
  on the remote host. thurbox shells out to your system `ssh`, so
  authentication, keys, and multiplexing come from `~/.ssh/config` —
  thurbox never handles credentials. Remote sessions get the same
  persistence, multi-instance sharing, and restore-on-startup as
  local ones.

  ```toml
  # ~/.config/thurbox/hosts.toml
  [[hosts]]
  name = "devbox"            # backend "ssh:devbox"; what --host expects
  destination = "me@devbox"  # ssh target or a ~/.ssh/config alias
  ssh_opts = ["-o", "ControlMaster=auto", "-o", "ControlPersist=10m"]
  # socket / session  — optional remote tmux -L / session-name overrides
  # worktrees_dir      — optional absolute remote worktrees dir
  ```

  Spawn remotely from the CLI with `thurbox-cli session create
  --host devbox …` (see [Headless CLI](#headless-cli-thurbox-cli)).
  The remote host needs **tmux >= 3.2** and **git**.

### Responsive UI

- `< 80` cols: terminal only · `>= 80`: sidebar + terminal ·
  `>= 120`: sidebar + terminal + info panel. Vim-inspired keys
  throughout. Pick a theme with `Ctrl+Y` (or `F4`).

The **info panel** (`Ctrl+B`) shows per-session details and live
CPU/RAM and agent metrics:

![Info panel](./docs/media/thurbox-info-panel.gif)

The **file viewer** (`Ctrl+E`) browses the session's worktree
tree with fuzzy search:

![File manager](./docs/media/thurbox-file-manager.gif)

Nine themes (five dark, four light) switch live with `Ctrl+Y`:

![Theme switcher](./docs/media/thurbox-theme.gif)

### Mouse navigation

The whole TUI is clickable (enabled by default; see
[Feature flags](#feature-flags) to turn it off):

- **Click a row** in the session list, tasks panel, automations
  pane, or file viewer to select it and focus that pane. A file row
  also opens the file / toggles the directory; a session-list group
  header selects that group's first session.
- **Click a picker row** (theme, agent, host, branch, …) to select
  **and confirm** it in one click. Modals swallow stray clicks, so a
  misclick can never discard typed input.
- **Hover** underlines the row under the pointer; the **mouse
  wheel** scrolls the focused terminal (and steps the selection
  while a modal is open, with a draggable scrollbar on long lists).
- **Drag** selects text in the terminal; `Ctrl+C` copies it.
  `Ctrl+Click` opens a URL.

Set `mouse = false` in `settings.toml` to skip mouse capture
entirely and keep your terminal's native selection / URL handling.

### OS notifications

- When a session crosses into a **needs-you** state — the agent rang
  the terminal bell or emitted an OSC 9 / OSC 777 notification — thurbox
  fires an OS desktop notification so you can react without watching the
  TUI. The body is the agent's last OSC message, or `Waiting for input`.
- On **Linux** the banner is **click-to-focus** (clicking it switches
  the TUI to that session); **macOS** shows the banner but ignores
  clicks. It fires only while the TUI is open, is deduplicated per
  session, and skips the session you are currently viewing by default.
- Tune it in the `[notifications]` block of `settings.toml`
  (`also_on_waiting` / `suppress_for_active` / `sound` /
  `min_interval_secs`), or turn it off with `notifications = false` —
  see [docs/CONFIG.md](docs/CONFIG.md).

### Feature flags

Whole TUI features can be switched off in
`~/.config/thurbox/settings.toml` under `[features]` (seeded
commented-out; everything defaults to `true` except `version_check`
and `auto_update`, which are opt-in because they reach the network). A
disabled feature
hides its pane and turns its keybinding into an explanatory toast,
but its data and the `thurbox-cli` surface keep working — so
flipping a flag back on is lossless.

```toml
[features]
tasks = true          # F5/Ctrl+W tasks panel
automations = true    # automations pane, Ctrl+P, schedule firing
file_viewer = true    # F3/Ctrl+E file viewer
global_search = true  # Ctrl+/ search strip
info_panel = true     # F2/Ctrl+B info panel
shell_pane = true     # Ctrl+T per-session shell
mouse = true          # mouse capture: clicks, wheel, drag-select, hover
notifications = true  # OS desktop alerts when a session needs attention
version_check = false # opt-in GitHub update check (makes a network call)
auto_update = false   # opt-in: silently download+verify+replace binaries on startup
```

`automations = false` is the one flag with teeth beyond the UI: it
also stops the TUI from firing due schedules and arming the tmux
heartbeat at startup (explicit `thurbox-cli automation` commands
still work). The same `settings.toml` holds scalar knobs
(scrollback, layout breakpoints, audit retention) — see
[docs/CONFIG.md](docs/CONFIG.md) for every config file in one place.

## Prerequisites

- **tmux >= 3.2** (Linux / macOS), or **[psmux](https://github.com/psmux/psmux)**
  on native Windows — a drop-in tmux clone thurbox drives identically
- **A coding-agent CLI** — e.g.
  [claude](https://github.com/anthropics/claude-code), codex,
  antigravity, opencode, or aider (whichever agents you plan to run)
- **git** (required for worktree features)
- **Rust 1.75+** (only to build from source)

## Uninstall

Remove the binary, depending on how you installed it:

```bash
rm ~/.local/bin/thurbox        # curl one-liner / manual install
brew uninstall thurbox         # Homebrew
paru -R thurbox thurbox-bin    # Arch (AUR)
choco uninstall thurbox        # Chocolatey (Windows)
```

Sessions outlive Thurbox in tmux, so stop them too:

```bash
tmux -L thurbox kill-server    # ends all running agent sessions
```

To also delete state and config (optional — this erases your
session history, theme, and `agents.toml`):

```bash
rm -rf ~/.local/share/thurbox ~/.config/thurbox
```

## Getting Started

1. **Launch** — run `thurbox`. You'll see a sidebar on the left
   listing your sessions and a terminal panel on the right.
2. **Create your first session** — press `Ctrl+N` to open the repo
   picker. Toggle repos with `Space`; press `w` on a repo to mark
   it as worktree mode (you'll be prompted for a base branch and
   new branch name). Confirm with `Enter`, name the session, then
   pick an **agent**. The agent picker is skipped when only one
   agent is defined.
3. **Work with the agent** — the right pane is a live agent
   session. All keys are forwarded to the PTY; `Ctrl+C` copies if
   you have a selection, otherwise sends SIGINT.
4. **Navigate** — `Ctrl+J` / `Ctrl+K` move between sessions in the
   sidebar; `Ctrl+L` / `Ctrl+H` cycle focus between panes.
   `Ctrl+O` opens the session's worktree in your editor.
5. **Quit without killing** — `Ctrl+Q` detaches all sessions.
   Tmux keeps them running; relaunch `thurbox` and they resume.

See the full [keybindings](#keybindings) below.

## Agents

A session launches exactly one coding-agent CLI. Agents are
described as data in `~/.config/thurbox/agents.toml`, which is
seeded with built-ins (claude, codex, antigravity, opencode, aider,
vibe) on first run. Edit the file to tweak an agent or add a new one — no
recompile required.

Each `[[agents]]` entry maps the resume / fork / new-session ids
onto argument-template groups. `args` is always passed (bake in
any flags you want, e.g. a model); the resume / fork /
new-session groups are appended only when their driving value is
present, with `{id}` substituted token-by-token:

```toml
default = "claude"

[[agents]]
name = "claude"
command = "claude"
resume_args = ["--resume", "{id}"]
fork_args = ["--resume", "{id}", "--fork-session"]
new_session_args = ["--session-id", "{id}"]

[[agents]]
name = "codex"
command = "codex"
```

## Common Workflows

- **Parallel branches** — `Ctrl+N`, pick a repo in Worktree mode,
  name a new branch. Repeat for a second branch. Two isolated
  agents now work in parallel with no git contention.
- **Mix agents** — run Claude Code on one repo and Codex on
  another in side-by-side sessions; each session remembers its own
  agent.
- **Recover a crash** — if Thurbox dies, relaunch it: sessions
  resume from tmux. Prefer raw tmux? `tmux -L thurbox attach`.

### Recipe: provision a monorepo headless

A practical example for a monorepo with a production app — three
roles, each with a different blast radius:

- one or more **operator** sessions in worktrees with a custom MCP
  config — **read/write** access to the prod backoffice;
- one or more **developer** sessions in worktrees with a custom MCP
  config — **read-only** access to the prod backoffice;
- one or more **security/quality reviewer** sessions running
  continuous code review across the monorepo.

A single shell script — no glue code, no overhead:

```bash
#!/usr/bin/env bash
set -euo pipefail
REPO="${REPO:-/home/me/code/monorepo}"   # path to the monorepo
N_OPERATORS="${N_OPERATORS:-1}"
N_DEVELOPERS="${N_DEVELOPERS:-2}"
N_REVIEWERS="${N_REVIEWERS:-1}"
HOST="${HOST:-}"                          # e.g. devbox to run remotely; empty = local
STAMP="$(date +%y%m%d-%H%M)"
cli() { thurbox-cli "$@"; }

host_flag() { [ -n "$HOST" ] && printf -- '--host\n%s\n' "$HOST"; }

# 1) Operator sessions — RW backoffice, each on its own worktree branch.
for i in $(seq 1 "$N_OPERATORS"); do
  cli session create \
    --name "operator-$i" \
    --repo-path "$REPO" \
    --agent operator \
    --worktree-branch "ops/$STAMP-$i" \
    $(host_flag)
done

# 2) Developer sessions — RO backoffice, each on its own worktree branch.
for i in $(seq 1 "$N_DEVELOPERS"); do
  cli session create \
    --name "developer-$i" \
    --repo-path "$REPO" \
    --agent developer \
    --worktree-branch "dev/$STAMP-$i" \
    $(host_flag)
done

# 3) Security/quality reviewer sessions — continuous review across the monorepo.
#    No worktree: review the repo as-is (read-only stance comes from the prompt).
for i in $(seq 1 "$N_REVIEWERS"); do
  id="$(cli session create \
        --name "reviewer-$i" \
        --repo-path "$REPO" \
        --agent reviewer \
        $(host_flag) \
        --json | jq -r '.id')"
  # Seed the reviewer with its standing instructions.
  cli session send --to "$id" --no-wake --body \
"You are a continuous security & code-quality reviewer for this monorepo.
Loop: pick the most recently changed files, review for security issues, correctness bugs,
and quality regressions. Report findings concisely, then move to the next changed area.
Do not modify code — review only."
done

cli session list
```

The "custom MCP config" is just an **agent** that launches `claude`
with a different `--mcp-config` file — Thurbox stays agent-neutral.
Define the three roles in `~/.config/thurbox/agents.toml`:

```toml
default = "claude"

[[agents]]
name = "operator"                 # prod backoffice: read/write
command = "claude"
args = [
  "--mcp-config", "/home/me/.config/thurbox/mcp/backoffice-rw.json",
  "--model", "opus",              # bake any default flags into args
]
new_session_args = ["--session-id", "{id}"]
resume_args      = ["--resume", "{id}"]
fork_args        = ["--resume", "{id}", "--fork-session"]

[[agents]]
name = "developer"                # prod backoffice: read-only
command = "claude"
args = ["--mcp-config", "/home/me/.config/thurbox/mcp/backoffice-ro.json"]
new_session_args = ["--session-id", "{id}"]
resume_args      = ["--resume", "{id}"]
fork_args        = ["--resume", "{id}", "--fork-session"]

[[agents]]
name = "reviewer"                 # continuous code review, no backoffice access
command = "claude"
args = []
new_session_args = ["--session-id", "{id}"]
resume_args      = ["--resume", "{id}"]
fork_args        = ["--resume", "{id}", "--fork-session"]
```

The read/write vs read-only split lives entirely in the two MCP
config files those agents point at (`~/.config/thurbox/mcp/`):

```json
{
  "mcpServers": {
    "backoffice": {
      "command": "npx",
      "args": ["-y", "@yourco/backoffice-mcp"],
      "env": {
        "BACKOFFICE_URL": "https://prod-backoffice.internal",
        "BACKOFFICE_MODE": "rw"
      }
    }
  }
}
```

`backoffice-ro.json` is identical but with `"BACKOFFICE_MODE": "ro"`.
Scale knobs are env vars — `N_DEVELOPERS=3 ./provision.sh` — and
`HOST=devbox ./provision.sh` runs every session's worktree + tmux on
a remote machine from `hosts.toml`. Need one session to span several
repos? Add `--add-repo PATH@main` (its own worktree per repo) or
`--add-dir PATH` (attached read-only) to its `session create`.

## Keybindings

### Global Keys

| Key | Action | Mnemonic |
|-----|--------|----------|
| `Ctrl+Q` | Quit (detach sessions) | **Q**uit |
| `Ctrl+N` | New session (opens repo picker) | **N**ew |
| `Ctrl+C` | Copy selection / SIGINT (terminal) | **C**opy |
| `Ctrl+V` | Paste from clipboard | Paste |
| `Ctrl+P` | Automations (list/new/edit/toggle/run/delete) | **P**rogram |
| `Ctrl+/` | Global search (sessions/tasks/automations/files) | **/** = search |
| `Ctrl+W` / `F5` | Toggle tasks panel (todo list) | **W**ork items |
| `Ctrl+T` | Toggle shell pane | **T**erminal |
| `Ctrl+H` | Focus previous pane (cycle backward) | Vim: **h** = left |
| `Ctrl+J` | Select next session | Vim: **j** = down |
| `Ctrl+K` | Select previous session | Vim: **k** = up |
| `Ctrl+L` | Focus next pane (cycle forward) | Vim: **l** = right |
| `Shift+J` / `Shift+K` | Move selected session down/up (manual order) | reorder |
| `Shift+S` | Sort sessions alphabetically within each repo group | **S**ort |
| `Ctrl+D` | Delete session | Vim: **d** = delete |
| `Ctrl+O` | Open active session's working dirs in editor | **O**pen |
| `Ctrl+R` | Restart active session | **R**estart |
| `Ctrl+F` | Fork active session | **F**ork |
| `Ctrl+S` | Sync worktrees with their base branch | **S**ync |
| `Ctrl+Z` | Undo session delete | **Z** = undo |
| `Ctrl+U` | Restore deleted sessions | **U**ndelete |
| `Ctrl+Y` / `F4` | Pick TUI theme | Color **Y**oke |
| `Ctrl+,` / `F6` | Settings panel (edit settings.toml) | **,** = preferences |
| `F1` / `Ctrl+G` | Keybindings help + interactive editor | Universal |
| `Ctrl+B` / `F2` | Toggle info panel | **B**rief |
| `Ctrl+E` / `F3` | Toggle file viewer | **E**xplorer |

Every chord above is rebindable from the `F1` editor (or by editing
`~/.config/thurbox/keybindings.json`). `Shift+J`/`Shift+K`/`Shift+S`
reorder or sort the session list only while it is focused.

**macOS:** in kitty-protocol terminals (iTerm2 3.5+, kitty, WezTerm,
Ghostty) the Command key works as a modifier — `Cmd+J`/`Cmd+Shift+J`
switch sessions and `Cmd+L`/`Cmd+Shift+L` cycle panes by default, and
any action can be rebound to a `cmd+…` chord from the F1 editor.
Terminal.app delivers no Cmd chords; everything else works there.

### List Navigation

| Key | Action |
|-----|--------|
| `j` / `Down` | Next item |
| `k` / `Up` | Previous item |
| `Enter` | Select / focus |

Searching is unified into the global search strip (`Ctrl+/`); there
is no separate per-list `/` filter. It matches sessions on name,
agent, branch, and live terminal-buffer content. (The file viewer's
own in-file `/` text search is unrelated and still there.)

### Terminal Scrollback and Selection

| Key | Action |
|-----|--------|
| `Shift+Up` / `Shift+Down` | Scroll 1 line |
| `Shift+PageUp` / `Shift+PageDown` | Scroll half page |
| `Alt+PageUp` / `Alt+PageDown` | Scroll half page (fallback where the terminal claims `Shift+Page`, e.g. Terminal.app/iTerm2) |
| Mouse wheel | Scroll 3 lines |
| Mouse drag | Select text |
| Any other key | Snap to bottom + forward to PTY |

## Headless CLI (`thurbox-cli`)

The `thurbox-cli` binary drives Thurbox without the TUI — useful
for scripting and automation. It shares the same SQLite database
and `tmux -L thurbox` server as the TUI, so changes made by either
appear live in the other (the TUI polls `PRAGMA data_version`).

Output is human-readable by default and switches to JSON
automatically when stdout is piped (so `… | jq` keeps working);
force a format with `--json` (compact), `--pretty` (indented JSON),
or `--text` (human even when piped). The binary is intentionally
thin — it parses arguments, calls into the database / tmux helpers,
and prints the result. There is no TUI and no event loop.

```bash
cargo build --bin thurbox-cli
```

### Sessions

```bash
thurbox-cli session list                 # all active sessions
thurbox-cli session get <uuid>           # one session by UUID
thurbox-cli session create \
  --name reviewer \
  --repo-path /path/to/repo \
  --agent codex \
  --worktree-branch feat/x \
  --base-branch main \
  --host devbox          # optional — run on a remote host from hosts.toml
thurbox-cli session send <uuid> "run the test suite"
thurbox-cli session capture <uuid> --lines 500
thurbox-cli session restart <uuid>       # kill + re-spawn with --resume
thurbox-cli session delete <uuid>        # soft-delete (see below)
thurbox-cli session restore <uuid>       # undo a soft-delete
```

- **`create`** runs synchronously — the tmux window is live by the
  time the command returns. `--agent` falls back to the default in
  `agents.toml` when omitted; `--worktree-branch` (off
  `--base-branch`, default `main`) creates a git worktree; `--host`
  (a name from `hosts.toml`) creates the worktree and tmux window on
  that remote host over SSH instead of locally.
- **`send`** types text into the session's terminal followed by
  Enter; **`capture`** dumps the rendered pane as text (`--lines`
  defaults to 200, max 10000).

#### How session delete is handled

`session delete <uuid>` is a **soft-delete**: by default it only
marks the database row as deleted. The tmux window, any git
worktrees, and pending scheduled commands are **left untouched** —
the running TUI cleans those up on its next sync, and the session
stays recoverable with `session restore <uuid>` (the TUI's `Ctrl+U`
/ `Ctrl+Z` undo path). Restore revives the metadata; the TUI
re-spawns a fresh window for it.

Pass **`--force`** to also tear down the session's runtime resources
in the same call — useful for headless cleanup when no TUI is
running to observe the deletion. With `--force` the command:

- kills the tmux window,
- removes the session's git worktrees (the underlying repos are left
  intact),
- removes the multi-repo symlink workspace, if any (only the
  symlinks — never the linked repos), and
- disables any `send` automations that target the session.

Teardown is **best-effort**: individual tmux/worktree failures are
recorded in the JSON report (`killed_window`, `removed_worktrees`,
`worktree_errors`, `disabled_automations`) but never abort the
delete. The DB row is always soft-deleted last, so even a forced
delete remains restorable (it just re-spawns from a clean slate).

### Automations (alias `auto`)

Scheduled agent runs, persisted to the shared DB. See
[Automations](#automations) for the model.

```bash
thurbox-cli automation create \
  --name nightly-triage \
  --trigger weekdays --time 09:00 \
  --session <uuid> --prompt "triage new issues"
thurbox-cli automation list
thurbox-cli automation show <id>
thurbox-cli automation edit <id> --prompt "..." --disabled
thurbox-cli automation remove <id>
thurbox-cli automation run <id>          # mark due for the next tick
thurbox-cli automation runs <id> --limit 20   # run history
thurbox-cli automation tick              # fire all due automations now
```

`--trigger` accepts `hourly`, `daily`, `weekdays`, `weekly`,
`cron:"<expr>"`, or `at:<unix_millis>`. A `--session` makes it a
*send* automation; a `--repo` (with optional `--worktree` / `--base`
/ `--agent`) makes it a *spawn* automation. `automation tick` is the
headless entry point the tmux heartbeat keeper and any
systemd/cron timer call to fire due automations without a TUI.

### Tasks (alias `todo`)

The built-in todo list (see [Tasks](#tasks) for the model).

```bash
thurbox-cli task create --title "audit deps" --description "markdown notes"
thurbox-cli task list
thurbox-cli task show <id>
thurbox-cli task edit <id> --status done   # --description "" clears notes
thurbox-cli task remove <id>
thurbox-cli task run <id>                   # trigger its Send/Spawn action
```

`create` with neither `--session` nor `--repo` is a plain local todo;
adding either (with optional `--worktree`/`--base`/`--agent`) connects
it to a coding agent like an automation.

### Messages (alias `msg`)

The [inter-session message queue](#inter-session-messages).

```bash
thurbox-cli message send --to flow --kind questions --body "scope?"
thurbox-cli message reply <message_id> --body "go ahead"
thurbox-cli message inbox --for flow --claim   # atomic, exactly-once drain
thurbox-cli message prune --older-than-days 14
```

Run *inside* a session, `send`/`reply`/`inbox` default their sender,
task, and recipient to the caller's injected identity
(`THURBOX_SESSION` / `THURBOX_TASK`), so an agent passes no ids. `send`
and `reply` wake the recipient by default (`--no-wake` to suppress).

### Extensions (alias `ext`)

Manage opt-in add-ons (see [Extensions](#extensions) for the model).
Every subcommand prints a JSON result with a human-readable `summary`.

```bash
thurbox-cli extension install <name|url|dir>   # install + activate
thurbox-cli extension list                     # installed, with staleness flags
thurbox-cli extension available [query]        # built-ins, with install commands
thurbox-cli extension status <name>            # one extension's health
thurbox-cli extension update [--all] [--force] # refresh payload (no name => all)
thurbox-cli extension reinstall <name>         # clean-slate uninstall + install
thurbox-cli extension activate <name>          # (re)create its sessions/automations
thurbox-cli extension deactivate <name>        # tear them down (real off-switch)
thurbox-cli extension uninstall <name> [--purge]
```

A bare `<name>` resolves against the official source, **pinned to your
binary's release tag** so the fetched extension matches the binary;
a URL or local dir installs from there instead. Installed extensions
**self-heal** — their declared sessions and automations are re-ensured
at TUI startup and on every headless `automation tick`, so `deactivate`
(not deleting the session) is the way to turn one off.

### Editor

```bash
thurbox-cli editor get                   # print configured command
thurbox-cli editor set "code --wait"     # set (empty string clears)
```

This is the command `Ctrl+O` runs in the TUI; the worktree path is
appended as the final argument.

### Config

```bash
thurbox-cli config validate              # strict-parse every config file (exit 1 on a problem)
thurbox-cli config show                  # print the effective resolved config
```

`validate` is handy in dotfiles CI; see
[docs/CONFIG.md](docs/CONFIG.md) for every config file in one place.

## Architecture

Thurbox follows **The Elm Architecture** (TEA):
`Event → Message → update(model, msg) → view(model) → Frame`.
All state lives in a single `App` model. Sessions run via a
`SessionBackend` trait backed by local tmux (`tmux -L thurbox`).
Terminal output is parsed by `vt100::Parser` and rendered by
`tui_term`. All persistent state (sessions, worktrees, automations)
is stored in SQLite.

### Module Dependency Rules

```text
session  ← pure data types, no local imports
agent    ← imports session only (NEVER ui or git)
ui       ← imports session only (NEVER agent or git)
app      ← coordinator, imports all modules
```

These rules are enforced by `tests/architecture_rules.rs`.
For the full set of architectural decisions with rationale,
see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Documentation

- [docs/CONSTITUTION.md](docs/CONSTITUTION.md) — Core principles
  and non-negotiable rules
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — Architectural
  decisions with rationale
- [docs/FEATURES.md](docs/FEATURES.md) — Feature-level design
  choices (including agent definitions)
- [docs/CONFIG.md](docs/CONFIG.md) — Every config file, env var,
  and DB setting in one place (settings.toml, feature flags, …)

## Development

### Setup

```bash
git clone https://github.com/Thurbeen/thurbox.git
cd thurbox
prek install   # Install pre-commit hooks
```

All required dev tools are documented in `Cargo.toml` under
`[package.metadata.dev-tools]`. Run `./scripts/install-dev-tools.sh`
to install them, or install individually with `cargo install`.

### Build and Run

```bash
cargo build                          # Debug build
cargo build --release                # Release build (LTO, stripped)
cargo run                            # Run in dev mode
```

### Testing

```bash
cargo nextest run --all              # Run all tests (preferred)
cargo nextest run -E 'test(name)'    # Single test by name
cargo test --test architecture_rules # Architecture validation
bats scripts/install.bats            # Install script tests
```

### Code Quality

```bash
cargo fmt --all                      # Format (100 char max)
cargo clippy --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
rumdl check .                        # Markdown lint
```

### Architecture Checks

```bash
cargo test --test architecture_rules # Module dependency rules
cargo deny check advisories          # Security advisories
cargo deny check bans licenses sources  # Dependency policy
```

## Committing Changes

This project uses
[Conventional Commits](https://www.conventionalcommits.org/).

```bash
cog commit feat "add worktree management"
cog commit fix "resolve memory leak" cli
```

### Commit Types

- `feat`: New features (minor version bump)
- `fix`: Bug fixes (patch version bump)
- `docs`, `refactor`, `test`, `chore`, `perf`, `ci`, `style`,
  `build`, `revert`: No release

### Valid Scopes

`api`, `cli`, `ui`, `git`, `core`, `docs`, `deps`, `config`, `mcp`

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes following our coding standards
4. Write tests for new functionality
5. Ensure all tests pass: `cargo nextest run --all`
6. Use conventional commits: `cog commit <type> "message"`
7. Submit a pull request

### Code Style

- Follow Rust naming conventions
- Maximum line width: 100 characters
- Use `rustfmt` for formatting
- Address all `clippy` warnings

## License

This project is licensed under the MIT License - see the
[LICENSE](LICENSE) file for details.

## Acknowledgments

- [Ratatui](https://github.com/ratatui-org/ratatui) — TUI
  framework
- [tui-term](https://github.com/a-kenji/tui-term) — terminal
  widget for ratatui
- [vt100](https://github.com/doy/vt100-rust) — terminal
  emulation
- [Claude Code CLI](https://github.com/anthropics/claude-code)
  — one of the supported coding agents
- [tmux](https://github.com/tmux/tmux) — terminal multiplexer

## Support

For issues, questions, or contributions, please visit our
[GitHub repository](https://github.com/Thurbeen/thurbox).
