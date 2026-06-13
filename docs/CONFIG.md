# Configuration Reference

Every knob thurbox reads, where it lives, and how it behaves. One file
per audience/lifecycle: hand-edited registries are TOML, the
machine-written keybindings are JSON, and concurrently-written runtime
state lives in SQLite (see ADR-8/ADR-19 in `ARCHITECTURE.md` for the
rationale).

Dev builds (version `0.0.0-dev`) use `thurbox-dev` in place of
`thurbox` in every path below, plus a `thurbox-dev` tmux socket, so a
development checkout never touches your real setup.

## Files at a glance

| File | Format | Edited by | Read | Purpose |
|------|--------|-----------|------|---------|
| `~/.config/thurbox/agents.toml` | TOML | you | **live** (mtime poll) | coding-agent CLI definitions |
| `~/.config/thurbox/hosts.toml` | TOML | you | startup | remote SSH hosts |
| `~/.config/thurbox/settings.toml` | TOML | you | startup | tuning knobs + feature flags |
| `~/.config/thurbox/themes.toml` | TOML | you | startup | custom theme palettes |
| `~/.config/thurbox/keybindings.json` | JSON | F1 editor (or you) | **live** (mtime poll) | key chord overrides |
| `~/.config/thurbox/extensions/<name>.toml` | TOML | `thurbox-cli extension install` | startup + tick | extension manifests (self-healed resources) |
| `~/.local/share/thurbox/thurbox.db` | SQLite | thurbox | live | sessions, automations, tasks, theme, editor command |
| `~/.local/share/thurbox/thurbox.log` | text | thurbox | — | logs (incl. config warnings) |

`agents.toml` and `keybindings.json` reload **live**: the TUI polls
their mtime (~1/s) and applies edits with a confirmation toast — no
restart. `hosts.toml` (SSH backends register at startup),
`settings.toml`, and `themes.toml` need a restart.

All paths respect `$XDG_CONFIG_HOME` / `$XDG_DATA_HOME`.

Config problems are **not silent**: parse errors, unknown fields,
invalid chords, and chord conflicts surface as a status-bar toast on
startup (and in the log file). Unknown TOML keys are tolerated —
stale keys from older versions or typos are *reported by name* but
your file still loads — while syntax/type errors fall back to
built-ins (agents), zero hosts, or defaults (settings).

Check everything from the command line:

```bash
thurbox-cli config validate   # strict parse of every file; exit 1 on problems
thurbox-cli config show       # effective config + where each value came from
```

`validate` fails on unknown keys (they are typos or leftovers either
way), making it usable as a dotfiles CI gate.

## agents.toml

Declares the launchable coding agents. Seeded with the built-ins
(`claude`, `codex`, `gemini`, `opencode`, `aider`, `vibe`) on first
run; edit or add `[[agents]]` entries to support any CLI — no
recompile. Malformed file → built-ins are used and the error is shown.

```toml
config_version = 1
default = "claude"          # agent preselected in the picker / headless spawns

[[agents]]
name = "claude"             # display + lookup name (unique)
command = "claude"          # executable
args = []                   # always passed; bake a model here if you want one
resume_args = ["--resume", "{id}"]            # emitted when resuming
fork_args = ["--resume", "{id}", "--fork-session"]
new_session_args = ["--session-id", "{id}"]   # emitted on a fresh spawn
resume_latest = false       # true = id-less "resume last session in cwd"
```

`{id}` is substituted with the thurbox-generated session UUID. Groups
are emitted only when their driving value exists; precedence is
fork > resume > new-session. See the seeded file's comments and
CLAUDE.md's *Agent Definitions* section for the `resume_latest`
semantics.

## hosts.toml

Declares remote SSH hosts; each `[[hosts]]` entry registers a session
backend named `ssh:<name>`. Seeded fully commented-out (fresh installs
are local-only). Malformed file → zero remote hosts, error shown.

| Field | Required | Default | Purpose |
|-------|----------|---------|---------|
| `name` | yes | — | backend id `ssh:<name>`; what `--host` expects |
| `destination` | yes | — | ssh target (`user@host` or `~/.ssh/config` alias) |
| `ssh_opts` | no | `[]` | extra ssh flags, one token per element |
| `socket` | no | `thurbox` | remote `tmux -L` socket |
| `session` | no | `thurbox` | remote tmux session name |
| `worktrees_dir` | no | remote `$HOME/.local/share/thurbox/worktrees` | absolute remote worktrees dir |

