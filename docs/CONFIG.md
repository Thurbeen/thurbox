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
| `~/.config/thurbox/settings.toml` | TOML | you + `Ctrl+,` panel | **live** (feature flags) / startup (rest) | tuning knobs + feature flags |
| `~/.config/thurbox/themes.toml` | TOML | you | startup | custom theme palettes |
| `~/.config/thurbox/keybindings.json` | JSON | F1 editor (or you) | **live** (mtime poll) | key chord overrides |
| `~/.config/thurbox/extensions/<name>.toml` | TOML | `thurbox-cli extension install` | startup + tick | extension manifests (self-healed resources) |
| `~/.local/share/thurbox/thurbox.db` | SQLite | thurbox | live | sessions, automations, tasks, theme, editor command |
| `~/.local/share/thurbox/thurbox.log` | text | thurbox | — | logs (incl. config warnings) |

`agents.toml`, `keybindings.json`, and `settings.toml` reload **live**:
the TUI polls their mtime (~1/s) and applies edits with a confirmation
toast — no restart. For `settings.toml` only the **feature flags that
gate UI panels** (`tasks`, `file_viewer`, `info_panel`, `global_search`,
`shell_pane`, `soft_delete`) apply live; the restart-only values stay
published through a write-once global (so they can't drift mid-frame),
and the reload toast says when a restart is needed. `hosts.toml` (SSH
backends register at startup) and `themes.toml` need a restart.

`settings.toml` can also be edited from the TUI: **`Ctrl+,`** (alt `F6`)
opens a **Settings panel** listing every knob. It writes the file back
**preserving its comments**, and feature flags that gate UI panels apply
**live** on save; the rest (`mouse`, `notifications`, `automations`,
`version_check`, the `[notifications]` knobs, and the scalars) take
effect on the next launch — the panel marks those rows with `⟳` and
toasts a restart note. Hand-editing the file (or the panel in another
instance) is picked up the same way, via the live mtime poll.

All paths respect `$XDG_CONFIG_HOME` / `$XDG_DATA_HOME`.

### Which file do I edit?

A task-to-file map so you don't have to scan every section to find
the right knob:

| I want to… | Edit | Section |
|------------|------|---------|
| Add a coding agent, pin a model, change resume/fork flags | `agents.toml` | [agents.toml](#agentstoml) |
| Run sessions on a remote machine over SSH | `hosts.toml` | [hosts.toml](#hoststoml) |
| Turn a whole TUI feature on/off (tasks, mouse, notifications…) | `settings.toml` `[features]` | [`[features]`](#features--whole-feature-switches) |
| Tune scrollback, panel breakpoints, audit retention | `settings.toml` | [settings.toml](#settingstoml) |
| Change when/how OS notifications fire | `settings.toml` `[notifications]` | [`[notifications]`](#notifications--os-notification-settings) |
| Add or recolour a TUI theme | `themes.toml` | [themes.toml](#themestoml) |
| Rebind a key | `keybindings.json` (or the F1 editor) | [keybindings.json](#keybindingsjson) |
| Set the `Ctrl+O` editor, pick a theme | (runtime — SQLite) | [SQLite-backed settings](#sqlite-backed-settings) |

None of these files need to exist on a fresh install — every one is
seeded (commented-out where applicable) on first run, and absent files
fall back to built-in defaults.

Config problems are **not silent**: parse errors, unknown fields,
invalid chords, and chord conflicts surface as a status-bar toast on
startup (and in the log file). Unknown TOML keys are tolerated —
stale keys from older versions or typos are *reported by name* but
your file still loads — while syntax/type errors fall back to
built-ins (agents), zero hosts, or defaults (settings).

`agents.toml` degrades **per entry**: a single malformed `[[agents]]`
block (e.g. `args` given a string instead of an array) is skipped with
a toast naming it, and your remaining agents still load — only a
document-level syntax error (or a file with no usable agents) falls
back to the built-ins.

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
recompile. A malformed `[[agents]]` entry is skipped (with a toast
naming it) and the rest still load; only a document-level syntax error
falls back to the built-ins. Either way the error is shown.

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

The seeded file also ships two commented, copy-pasteable templates
below the built-ins — **Add your own agent** (every field annotated)
and **Pin a model** (a `claude-opus` variant baking `--model opus`
into `args`). Both stay commented, so a fresh install still resolves
to exactly the six built-ins.

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
wants are exposed; internals stay hardcoded. The seed closes with a
**Common recipes** block — copy-pasteable groupings (bigger scrollback,
a minimal/focused TUI, notification tuning, enabling the update badge),
all commented so defaults still apply out of the box.

| Key | Default | Purpose |
|-----|---------|---------|
| `scrollback_lines` | `1000` | terminal scrollback kept per session |
| `two_panel_min_cols` | `80` | width below which only the terminal renders |
| `three_panel_min_cols` | `120` | width unlocking the optional third column |
| `audit_retention_days` | `90` | audit-log history kept (pruned on startup) |

A complete `settings.toml` showing every knob at its default — copy
this, uncomment what you want to change, and restart:

```toml
config_version = 1

# Scalar tuning knobs (top level)
scrollback_lines      = 1000   # terminal scrollback kept per session
two_panel_min_cols    = 80     # width below which only the terminal renders
three_panel_min_cols  = 120    # width unlocking the optional third column
audit_retention_days  = 90     # audit-log history kept (pruned on startup)

[features]
tasks         = true
automations   = true
file_viewer   = true
global_search = true
info_panel    = true
shell_pane    = true
mouse         = true
notifications = true
version_check = false          # opt-in: makes a network call
auto_update   = false          # opt-in: downloads + replaces binaries

[notifications]
also_on_waiting     = false    # also fire on Busy → Waiting (no bell)
suppress_for_active = true     # skip the session you're currently viewing
sound               = true     # play the OS default notification sound
min_interval_secs   = 5        # per-session floor between notifications
```

### `[features]` — whole-feature switches

Turn major TUI features off entirely. All default to `true` **except
`version_check` and `auto_update`, which default to `false`** (both
reach the network, so they are opt-in). Like the rest of settings.toml,
changes need a
restart. A disabled feature's pane never renders, its keybinding shows
a status toast instead of acting, and its global-search scope returns
no results. Data is never touched, so re-enabling a flag is lossless.

| Key | Default | Controls |
|-----|---------|----------|
| `tasks` | `true` | tasks panel (`F5`/`Ctrl+W`) and task search results |
| `automations` | `true` | automations pane, `Ctrl+P`, TUI schedule firing, heartbeat arming |
| `file_viewer` | `true` | file viewer column (`F3`) and file search results |
| `global_search` | `true` | global search strip (`Ctrl+/`) |
| `info_panel` | `true` | info panel column (`F2`) |
| `shell_pane` | `true` | per-session shell toggle (`Ctrl+T`) |
| `mouse` | `true` | mouse capture: clicks, wheel, drag-select, hover, scrollbars |
| `notifications` | `true` | OS desktop notifications when a session needs attention |
| `soft_delete` | `true` | TUI `Ctrl+D` soft-deletes (Ctrl+Z undo); off = hard delete after a confirmation prompt |
| `version_check` | `false` | GitHub update check: TUI header "update available" badge + `thurbox-cli version --check` |
| `auto_update` | `false` | Silent self-update: download + verify + replace the binaries on startup + `thurbox-cli update` |

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
`notifications = false` keeps the background dispatcher thread from
ever starting (zero overhead) and silently no-ops every transition;
the session status display itself is unaffected.

`soft_delete = false` turns the TUI's `Ctrl+D` into a destructive
**hard delete**: instead of marking the row deleted with a `Ctrl+Z`
undo window, it kills the session's tmux window, removes its worktrees
and symlink workspace, and disables any pending `Send` automations —
after a confirmation prompt (`Enter`/`y` to delete, `Esc`/`n` to
cancel), since the teardown is irreversible. The soft-deleted row is
still written last, so the session remains restorable via `Ctrl+U`
(which re-spawns it fresh). This flag governs the TUI only:
`thurbox-cli session delete` always soft-deletes unless you pass
`--force`, regardless of the setting.

`version_check = true` enables the update check (default `false`, since
it makes a network call). On launch the TUI reads a cached result
(`~/.local/share/thurbox/version-check.json`) and, if it is older than
24 h, fires a single best-effort background fetch of GitHub's latest
release (`api.github.com/repos/Thurbeen/thurbox/releases/latest`, via
`curl`/`wget` — no new dependency); a newer release shows a `⬆ vX.Y.Z
available` badge next to the version in the header. The fetch never runs
on the render path and never blocks startup; failures are silent. Dev
builds (`0.0.0-dev`) never show the badge. The same flag enables
`thurbox-cli version --check`, which fetches fresh on demand and reports
current vs. latest (`thurbox-cli version` with no flag always prints the
current version, regardless of the flag).

`auto_update = true` goes a step further than `version_check`: instead of
just showing a badge, the TUI **silently updates itself** on startup. After
the same 24 h cache check, if a newer release exists it downloads that
release's tarball + checksums from GitHub Releases (`curl`/`wget`, no new
dependency), verifies the SHA256 (`sha256sum`/`shasum`), extracts it
(`tar`), and atomically replaces the installed `thurbox`/`thurbox-cli`
binaries in place — mirroring `scripts/install.sh`. The download is verified
**before** any installed file is touched, so a failed/corrupt download leaves
the current binaries untouched; the whole step runs before the TUI takes the
terminal and is best-effort (any failure is logged and startup continues on
the current version). The replaced binary takes effect on the **next launch**
(the running process keeps its open file), so the TUI shows an "Updated to
vX.Y.Z — restart to apply" status line. `thurbox-cli update` performs the
same update on demand (with `--force` to bypass the up-to-date and dev-build
guards); dev builds (`0.0.0-dev`) never auto-update. The default install
location (`~/.local/bin`) is user-writable; a system-wide install in a
root-owned directory will fail the replace (logged, non-fatal). `version_check`
and `auto_update` are independent — enable either or both.

### `[notifications]` — OS notification settings

Surfaces an OS notification when a session crosses into a state that
needs the user's attention (the agent rang the terminal bell or emitted
an OSC 9 / OSC 777 message — usually because it's waiting on an answer
or has finished a task). Linux dispatches via dbus and supports
**click-to-focus**: clicking the banner writes a focus request that the
running TUI reads on its next tick and switches to that session. macOS
shows the notification banner but ignores clicks (the modern
`UNUserNotificationCenter` API requires a signed app bundle, which
thurbox is not). On both platforms the notification body is the agent's
last OSC message when present, otherwise `Waiting for input`. **Only
fires while the TUI is open** — the agent terminal parser is what sees
the bell, and it doesn't run when thurbox isn't.

| Key | Default | Purpose |
|-----|---------|---------|
| `also_on_waiting` | `false` | also fire on `Busy → Waiting` (no explicit bell from the agent) |
| `suppress_for_active` | `true` | skip the notification for the session you're currently viewing |
| `sound` | `true` | play the OS default notification sound |
| `min_interval_secs` | `5` | per-session floor between two notifications (dedup) |

The default-on `Attention` trigger is the right knob for any agent that
respects the terminal bell / OSC 9 / OSC 777 conventions (Claude Code
out of the box, for example). For agents that only go quiet without
ringing a bell, set `also_on_waiting = true` — note this fires once each
time the agent goes idle after activity, so it can be chatty.

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
- **Terminal passthrough.** When a session **terminal is focused**, the
  readline / shell line-editing chords (`Ctrl+A` start-of-line, `Ctrl+E`
  end-of-line, `Ctrl+W` delete-word, `Ctrl+U` kill-line, `Ctrl+R`
  reverse-search, `Ctrl+D` EOF, plus `Ctrl+B/F/O/P/S`) are **forwarded to the
  agent CLI** instead of triggering their thurbox command, so your terminal
  muscle memory works inside a session. Those thurbox commands stay reachable
  from the **session list** (focus it with `Ctrl+H`) and via their `F`-key
  alternates (`F2` info panel, `F3` file viewer, `F5` tasks). Rebinding such an
  action to a key that isn't a bare `Ctrl+<letter>` makes it work in the
  terminal too. Navigation/quit chords (`Ctrl+H/J/K/L`, `Ctrl+Q`, `Ctrl+N`) are
  **never** forwarded — they're how you leave the terminal.
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
config_version = 1              # manifest *format* version (for migrations)
version = "1.0.0"              # the extension's own version (bumped by its author)
min_thurbox_version = "0.113.0" # minimum thurbox; older binaries get a warning
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

# [[automations]] is an OPTIONAL runtime resource (flow itself ships none —
# it is purely event-driven). An extension that wants a scheduled tick declares:
[[automations]]
name = "example-tick"
trigger = "cron:*/10 * * * *"   # same grammar as `automation create --trigger`
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
thurbox-cli extension list                 # installed + active/healthy + version/stale
thurbox-cli extension update <name>        # re-fetch from recorded source (refresh)
thurbox-cli extension update --all         # update every installed extension
thurbox-cli extension update <name> --force # also overwrite user-edited seed files
thurbox-cli extension activate <name>      # (re)create resources + mark active
thurbox-cli extension deactivate <name>    # tear down + stop self-heal
thurbox-cli extension deactivate <name> --force --purge  # also kill tmux + drop manifest
thurbox-cli extension status [<name>]      # per-resource presence + version/stale
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

### Versioning + the update lifecycle

Extensions carry two version markers, and the installer stamps two more
into the discovery-dir copy so staleness can be detected:

| Field | Where set | Purpose |
|-------|-----------|---------|
| `version` | source manifest | the extension's own semver (author-bumped) |
| `min_thurbox_version` | source manifest | minimum thurbox; older binaries warn |
| `installed_with` | stamped on install | the thurbox version that installed it |
| `source` | stamped on install | the target it was installed from |

A **bare-name** install (`extension install flow`) fetches from the
official source **pinned to the running binary's release tag**, so the
extension you get always matches your thurbox. When you later **upgrade
thurbox**, the on-disk copy is now older than the binary — thurbox
flags it as `stale` (in `extension list`/`status`, and as a one-line
nudge from self-heal at startup). Run `extension update <name>` (or
`--all`) to re-fetch from the recorded `source`; because a bare name
re-resolves against the *new* binary's tag, this pulls the version that
matches your upgraded thurbox. Updates honour the same file rules as
install — user-edited `substitute` files and `if_absent` seeds are
preserved unless you pass `--force`.

`min_thurbox_version` is a **soft** gate: an extension authored for a
newer thurbox still installs on an older binary, but install/activate and
self-heal emit a compatibility warning so the mismatch is visible.
**Dev builds** (`0.0.0-dev`) skip both the staleness and compatibility
checks — their version doesn't order against release tags.

**Rollback.** There's no version snapshot store: to roll an extension
back, pin a specific thurbox tag — `extension install
https://raw.githubusercontent.com/Thurbeen/thurbox/v0.112.0/extensions/flow`
— or downgrade the binary and run `extension update`, which re-resolves
the bare name to that older tag.

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
