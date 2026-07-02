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
| `~/.config/thurbox/hosts.toml` | TOML | you | startup | remote SSH hosts + local WSL distros |
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
`shell_pane`, `code_review`, `soft_delete`) apply live; the restart-only values stay
published through a write-once global (so they can't drift mid-frame),
and the reload toast says when a restart is needed. `hosts.toml` (SSH
backends register at startup) and `themes.toml` need a restart.

`settings.toml` can also be edited from the TUI: **`Ctrl+,`** (alt `F6`)
opens a **Settings panel** listing every knob. It writes the file back
**preserving its comments**, and feature flags that gate UI panels apply
**live** on save; the rest (`mouse`, `notifications`, `automations`,
`version_check`, `auto_update`, the four editable `[notifications]` knobs,
and the scalars) take effect on the next launch — the panel marks those rows
with `⟳` and toasts a restart note. The panel exposes only the four
editable notification knobs (`also_on_waiting`, `suppress_for_active`,
`sound`, `min_interval_secs`); `[notifications] backend` is **not** in
the panel — set it only by hand-editing `settings.toml`. Hand-editing
the file (or the panel in another instance) is picked up the same way,
via the live mtime poll.

All paths respect `$XDG_CONFIG_HOME` / `$XDG_DATA_HOME`.

### Which file do I edit?

A task-to-file map so you don't have to scan every section to find
the right knob:

| I want to… | Edit | Section |
|------------|------|---------|
| Add a coding agent, pin a model, change resume/fork flags | `agents.toml` | [agents.toml](#agentstoml) |
| Run sessions on a remote machine over SSH, or in a local WSL distro | `hosts.toml` | [hosts.toml](#hoststoml) |
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
(`claude`, `codex`, `antigravity`, `opencode`, `aider`, `copilot`, `vibe`) on
first run; edit or add `[[agents]]` entries to support any CLI — no
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
to exactly the seven built-ins.

## hosts.toml

Declares off-local hosts — **remote SSH machines** and **local WSL
distros**. Each `[[hosts]]` entry registers a session backend named
`ssh:<name>` (default kind) or `wsl:<name>`. Seeded fully commented-out
(fresh installs are local-only for SSH). Malformed file → zero
configured hosts, error shown.

| Field | Required | Default | Purpose |
|-------|----------|---------|---------|
| `name` | yes | — | backend id `ssh:<name>` / `wsl:<name>`; what `--host` expects |
| `kind` | no | `ssh` | transport: `ssh` (remote machine) or `wsl` (local distro) |
| `destination` | for ssh | — | ssh target (`user@host` or `~/.ssh/config` alias) |
| `distro` | no | `name` | WSL distro name (`kind = "wsl"` only) |
| `ssh_opts` | no | `[]` | extra ssh flags, one token per element (ssh only) |
| `socket` | no | `thurbox` | host `tmux -L` socket |
| `session` | no | `thurbox` | host tmux session name |
| `worktrees_dir` | no | host `$HOME/.local/share/thurbox/worktrees` | absolute worktrees dir on the host/distro |
| `multiplexer` | no | `tmux` | host multiplexer binary; set to `psmux` for a Windows SSH host |

**SSH** auth comes entirely from your `~/.ssh/config`; thurbox never
handles credentials. **WSL** distros are reached with
`wsl.exe -d <distro>` and need no config entry at all — on Windows they
are **auto-discovered** (`wsl.exe -l -q`) and appear in the host picker
and `--host` automatically; add a `kind = "wsl"` entry only to override
a default (e.g. `worktrees_dir`). For both kinds, tmux, git, the agent,
and worktrees all run **on the host / inside the distro** at native
paths (a WSL distro's worktrees live in its own Linux filesystem, not on
`/mnt/c`); the distro needs `tmux` >= 3.2 and `git`. Host changes
require a restart (the registry is read once and each host's `$HOME` is
cached for the process lifetime).

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
code_review   = true
mouse         = true
notifications = true
version_check = false          # opt-in: makes a network call
auto_update   = false          # opt-in: downloads + replaces binaries

[notifications]
also_on_waiting     = false    # also fire when a session finishes (Working → Done)
suppress_for_active = true     # skip the session you're currently viewing
sound               = true     # play the OS default notification sound
min_interval_secs   = 5        # per-session floor between notifications
```

### `[features]` — whole-feature switches

Turn major TUI features off entirely. All default to `true` **except
`version_check` and `auto_update`, which default to `false`** (both
reach the network, so they are opt-in). The UI-panel flags
(`tasks`, `file_viewer`, `info_panel`, `global_search`, `shell_pane`,
`code_review`, `soft_delete`) apply **live** on save; the rest
(`automations`, `mouse`, `notifications`, `version_check`, `auto_update`)
take effect on the next launch.
A disabled feature's pane never renders, its keybinding shows
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
| `code_review` | `true` | native code-review view (diff + comments, `Ctrl+X`) |
| `mouse` | `true` | mouse capture: clicks, wheel, drag-select, hover, scrollbars |
| `notifications` | `true` | OS desktop notifications when a session needs attention |
| `soft_delete` | `true` | TUI `Ctrl+D` soft-deletes (Ctrl+Z undo); off = hard delete after a confirmation prompt |
| `version_check` | `false` | GitHub update check: TUI header "update available" badge + `thurbox-cli version --check` |
| `auto_update` | `false` | Silent self-update: download + verify + replace the binaries on startup + `thurbox-cli update`; also auto-refreshes stale extensions |

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
just showing a badge, the TUI **silently updates itself** on startup. On every
launch (it does **not** reuse the `version_check` badge's 24 h cache — sharing
that gate let the badge keep the cache "fresh" and starve the updater) it
fetches the latest release tag; if a newer release exists it downloads that
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

`auto_update = true` also keeps **installed extensions** in step with the
binary. Extension versions are pinned to the binary's release tag, so an
extension only goes stale (`installed_with` ≠ the running binary) right after an
upgrade. The self-heal pass — which already runs on TUI startup and on the
headless `automation tick` — then refreshes each stale extension in place
(re-fetching it from its recorded source) instead of only nudging you to run
`thurbox-cli extension update`. The staleness check is local and network-free,
so a launch where nothing is stale does no extra work; a refresh runs at most
once per extension per binary version. With `auto_update` off, the nudge is
shown and you update extensions by hand.

### `[notifications]` — OS notification settings

Surfaces an OS notification when a session **transitions into a state
that needs your attention**. The trigger is the hooks-driven
[session status](#session-status) (reported by the agent's hooks, *not*
the terminal bell / output): the notification fires when a session crosses
into `Blocked` (the agent needs input or approval) and, with
`also_on_waiting = true`, also when it finishes a turn (`Working → Done`).
The edge is observed once per tick in `refresh_session_statuses` — the
same place the session-list status icon is computed, so the banner can
never drift from the list — then deduped per session (`min_interval_secs`)
and skipped for the session you're currently viewing (`suppress_for_active`).
The notification body is the agent's last OSC 9 / OSC 777 message when
present (truncated to 200 chars), otherwise `Waiting for input` — the OSC
message is kept only for the body text, no longer for the trigger.
**Only fires while the TUI is open** — the dispatcher thread runs only
inside the TUI, so a headless `automation tick` never notifies.

**Delivery backend** (`backend`, default `auto`) is detected at startup:

| Backend | When | Click-to-focus |
|---------|------|----------------|
| `dbus` | normal Linux desktop with a running notification daemon (`org.freedesktop.Notifications`) | **yes** — clicking the banner writes a focus request the running TUI reads next tick and switches to that session |
| `windows` (toast) | **native Windows**, or **WSL** / any Linux with no dbus daemon — delivers a Windows toast via `powershell.exe` (WSL needs interop, on by default) | no (a Windows toast can't call back into the thurbox process) |
| `macos` | macOS native banner. Uses **`terminal-notifier`** when it's in `PATH` (own bundle + icon, looks like a real app notification — `brew install terminal-notifier`), otherwise the built-in **`osascript`** `display notification` (attributed to Apple's Script Editor). The `UNUserNotificationCenter` click API needs a signed `.app` bundle, which thurbox is not, so the `osascript` path is informational | **with `terminal-notifier`** — clicking the banner runs `thurbox-cli session focus <id>`, which writes the same metadata row the dbus path does and the TUI picks it up next tick. Without `terminal-notifier` (osascript fallback): no |

`auto` prefers `dbus` whenever a daemon answers, and only falls back to
the Windows toast when no dbus service is reachable. Force a specific
path or disable delivery with `backend = "dbus" | "windows" | "off"`
(`off` is a soft switch distinct from `[features] notifications`, which
stops the dispatcher thread entirely).

This auto-detection fixes a previously **silent failure** on WSL: the
dbus path errored on connect, but the only signal was a line in the
logfile, so the user saw nothing. Delivery errors are now recorded and
surfaced by the diagnostic:

```bash
thurbox-cli notify          # show the detected backend + last delivery error
thurbox-cli notify --test   # fire a sample notification to confirm it works
```

| Key | Default | Purpose |
|-----|---------|---------|
| `also_on_waiting` | `false` | also fire when a session finishes (`Working → Done`); the field name is historical |
| `suppress_for_active` | `true` | skip the notification for the session you're currently viewing |
| `sound` | `true` | play the OS default notification sound |
| `min_interval_secs` | `5` | per-session floor between two notifications (dedup) |
| `backend` | `auto` | delivery backend: `auto` \| `dbus` \| `windows` \| `off` |

Notifications fire on the hooks-driven status transitions (see
[Session status](#session-status)): always on `→ Blocked` (the agent needs
you), and with `also_on_waiting = true` also on `Working → Done`.

## Session status

Each session's state (Blocked / Working / Done / Idle / Error) is driven by
**agent hooks** that call `thurbox-cli session signal --state
<working|blocked|done|idle>`. The state is persisted on the `sessions` row
(`hook_state`, `hook_state_at`, `seen_at` — schema v34) and survives the TUI
being closed; a hook fired headlessly is picked up via `PRAGMA data_version`.
Identity comes from the injected `THURBOX_SESSION` env var, so a hook passes
no id. A finished turn shows `Done` (blue) — for the session you're watching too
— and becomes `Idle` once you switch focus off it.

The hooks are wired up automatically by the built-in **hooks** extension
(auto-activated on first run). Opt out with `thurbox-cli extension deactivate
hooks`. The status colours are tunable theme keys (`status_working` /
`status_blocked` / `status_done` / `status_idle` / `status_error` — see
`themes.toml`).

The wiring is applied **only to agents thurbox launches** — it never edits your
own global agent config (e.g. your personal `~/.claude/settings.json`). thurbox's
managed hook config lives per agent, applied by injecting a flag into
`agents.toml` or by a reversible merge into / managed file in the agent's own
config dir:

| Agent | On-disk location | How it's applied |
|-------|------------------|------------------|
| claude | `~/.config/thurbox/hooks/claude.json` | `--settings` flag (claude merges it with your own settings) |
| aider | — (no file) | `--notifications-command` flag |
| opencode | `~/.config/opencode/plugin/thurbox-status.js` | managed plugin file |
| codex | `~/.codex/hooks.json` | reversible JSON-merge of thurbox's entries |
| vibe | `~/.vibe/hooks.toml` | managed file (refused if you already have one) |
| antigravity | `~/.gemini/settings.json` | reversible JSON-merge of thurbox's entries |

The home dir is `~/.config/thurbox/hooks` on a release build and
`~/.config/thurbox-dev/hooks` on a dev build. Because claude *merges* the
`--settings` file, your own hooks still fire inside a thurbox session — both run.
Hand-edits to a managed file are rewritten from the embedded payload on the next
TUI start / heartbeat tick; to customize, deactivate the extension and wire the
hook yourself, or edit the payload under `extensions/hooks/` and reinstall. Full
per-agent detail: `extensions/hooks/README.md`.

## themes.toml

User-defined themes, offered in the `Ctrl+Y` picker alongside the fifteen
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
overridable key — including the code-review diff colours `diff_added` /
`diff_removed` (added/removed line foreground) and `diff_added_bg` /
`diff_removed_bg` (the subtle full-row tint). Bad colours and built-in name
collisions degrade to startup warnings (the base colour / the built-in stays in
effect).

## keybindings.json

Maps `Action` names to one or more chord strings:

```json
{ "QuitApp": ["ctrl+a"], "OpenThemePicker": ["ctrl+y", "f4"] }
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
never seeds this dir). The install **home** (where payload files land and the
session runs) defaults to `~/.config/thurbox/extensions/<name>/` — a sibling dir
of that manifest — unless the manifest pins a `home` or you pass `--home`. The
manifest has two halves — an **install** spec and a **runtime** spec:

```toml
name = "flow"
description = "Focus-protecting triage agent"
config_version = 1              # manifest *format* version (for migrations)
version = "1.0.0"              # the extension's own version (bumped by its author)
min_thurbox_version = "0.113.0" # minimum thurbox; older binaries get a warning
# home = "~/flow"               # OPTIONAL; default is <config>/extensions/<name>.
                                # {home} is substituted everywhere it appears

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

# Reaching OUTSIDE the extension home (used by the built-in hooks extension):
[[external_files]]              # write a file into an agent's OWN config dir
path = "~/.config/opencode/plugin/x.js"
source = "x.js"
requires_dir = "~/.config/opencode"  # skip when that agent isn't installed

[[agent_patches]]               # append args to an EXISTING agent (reversible)
name = "claude"
append_args = ["--settings", "{home}/claude.json"]

[[config_merges]]               # reversibly deep-merge JSON into an agent's own
path = "~/.gemini/settings.json"  #   SHARED config file (never clobbered)
source = "antigravity-hooks.json"  # objects recurse, arrays union; uninstall prunes
requires_dir = "~/.gemini"      #   exactly our entries (by marker). no-op write
                                #   when unchanged; malformed target soft-skipped

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
| `active_theme` | `Ctrl+Y` / `F4` picker | TUI palette (fifteen built-ins) |
| `editor_command` | `thurbox-cli editor set "<cmd>"` | what `Ctrl+O` runs |
| `active_extensions` | `thurbox-cli extension activate/deactivate` | JSON array of active extensions to self-heal |
| `builtin_hooks_optout` | `thurbox-cli extension deactivate hooks` | `1` when the user opted out of the auto-activated hooks extension |

These are in the DB rather than a file because they are written
concurrently by multiple thurbox processes (TUI, CLI, MCP) and picked
up live via `PRAGMA data_version` polling.

## Environment variables

User-set (read by thurbox):

| Variable | Used for |
|----------|----------|
| `XDG_CONFIG_HOME`, `XDG_DATA_HOME` | config/data roots |
| `VISUAL`, then `EDITOR` | `Ctrl+O` editor when `editor_command` is unset |
| `SHELL` | the `Ctrl+T` companion shell pane (fallback `/bin/sh`) |
| `RUST_LOG` | log filter for `thurbox.log` |
| `THURBOX_SOCKET` | overrides the **local** multiplexer socket name (default `thurbox`; dev builds `thurbox-dev`). For test/sandbox tooling: Unix scoping uses `TMUX_TMPDIR`, but psmux (Windows) resolves every `-L <name>` machine-wide, so this is the only way to fully scope an instance there. Remote hosts are unaffected (socket from `hosts.toml`). Empty = unset. |

Set **by** thurbox into every spawned agent process (not user-set;
`session_ops::inject_thurbox_env` / `App::build_spawn_inputs`). An
agent — or a `thurbox-cli` call running inside the session — reads
these to prove its own identity without scraping panes or names:

| Variable | Set into agent process |
|----------|------------------------|
| `THURBOX_SESSION` | the stable thurbox `SessionId` (the registry key); read back by `thurbox-cli message`/`inbox` for self-identity |
| `THURBOX_SESSION_ID` | the agent's own conversation id (`agent_session_id`); consumed by the metrics statusline. Distinct from `THURBOX_SESSION` |
| `THURBOX_TASK` | the originating task id; task-spawned sessions only (headless `task run`) |
| `THURBOX_METRICS_DIR` | metrics output dir |
| `THURBOX_CONFIG_DIR` / `THURBOX_DATA_DIR` | the resolved config/data dirs, so the agent's `thurbox-cli` (its status hook) targets the same DB the TUI reads — independent of XDG, which `thurbox-cli` is on PATH, or a stale tmux-server env. Also honored if you set them yourself to relocate thurbox's state. |

Set **at build time** (not runtime):

| Variable | Used for |
|----------|----------|
| `THURBOX_RELEASE_VERSION` | read by `build.rs` to inject the binary version at build (CI release workflow sets it, e.g. `v0.7.0`); absent → falls back to `CARGO_PKG_VERSION` |

Editor resolution order: DB `editor_command` → `$VISUAL` → `$EDITOR` →
error toast.

## Versioning

The SQLite schema migrates automatically (`schema_version` in
`metadata`). The TOML files carry a `config_version = 1` marker so a
future format change can migrate them too; current files are version 1
and the field is optional.