Auth comes entirely from your `~/.ssh/config`; thurbox never handles
credentials. Host changes require a restart (the registry is read once
and the remote `$HOME` is cached per destination for the process
lifetime).

## settings.toml

Scalar tuning knobs plus the `[features]` switches, seeded fully
commented-out (defaults apply when absent). Only knobs a user plausibly
wants are exposed; internals stay hardcoded.

| Key | Default | Purpose |
|-----|---------|---------|
| `scrollback_lines` | `1000` | terminal scrollback kept per session |
| `two_panel_min_cols` | `80` | width below which only the terminal renders |
| `three_panel_min_cols` | `120` | width unlocking the optional third column |
| `audit_retention_days` | `90` | audit-log history kept (pruned on startup) |

### `[features]` — whole-feature switches

Turn major TUI features off entirely. All default to `true`; like the
rest of settings.toml, changes need a restart. A disabled feature's
pane never renders, its keybinding shows a status toast instead of
acting, and its global-search scope returns no results. Data is never
touched, so re-enabling a flag is lossless.

| Key | Disables |
|-----|----------|
| `tasks` | tasks panel (`F5`/`Ctrl+W`) and task search results |
| `automations` | automations pane, `Ctrl+P`, TUI schedule firing, heartbeat arming |
| `file_viewer` | file viewer column (`F3`) and file search results |
| `global_search` | global search strip (`Ctrl+A`) |
| `info_panel` | info panel column (`F2`) |
| `shell_pane` | per-session shell toggle (`Ctrl+T`) |
| `mouse` | mouse capture: clicks, wheel, drag-select, hover, scrollbars |

`automations = false` is a full stop on the TUI side: the pane
disappears (the session list takes the whole left column and `j`/`k`
wrap within it), and the TUI neither fires due schedules nor arms the
tmux heartbeat keeper on startup. Explicit `thurbox-cli automation`
commands still work — and `automation create` still arms the
heartbeat, so an already-armed keeper window (or an OS timer from
`packaging/`) keeps firing schedules externally. Disabling
`shell_pane` hides existing shell panes but never kills their
processes. `mouse = false` skips terminal mouse capture entirely, so
the terminal keeps its native mouse behavior (its own text selection,
URL handling, etc.) and no click/wheel/hover handling runs in the TUI.

## themes.toml

User-defined themes, offered in the `Ctrl+Y` picker alongside the nine
built-in presets and persisted by `name` like any preset. Each
`[[themes]]` entry starts from a built-in `base` and overrides only the
colours it names:

```toml
[[themes]]
name = "my-mocha"            # stable id; must not shadow a built-in
display_name = "My Mocha"    # picker label (default: name)
base = "catppuccin-mocha"    # starting palette (default: default)
accent = "#fab387"
app_bg = "reset"             # keep the terminal's native background
```

Colours accept anything ratatui parses: `#rrggbb`, ANSI names (`red`,
`lightcyan`), indexed (`14`), or `reset`. The seeded file lists every
overridable key. Bad colours and built-in name collisions degrade to
startup warnings (the base colour / the built-in stays in effect).

## keybindings.json

Maps `Action` names to one or more chord strings:

```json
{ "QuitApp": ["ctrl+x"], "OpenThemePicker": ["ctrl+y", "f4"] }
```

- Preferred editing path is the **F1 panel** (live capture, conflict
  stealing, immediate persistence). Hand-edits are read at startup.
- Chord syntax: `[ctrl+][alt+][shift+][cmd+]<key>` where `<key>` is a
  letter, `f1`–`f12`, or a named key (`enter`, `esc`, `tab`, arrows,
  `home`, `end`, `pageup`, `pagedown`, `backspace`, `delete`,
  `insert`). Case-insensitive. `cmd` (aliases `super`, `command`,
  `win`) is the macOS Command key — delivered only by
  kitty-keyboard-protocol terminals (iTerm2 3.5+, kitty, WezTerm,
  Ghostty; not Terminal.app), and only for chords the emulator
  doesn't claim itself.
- Unknown action names, invalid chords, and the same chord bound to two
  actions in overlapping contexts are reported at startup (the file
  still loads; bad entries fall back to defaults).
- Action names and defaults: see the table in CLAUDE.md / README, or
  `src/session/keybindings.rs`.

## extensions/

Each opt-in extension (see `extensions/<name>/`) is described by a single
`extension.toml` manifest. `thurbox-cli extension install` writes the
home-resolved copy to `~/.config/thurbox/extensions/<name>.toml` (thurbox
never seeds this dir). The manifest has two halves — an **install** spec
and a **runtime** spec:

```toml
name = "flow"
description = "Focus-protecting triage agent"
config_version = 1
home = "~/flow"                 # install home; {home} is substituted everywhere

# install spec ---------------------------------------------------------------
[[agents]]                      # registered in agents.toml (existing kept)
name = "flow"
command = "claude"
args = ["--model", "claude-haiku-4-5"]

[[files]]                       # fetched from the source, written under home
path = "FLOW.md"
[[files]]
path = "scripts/create-task.sh"
executable = true               # chmod +x
[[files]]
path = "repos.md"
if_absent = true                # seed once; never clobbered on reinstall
[[files]]
path = ".claude/settings.json"
source = "claude-settings.json" # source path differs from dest
substitute = true               # replace {home} in the content

[[symlinks]]                    # never clobbers a real file at `link`
link = "CLAUDE.md"
target = "FLOW.md"

# runtime spec (ensured on activate, self-healed if deleted) -----------------
[[sessions]]
name = "flow"
agent = "flow"
repo_path = "{home}"            # resolved to the absolute home at install

[[automations]]
name = "flow-tick"
trigger = "cron:*/5 * * * *"    # same grammar as `automation create --trigger`
session_ref = "flow"           # must match a [[sessions]] name above
prompt = "tick"
```

Manage extensions with the CLI:

```bash
thurbox-cli extension install flow         # fetch + lay files + agents + activate
thurbox-cli extension install ./extensions/flow   # from a local dir
thurbox-cli extension install <url> --home ~/x    # from a URL, custom home
thurbox-cli extension uninstall <name>     # reverse install (keep home dir)
thurbox-cli extension uninstall <name> --purge    # also delete the home dir
thurbox-cli extension list                 # installed + active/healthy state
thurbox-cli extension activate <name>      # (re)create resources + mark active
thurbox-cli extension deactivate <name>    # tear down + stop self-heal
thurbox-cli extension deactivate <name> --force --purge  # also kill tmux + drop manifest
thurbox-cli extension status [<name>]      # per-resource presence
```

A bare name installs from the official source
(`raw.githubusercontent.com/Thurbeen/thurbox/<ref>/extensions/<name>`,
fetched via curl/wget) — `<ref>` is the running binary's release tag
(`main` for dev builds), so a fetched extension matches your binary. A
path or `http(s)://` URL installs from there instead. Payload paths are
validated against traversal (no absolute paths or `..`), and a
`substitute` file you've edited isn't overwritten on reinstall (use
`--force`). Payload files are fetched as **text** (specs/scripts/JSON),
not binaries.

While an extension is **active**, thurbox **self-heals** its declared
resources: on TUI startup and on every `automation tick` it re-creates
any session/automation that has been deleted. So deleting them by hand is
a no-op (they come back); `extension deactivate` is the real off-switch.
Self-heal while the TUI is closed depends on the automation heartbeat
(`[features] automations = true`); with automations off, healing happens
at the next TUI startup only.

## SQLite-backed settings

Live in the `metadata` table and apply immediately (no restart):

| Key | Set via | Purpose |
|-----|---------|---------|
| `active_theme` | `Ctrl+Y` / `F4` picker | TUI palette (nine built-ins) |
| `editor_command` | `thurbox-cli editor set "<cmd>"` | what `Ctrl+O` runs |
| `active_extensions` | `thurbox-cli extension activate/deactivate` | JSON array of active extensions to self-heal |

These are in the DB rather than a file because they are written
concurrently by multiple thurbox processes (TUI, CLI, MCP) and picked
up live via `PRAGMA data_version` polling.

## Environment variables

| Variable | Used for |
|----------|----------|
| `XDG_CONFIG_HOME`, `XDG_DATA_HOME` | config/data roots |
| `VISUAL`, then `EDITOR` | `Ctrl+O` editor when `editor_command` is unset |
| `SHELL` | the `Ctrl+T` companion shell pane (fallback `/bin/sh`) |
| `RUST_LOG` | log filter for `thurbox.log` |

Editor resolution order: DB `editor_command` → `$VISUAL` → `$EDITOR` →
error toast.

## Versioning

The SQLite schema migrates automatically (`schema_version` in
`metadata`). The TOML files carry a `config_version = 1` marker so a
future format change can migrate them too; current files are version 1
and the field is optional.
