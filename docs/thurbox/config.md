# Configuration Reference

Every knob thurbox reads, where it lives, and how it behaves. One file
per audience/lifecycle: hand-edited registries are TOML, and what the
interface writes for itself is JSON. thurbox keeps no database: every
session is a Task of a Fleet ([`[fleets]`](#fleets--the-fleets-that-manage-sessions)).

Dev builds (version `0.0.0-dev`) use `thurbox-dev` in place of
`thurbox` in every path below, plus a `thurbox-dev` tmux socket, so a
development checkout never touches your real setup.

## Files at a glance

| File | Format | Edited by | Read | Purpose |
|------|--------|-----------|------|---------|
| `~/.config/thurbox/hosts.toml` | TOML | you | startup | ssh machines + local WSL distros for the companion shell; places replace it ([Where sessions run](../where-sessions-run.md)) |
| `~/.config/thurbox/settings.toml` | TOML | you + `Ctrl+,` panel | **live** (feature flags) / startup (rest) | tuning knobs + feature flags |
| `~/.config/thurbox/themes.toml` | TOML | you | startup | custom theme palettes |
| `~/.config/thurbox/ui/` | Lua | you | **live** (watched, 120 ms debounce; `F10` forces) | **the interface itself** — one file per pane, plus `layout.lua`, `lib/`, `AGENTS.md`/`README.md` for whoever edits it, and a directory per plugin installed from a repository (its own working copy, `.git` included) |
| `~/.config/thurbox/ui/plugins.toml` | TOML | you (or `thurbox-cli plugin`) | on each `plugin` command | **what the interface is composed of**: a source, a destination file and an optional pin, per installed pane |
| `~/.config/thurbox/ui/plugins.lock` | TOML | `thurbox-cli plugin` | on each `plugin` command | what each entry resolved to, and the digest of every file delivered. Machine-written — commit it beside the spec and the same interface reproduces elsewhere |
| `~/.config/thurbox/ui.json` | JSON | `F1` / Interface tab (or you) | startup | your decisions *about* the interface: rebound chords, plugin settings, and the action band's order (`"pills": { "<action>": <priority> }`, replacing what the declaring plugin chose — an entry naming an action nothing declares is ignored, not drawn). Written back only by a registry that **read** it (`registry::Origin`), so a process holding no decisions cannot empty it |
| `~/.config/thurbox/plugin-state.json` | JSON | Interface tab (or `thurbox-cli plugin`) | startup | **what a plugin may do**: `trusted` (absolute path → the grant, a digest or `{pin, digest}` for an installed pane) and `disabled` (absolute paths turned off). Kept apart from `ui.json` so the file capabilities are granted from is written only when a grant or the disabled set changes. One that exists but cannot be read or parsed is never replaced, and `config validate` names it. Same `registry::Origin` rule as `ui.json` |
| `~/.config/thurbox/interface.json` | JSON | thurbox (theme picker, `thurbox-cli editor`, the session list's order, the companion shell) | startup, and again after each change | the theme last chosen, the editor `Ctrl+O` opens, the manual session order and one record per companion shell (its multiplexer's route, its pane, its window's stamp). Rewritten whole under a lock every process takes (`interface.json.lock`). Missing or unreadable is the defaults |
| `~/.local/share/thurbox/bookmarks.json` | JSON | the creation flow | each read of the repository list | the repositories and folders the creation flow remembers, per host. Rewritten whole under a lock every process takes (`bookmarks.json.lock`) |
| `~/.local/share/thurbox/notify-dedupe.json` | JSON | `thurbox-cli notify send --key` | each keyed send | each dedupe key and when its hold ends; expired holds are dropped on the next keyed send. Rewritten whole under a lock (`notify-dedupe.json.lock`) |
| `~/.local/share/thurbox/perf.json` | JSON | thurbox, while perf timing is on | each perf window | the perf snapshot `thurbox-cli perf` prints |
| `~/.local/share/thurbox/thurbox.log` | text | thurbox | — | logs (incl. config warnings). Rotated daily into `thurbox.log.<date>`; the 30 most recent are kept and older ones deleted at startup |
| `~/.config/thurbox/automations.json` | JSON | `thurbox-cli automation` (or you) | each tick and each command | every automation: its trigger, action and prompt, its last and next run, and its 20 most recent runs. Rewritten whole under a lock every process takes (`automations.json.lock`); a file that does not parse is an error, never an empty list |

`settings.toml` reloads **live**: the TUI checks its mtime about once per
second and applies edits without a restart. A successful edit shows a toast;
an invalid file leaves the last good settings in force and reports the error.
The `ui/` directory reloads live too, but on a **filesystem watcher**
(120 ms debounce) rather than a poll, and `F10` forces one. For
`settings.toml` the values that apply live are the `shell_pane` and
`perf_hud` flags and `[clipboard]`; the restart-only values stay
published through a write-once global (so they can't drift mid-frame),
and the reload toast says when a restart is needed. `hosts.toml` (SSH
backends register at startup) and `themes.toml` need a restart.

`settings.toml` can also be edited from the TUI: **`Ctrl+,`** (alt `F6`)
opens a **Settings panel** listing every knob. It writes the file back
**preserving its comments**, and feature flags that gate UI panels apply
**live** on save; the rest (`mouse`, `notifications`, `automations`,
`version_check`, `auto_update`, the three editable `[notifications]` knobs,
and the scalars) take effect on the next launch — the panel marks those rows
with `⟳` and toasts a restart note. The panel exposes only the three
editable notification knobs (`suppress_for_active`, `sound`,
`min_interval_secs`); `[notifications] backend` is **not** in
the panel — set it only by hand-editing `settings.toml`. Hand-editing
the file (or the panel in another instance) is picked up the same way,
via the live mtime poll.

Every file thurbox writes that a user edits or thurbox reads back — `settings.toml`,
`ui.json`, `plugin-state.json` (and the other seeded config files),
the interface's plugin
lock, spec and bundled files — is written through
one writer, `paths::write_atomically`. It stages a complete file beside the
destination and replaces it with one native same-directory rename, so a reader
never observes a truncated intermediate file and a failed staged write (a
crash, a full disk) leaves the previous file intact. The rename is
`std::fs::rename`, which on Windows replaces even while a reader holds the old
file open. An existing symlink is preserved and its target is replaced, a
replaced file keeps its permissions, and a file made read-only is refused
rather than replaced. This
protects the process-interruption boundary, not storage failure: the staged
file and parent directory are not explicitly synced to stable storage.
thurbox's restrictions in `crates/architecture.toml`, which `arch-rules`
checks, fail on any other
production write, apart from an allow-list of logs, a cache, the control
socket's privately staged files and the self-update's staged binary.

The config files beside `config.toml` — `settings.toml`,
`themes.toml` and `hosts.toml` — are
read and written by one loader, `config::file`. It seeds an absent file with
its documented defaults, parses it reporting every unknown key as a warning,
writes through `paths::write_atomically`, and on any failure (no path, a seed
it could not write, an unreadable or malformed file) keeps thurbox starting:
the file's fallback is used and a warning names the file and what it fell
back to.

All paths respect `$XDG_CONFIG_HOME` / `$XDG_DATA_HOME`.

### Which file do I edit?

A task-to-file map so you don't have to scan every section to find
the right knob:

| I want to… | Edit | Section |
|------------|------|---------|
| Add a coding agent, pin a model | Fleet's harness profiles | [Agents](#agents) |
| Run sessions on a remote machine or in a WSL distro | Fleet's config, or `[fleets]` in `settings.toml` (planned) | [Where sessions run](../where-sessions-run.md) |
| Turn a whole TUI feature on/off (mouse, notifications…) | `settings.toml` `[features]` | [`[features]`](#features--whole-feature-switches) |
| Tune scrollback, panel breakpoints, git polling | `settings.toml` | [settings.toml](#settingstoml) |
| Change when/how OS notifications fire | `settings.toml` `[notifications]` | [`[notifications]`](#notifications--os-notification-settings) |
| Add or recolour a TUI theme | `themes.toml` | [themes.toml](#themestoml) |
| Rebind a key | `ui.json` `bindings` (the `F1` editor writes it) | [Files at a glance](#files-at-a-glance) |
| Reorder the action band's buttons | `ui.json` `pills` | [Files at a glance](#files-at-a-glance) |
| Set the `Ctrl+O` editor, pick a theme | `interface.json` (`thurbox-cli editor`, the `Ctrl+Y` picker) | [Files at a glance](#files-at-a-glance) |

None of these files need to exist on a fresh install — every one is
seeded (commented-out where applicable) on first run, and absent files
fall back to built-in defaults.

Config problems are **not silent**: parse errors, unknown fields,
invalid chords, and chord conflicts surface as a status-bar toast on
startup (and in the log file). Unknown TOML keys are tolerated —
a stale key or a typo is *reported by name* but
your file still loads — while syntax/type errors fall back to
zero hosts, or defaults (settings).

## Agents

The agents a session can run are the harnesses Fleet has profiles for, and
Fleet launches, resumes and watches them ([agents.md](agents.md)).

### What happens when it is *not* installed

thurbox starts with no multiplexer on the machine, and that is deliberate —
browsing, reading and configuring must work on a fresh box. `thurbox-cli
doctor` answers the question directly: the Fleet in force (its program,
version, harnesses and config), `runtime-mux`, the multiplexer and its
version, the launcher for each configured host, and the full search path.

The advice is platform-specific and never invented. A Windows user is pointed at
[psmux](https://github.com/psmux/psmux), never at `tmux`; where the install
command depends on a distribution, the package name is given and the link is
tmux's own [install page](https://github.com/tmux/tmux/wiki/Installing) rather
than a guessed invocation.

## hosts.toml

Declares off-local hosts — **remote SSH machines** and **local WSL
distros**. Each `[[hosts]]` entry registers a session backend named
`ssh:<name>` (default kind) or `wsl:<name>`. Seeded fully commented-out
(fresh installs are local-only for SSH). Malformed file → zero
configured hosts, error shown.

| Field | Required | Default | Purpose |
|-------|----------|---------|---------|
| `name` | yes | — | backend id `ssh:<name>` / `wsl:<name>`; what the host picker offers |
| `kind` | no | `ssh` | transport: `ssh` (remote machine) or `wsl` (local distro) |
| `destination` | for ssh | — | ssh target (`user@host` or `~/.ssh/config` alias) |
| `distro` | no | `name` | WSL distro name (`kind = "wsl"` only) |
| `ssh_opts` | no | `[]` | extra ssh flags, one token per element (ssh only) |
| `socket` | no | `thurbox` | host `tmux -L` socket name. The socket file lives under the host's thurbox data dir, `~/.local/share/thurbox/tmux/tmux-<uid>/<socket>` — see [Where the tmux socket lives](#where-the-tmux-socket-lives) |
| `session` | no | `thurbox` | host tmux session name |
| `multiplexer` | no | platform default | host preference for new sessions; an explicit per-create choice wins. Names are `tmux`, `psmux`, `rmux`, and `herdr` (no Herdr adapter yet). Use `psmux` for a native Windows SSH host |
| `platform` | no | `windows` if `multiplexer = "psmux"`, else `posix` | the host's OS: `posix` or `windows`. Decides its shell and paths, independent of `multiplexer` (ssh only — a WSL distro is always `posix`) |
| `path_prepend` | no | `[]` | directories put first on the `PATH` of the multiplexer commands and the panes they open on a POSIX host — absolute or `~/`-rooted (`~` = the host's `$HOME`); for what the host's login shell cannot report |

**The `PATH` on a host.** ssh and `wsl.exe` hand a command a
non-login environment — `wsl.exe -e` skips the user's shell entirely, so
not even `~/.zshenv` is read — which has none of `~/.local/bin`,
`~/.cargo/bin`, nvm/fnm and the like. So thurbox reads the host's login
`PATH` once per host and process (`$SHELL -lc`, the passwd shell when
`$SHELL` is unset, and `/bin/sh -lc`; stdin closed, never `-i`, 5 s each
under `timeout`, 15 s overall). The multiplexer commands (so a Homebrew
tmux in `/opt/homebrew/bin`, which only a macOS login shell puts on the
`PATH`, is found) and the panes they open
run there on `path_prepend`, then the login shells' `PATH`, then the
launcher's own (on WSL that includes the Windows `PATH` WSL appends unless
`appendWindowsPath = false`), each directory once. A host whose shells cannot be read keeps
the launcher's `PATH` with a warning in the log.

**SSH** auth comes entirely from your `~/.ssh/config`; thurbox never
handles credentials. **WSL** distros are reached with
`wsl.exe -d <distro>` and need no config entry at all — they are
**auto-discovered** (`wsl.exe -l -q`) and appear in the host picker
automatically; add a `kind = "wsl"` entry only to override
a default (e.g. `socket`). Discovery also works from *inside* a
distro (interop puts `wsl.exe` on `PATH` there), and excludes the distro
thurbox is itself running in: that one is this machine. An entry whose
`distro` is that one is ignored with a startup warning, and an entry merely
*named* after it is warned about. A distro needs its selected multiplexer
(`tmux` >= 3.2 by default, or opt-in RMUX >= 0.10.0) and `git`. Host changes
require a restart (the registry is read once and each host's `$HOME` is
cached for the process lifetime).

The new-session modal's host step names the Fleet place a session runs on.
Fleets and places are explained in
[Where sessions run](../where-sessions-run.md).

### Host platform

A host's **platform** (`posix` or `windows`) is its operating system, and it is
a choice of its own: not the multiplexer, not the way the host is reached, and
not the OS thurbox itself runs on. It decides the host's shell and path
semantics — PowerShell, `%USERPROFILE%` and no `/bin/sh` on Windows; `sh -c`,
`$HOME` and `/bin/sh` as the server's `default-command` on POSIX — so a Windows
thurbox driving a WSL distro pins `/bin/sh` there, and a Windows host on a
multiplexer other than psmux still gets PowerShell.

An entry that does not set `platform` is Windows when `multiplexer =
"psmux"`, and POSIX otherwise. A WSL distro is POSIX
whatever its entry says. What a multiplexer can do is not the platform's to
say either: whether thurbox polls a backend for dead panes is whether that
multiplexer reports a closed window (tmux does; psmux and RMUX need polling).

### Multiplexer choice and existing sessions

A Fleet session's terminal is its runtime's: `runtime-mux` starts it on this
platform's default multiplexer (`tmux` on POSIX, `psmux` on native Windows).
Nothing in thurbox chooses another one for a session. A `hosts.toml` entry's
`multiplexer` names that host's preference for the companion shell (`Ctrl+T`)
opened there. Names are `tmux`, `psmux`, `rmux`, and `herdr`.

The resolved choice is a session's **route**. One parser (`session::Route`)
reads it, and every route names its multiplexer:

| route | machine | multiplexer |
|---|---|---|
| `local:<mux>` | this one | `<mux>` |
| `ssh:<host>:<mux>`, `wsl:<distro>:<mux>` | that host | `<mux>` |

Any other key, such as `local-tmux`, `tmux` or the empty key, names no route
and is refused. The host picker carries a **host key**, `ssh:<host>` or
`wsl:<distro>`, which names a machine only: `hosts.toml` resolves it, and the
host's `multiplexer` (tmux by default) completes the route. A user never types
a multiplexer to pick a host.

A host name may not contain `:`, which separates it from the multiplexer in a
route. `hosts.toml` ignores such an entry with a warning, and
`config validate` fails naming it.

Which multiplexers work is what is **registered**, never the OS: every
multiplexer an adapter implements (`tmux`, `psmux`, and `rmux`) is registered for
this machine and for every host, whatever either's platform or preference — a
binary that is not installed is reported by name when it is first needed.
A route naming a multiplexer no adapter implements (`herdr` today)
is refused by name, and is never driven with another binary.

### Multiplexer requirements and RMUX setup

Use the latest stable release of the selected multiplexer. These are the
minimums enforced by the adapters, checked against upstream on 2026-10-22.

| Multiplexer | Minimum | Code enforcing it | Upstream evidence |
| --- | --- | --- | --- |
| [tmux](https://github.com/tmux/tmux) | 3.2 | [`MIN_TMUX_VERSION` and `check_min_version`](../../crates/runtime-mux/src/tmux.rs) | [3.2 changes](https://github.com/tmux/tmux/blob/3.2/CHANGES): control-client flow control and format subscriptions |
| [psmux](https://github.com/psmux/psmux) | 3.3.8 | [`MIN_PSMUX_VERSION`, `check_psmux_version` (server) and `check_psmux_binary` (binary)](../../crates/runtime-mux/src/psmux.rs) | [3.3.8 release](https://github.com/psmux/psmux/releases/tag/v3.3.8): pane-slot targets (`thurbox:.%3`) resolve to that pane; [3.3.7](https://github.com/psmux/psmux/releases/tag/v3.3.7): dead warm-pane fix (psmux#450) |
| [RMUX](https://github.com/Helvesec/rmux) | 0.10.0 | [`Rmux::check_banner`](../../crates/runtime-mux/src/rmux.rs) | [0.10.0 release](https://github.com/Helvesec/rmux/releases/tag/v0.10.0): wire protocol 8, incompatible with 0.9.x daemons |

For tmux, the limiting features are `refresh-client -f pause-after=5`,
`refresh-client -A` (pane monitoring/resume), and `refresh-client -B`
(format subscriptions), added in 3.2. Control mode arrived in 1.8;
`send-keys -H` and pane options in 3.0; `set-clipboard external` in 2.6;
`resize-window` in 2.9. These are recorded in the same upstream changelog.
The mouse-state seed in
[`pane_state_seed`](../../crates/runtime-mux/src/tmux_compat/server.rs) uses six
`#{mouse_*_flag}` formats: all are present in upstream
[3.2 format.c](https://github.com/tmux/tmux/blob/3.2/format.c).
The changelog records the SGR/UTF-8 mouse formats in 3.0; the source confirms
the floor rather than assuming a newer requirement from the current manual.
Unknown flags expand empty and the seed falls back to press reporting when
only `mouse_any_flag` is set. `extended-keys-format` arrived in
[3.5](https://github.com/tmux/tmux/blob/3.5/CHANGES) but is optional: its
rejection is tolerated on 3.2–3.4. See ADR-12 for the control setup.

psmux's floor is its own release version, not a claim of tmux feature parity.
Older releases can spawn dead panes after console attach/detach; 3.3.7 fixes
that. 3.3.7 still runs a command aimed at a pane on the session's active pane
instead, so keystrokes reach another session; 3.3.8 fixes that. The adapter
checks both the client banner and the running server's `#{version}` before
any pane is named — a server started before an upgrade must be restarted
(`psmux -L <socket> kill-server`, which ends the agents running on it) before
thurbox will use it; an old binary only needs replacing. RMUX
checks its `rmux -V` banner instead: its `#{version}` is the tmux
compatibility version, not the RMUX release.

RMUX is a Rust terminal multiplexer with a tmux-compatible command interface.
It never changes the platform defaults: a host's `multiplexer = "rmux"` opens
that host's companion shells on it. Install it separately on that machine,
following its
[installation guide](https://github.com/Helvesec/rmux/tree/v0.10.0#installation).
For example, `cargo install rmux --locked` installs the latest published
crate; check `rmux -V` afterwards. Release archives need their complete
package layout: on Unix run the archive's `./install.sh --prefix ~/.local`,
and keep its `bin/` and `libexec/`; do not copy just the executable.

RMUX 0.10.0 rejects `pause-after`, `refresh-client -A`, and subscriptions
(`refresh-client -B`). Thurbox omits those commands, uses one reply block for
command lists, and polls pane liveness and remote hook options. Output buffering
therefore relies on RMUX rather than tmux's pause/resume mechanism. See
[ADR-31](architecture.md#adr-31-tmux-and-psmux-are-peer-adapters-over-one-tmux-protocol-server).

If RMUX is not found, check `rmux -V` in the environment that launches
thurbox. For a host, check it on that host or inside the WSL
distro. A version rejection needs an upgrade on the session machine.
RMUX 0.9.x daemons cannot talk to a 0.10.0 client: stop the old server before
upgrading, using `rmux -L <socket> kill-server` for each affected socket.
This stops its sessions; finish or save the agents' work first.

A socket learned from a host's own CLI (`version --json`'s `tmux_socket`) is
kept per **host**, not per route: it is the address of the thurbox instance
there (ADR-12), which every multiplexer on that host runs under. Why routes
are read this way, and what it costs, is ADR-28 in `docs/thurbox/architecture.md`.

## settings.toml

> The `F6` / `Ctrl+,` panel edits this file: core settings are listed above
> whatever plugins declared, `⟳` marks the ones that wait for the next
> launch, and `Ctrl+S` saves — written back through `toml_edit`, so the
> comments below survive.

Scalar tuning knobs plus the `[features]` switches, seeded fully
commented-out (defaults apply when absent). Only knobs a user plausibly
wants are exposed; internals stay hardcoded. The seed closes with a
**Common recipes** block — copy-pasteable groupings (bigger scrollback,
a minimal/focused TUI, notification tuning, enabling the update badge),
all commented so defaults still apply out of the box.

| Key | Default | Purpose |
|-----|---------|---------|
| `scrollback_lines` | `1000` | terminal scrollback kept per session — and how far back global search reaches |
| `hidden_terminal_secs` | `30` | how long a session can be off screen before its terminal grid is dropped (rebuilt from tmux when shown or searched); `0` keeps every grid |
| `stream_panes` | `false` | open each local tmux session's pane through `runtime-mux view`, a [terminal stream](../runtime/README.md#the-terminal-stream), instead of thurbox's own control-mode connection; restart to apply. A stream pane keeps its grid while hidden, so `hidden_terminal_secs` does not apply |
| `two_panel_min_cols` | `80` | width below which only the terminal renders |
| `git_poll_secs` | `5` | how often each session's git worktree is re-statted; `0` turns it off |
| `[fleets.<name>]` | none: `fleet` on this machine | the Fleets that manage sessions; see [`[fleets]`](#fleets--the-fleets-that-manage-sessions) |

A complete `settings.toml` showing every knob at its default — copy
this, uncomment what you want to change, and restart:

```toml
config_version = 1

# Scalar tuning knobs (top level)
scrollback_lines      = 1000   # terminal scrollback kept per session
hidden_terminal_secs  = 30     # seconds off screen before a grid is dropped; 0 = keep all
stream_panes          = false  # local tmux panes through runtime-mux view, a terminal stream
two_panel_min_cols    = 80     # width below which only the terminal renders
git_poll_secs         = 5      # seconds between git stats of a session; 0 = off

[features]
shell_pane    = true
perf_hud      = true
automations   = true
mouse         = true
notifications = true
version_check = true           # on by default (1.0): makes a network call
auto_update   = true           # on by default (1.0): downloads + replaces binaries

[notifications]
suppress_for_active = true     # skip the session you're currently viewing
sound               = true     # play the OS default notification sound
min_interval_secs   = 5        # per-session floor between notifications

[clipboard]
provider       = "auto"        # auto | native | osc52 | none
copy_on_select = true          # releasing a drag copies; false = Ctrl+C copies a selection

# No [fleets] table: `fleet` on this machine. Restart-only.
[fleets.laptop]
transport = "local"            # "local" | { ssh = "me@box", ssh_opts = [...] } | { wsl = "Ubuntu" }
program   = "fleet"            # the fleet binary: a name on PATH or a path, on that machine
```

### `hidden_terminal_secs` — how much memory a session off screen holds

tmux parses every pane and keeps its screen and history. The interface parses
them a second time to draw them, into a grid of 32 bytes a cell plus
`scrollback_lines` rows of history, so a 200-column session with a full
history costs about 6 MiB, whether you look at it or not. With this set, a
session that has been off screen for this many seconds drops that grid, and
one not shown since the interface started never builds it. It keeps reading
its output meanwhile: its title, a bell or notification, and whether it is
printing are reported as before.

Showing the session, or a search reaching it, reads the pane back from tmux.
That is a round trip and a parse, about 10 ms for a 200x50 pane with 1,000
lines of history, so switching to a session hidden longer than this takes that
long to draw. A search whose strip was just opened reads each such session the
same way before the first keystroke. `0` keeps every grid for as long as the
session runs (ADR-P27 in
[performance.md](performance.md)). psmux (Windows) cannot hand a pane back in
step with its output, so its sessions always keep their grids. Read at startup.

### `git_poll_secs` — how much `git` thurbox runs

The diffstat and the ahead/behind beside each session come from `git`, and
they are *polled*: every session, every `git_poll_secs`. So this one number,
times the session count, is thurbox's whole git load — and a session whose
branch is ahead of the default and has not landed yet pays for the merge
check as well, which is seven subprocesses rather than one.

```text
one poll of one session
  git status --porcelain=v2 --branch     always
  git diff --numstat HEAD                only when a tracked file differs
  ┌ symbolic-ref origin/HEAD             ┐
  │ merge-base --is-ancestor HEAD …      │  the merge check: only while the
  │ diff --quiet origin/main HEAD        ├─ branch is ahead and unlanded, and
  │ merge-base / cherry / commit-tree    │  at most once a minute per commit
  └ cherry                               ┘
```

Two things keep that from scaling with the session list, and neither needs
configuring: an answer that comes back **unchanged** stretches that session's
own interval — doubling each time, up to 12× `git_poll_secs` — and the first
change resets it, so a dormant session is re-statted a twelfth as often; and
the merge check's answer is remembered against the commit it was computed for
and rechecked on the first poll at least a minute later — so its age is bounded
by a minute or by that session's own interval, whichever is longer.

Raise `git_poll_secs` on an instance holding many sessions, and set it to `0`
where a process launch is expensive for reasons outside thurbox — **Microsoft
Defender / Intune on macOS, or Defender for Endpoint on Windows, scans every
process as it is created**, which turns a poll into an antivirus workload. At
`0` nothing is statted, so the session list shows no diffstat and **a delete
always asks for confirmation** — the confirmation reads a session's git state to
describe what may be lost, and "could not be read" is reported rather than
treated as clean. It is the same prompt
a remote session already gets. `thurbox-cli` is unaffected. Read once at
startup, so a change applies on the next launch.

### `[features]` — whole-feature switches

Switch off behaviour that reaches outside the interface. All default to
`true`, `version_check` and `auto_update` included, which both reach the
network. `shell_pane`,
and `perf_hud` apply **live** on save; `automations`,
`mouse`, `notifications`, `version_check` and `auto_update` take effect on
the next launch. Data is never touched, so re-enabling a flag is lossless.

**A pane is not switched off here.** Panes are files, so turning one off
is `space` on its row in the Interface tab (`Ctrl+,` then `]`), recorded
in `plugin-state.json`. That does strictly more than a flag could: it works for a
plugin *you* wrote, which no compiled-in flag could know about.

The panes for code review and the info panel are plugins
([`thurbox-code-review`](https://github.com/Thurbeen/thurbox-code-review),
[`thurbox-info-panel`](https://github.com/Thurbeen/thurbox-info-panel)),
switched on and off from the Interface tab like any other pane.

Seven keys are **retired**, because nothing read them: the top-level
`three_panel_min_cols`, `[features]` `file_viewer`, `info_panel`,
`code_review` and `global_search`, and `[notifications]` `also_on_waiting`
and `on_automation_failure`. A file that sets one still loads and passes
`thurbox-cli config validate`; startup names each as no longer read, and a
save from the settings panel removes it.

| Key | Default | Controls |
|-----|---------|----------|
| `shell_pane` | `true` | per-session shell toggle (`Ctrl+T`) |
| `perf_hud` | `true` | perf HUD overlay (`F12`): live perf counters, frame/tick timing and the per-pane cost table (see `docs/thurbox/performance.md`) |
| `automations` | `true` | the interface firing due automations while it runs (the CLI, `automation tick` included, stays fully functional) |
| `mouse` | `true` | mouse capture: clicks, wheel, drag-select, hover, scrollbars |
| `notifications` | `true` | OS desktop notifications when a session needs attention |
| `version_check` | `true` | GitHub update check: TUI header "update available" badge + `thurbox-cli version --check` |
| `auto_update` | `true` | Silent self-update **within the current major**: download + verify + replace the binaries on startup + `thurbox-cli update` |

`automations = false` stops the interface firing due automations; nothing
fires unless an OS timer runs `thurbox-cli automation tick` or someone runs
`automation run`. Every other `thurbox-cli automation` command still works.
Disabling
`shell_pane` hides existing shell panes but never kills their
processes. `mouse = false` skips terminal mouse capture entirely, so
the terminal keeps its native mouse behavior (its own text selection,
URL handling, etc.) and no click/wheel/hover handling runs in the TUI.
`notifications = false` keeps the background dispatcher thread from
ever starting (zero overhead) and silently no-ops every transition;
the session status display itself is unaffected.

A delete is final: a session is a Fleet Task, and `fleet delete task` has no
undo window. Fleet keeps the Task, marked for deletion, until its controller
has released the session. The row leaves the list on the keystroke and stays
off it while Fleet tears the session down. A Task still marked for deletion
three minutes after Fleet marked it comes back, marked `deletion stuck` with
why Fleet still holds it (its `Released` condition), so it can be inspected
with `fleet get task`. Deleting it again leaves it listed, since Fleet has
nothing more to do for it. Three minutes covers a release Fleet still expects to
finish on its own: three attempts at the runtime's default 15 s call bound,
ssh hosts included, with the 30 s wait between attempts. A session still busy
when it is deleted waits to be at rest first, and can pass the mark that way.
A delete that Fleet refuses keeps the row, and the message band says why.

`version_check` enables the update check — it
makes a network call. On launch the TUI reads a cached result
(`~/.local/share/thurbox/version-check.json`) and, if it is older than
24 h, fires a single best-effort background fetch of GitHub's release list
(`api.github.com/repos/Thurbeen/thurbox/releases?per_page=100`, via
`curl`/`wget` — no new dependency) and takes the newest release **on the
binary's own major line**; a newer one shows a `⬆ vX.Y.Z available` badge next
to the version in the header. The fetch never runs
on the render path and never blocks startup; failures are silent. Dev
builds (`0.0.0-dev`) never show the badge. The same flag enables
`thurbox-cli version --check`, which fetches fresh on demand and reports
current vs. latest, plus any newer major with a link to the migration notes
(`thurbox-cli version` with no flag always prints the current version,
regardless of the flag).

`auto_update` goes a step further than
`version_check`: instead of just showing a badge, the TUI **silently
updates itself** on startup. On every
launch (it does **not** reuse the `version_check` badge's 24 h cache, which
the badge keeps fresh) it
fetches the release list; if a newer release exists on the same major line it
downloads that
release's tarball + checksums from GitHub Releases (`curl`/`wget`, no new
dependency), verifies the SHA256 **in process** (`sha2`, so the check does not
depend on the local `PATH`), extracts it
(`tar`, or PowerShell's `Expand-Archive` for Windows' zip), and atomically
replaces the installed `thurbox`/`thurbox-cli`
binaries in place — mirroring `scripts/install.sh`. The download is verified
**before** any installed file is touched, so a failed/corrupt download leaves
the current binaries untouched; the whole step runs before the TUI takes the
terminal and is best-effort (any failure is logged and startup continues on
the current version). The replaced binary takes effect on the **next launch**
(the running process keeps its open file), so the TUI shows an "Updated to
vX.Y.Z — restart to apply" status line. `thurbox-cli update` performs the
same update on demand (with `--force` to bypass the up-to-date and dev-build
guards — never the major line); dev builds (`0.0.0-dev`) never auto-update. The default install
location (`~/.local/bin`) is user-writable; a system-wide install in a
root-owned directory will fail the replace (logged, non-fatal). `version_check`
and `auto_update` are independent — enable either or both.

**Windows takes the same path, with two differences.** The release artifact
is the `.zip` `install.ps1` extracts, so the
unpacker is chosen by the archive's extension. And a rename cannot replace an
executable a process is running from — Windows keeps its image mapped — so the
swap there moves each installed binary aside to `.thurbox.exe.old` first (which
Windows allows for a running image) and then moves the new one in. The running
process keeps its old image until the next launch, as on Unix. A thurbox still
running from the last update's backup — a `thurbox-cli.exe` that has not
exited — keeps that file, so the next update moves the binary to
`.thurbox.exe.1.old` (and so on) instead, and deletes every backup nothing runs
from any more. The two binaries are committed together: if either cannot be
swapped, the one already swapped is put back from its backup, so a failed update
normally leaves the TUI and CLI on the version they had. The
put-back is best effort: if it is refused too — say a process has just started
from the new binary — that binary stays on the new version, and the update
fails with an error saying it may be partial. Re-running `install.ps1` over a
running thurbox works the same way: it renames each installed binary to `.<name>.old` rather
than deleting it, and names the process to close when even that is refused.

**Auto-update never crosses a major version.** An install only ever looks at
releases on its own major line, maintenance releases GitHub does not mark
"Latest" included. `thurbox-cli version --check` and `thurbox-cli update` say a
newer major exists and link the migration notes; the header badge only offers
the same line. A new major can be a different program under the same binary
name. **No flag crosses**, `--force` included: moving to a new major is a reinstall
(`scripts/install.sh` or the new major's package). The line is
`update::version_check::RELEASE_LINE`, and the release workflow refuses to cut a
version off it — `docs/thurbox/releasing.md`, "v2 and v3".

### `[fleets]` — the Fleets that manage sessions

```toml
[fleets.laptop]
transport = "local"                     # the default
program = "fleet"                       # the default: a name on PATH or a path

[fleets.box]
transport = { ssh = "me@box", ssh_opts = ["-p", "2222"] }
program = "/home/me/.cargo/bin/fleet"   # ssh does not read your login PATH
config = "/home/me/.config/fleet/config.toml"   # optional: `fleet --config <path>`

[fleets.ubuntu]
transport = { wsl = "Ubuntu" }
```

A Fleet manages sessions
([ADR-41](architecture.md#adr-41-fleet-is-thurboxs-engine-sessions-run-on-runtimes)),
and the interface is a client of Fleet's command line. thurbox keeps no
database. [Where sessions run](../where-sessions-run.md) explains a Fleet and
a place.

- Each `[fleets.<name>]` table is one Fleet. `transport` says how `fleet` is
  run: `"local"` (the default), `{ ssh = "<destination>", ssh_opts = [...] }`
  or `{ wsl = "<distro>" }`. Over ssh, each call is one `ssh` with
  `BatchMode`, `ConnectTimeout=5` and a shared master connection. `program`
  (default `fleet`) is found on that machine; ssh runs it with that machine's
  non-login `PATH`, so give a path when `fleet` is in `~/.cargo/bin`.
  `config`, when set, is given as `fleet --config <path>` before every verb.
- No `[fleets]` table means one Fleet, `local`: `fleet` on this machine. Once
  the file lists a Fleet, thurbox uses only the listed Fleets, so a machine
  that is only a client lists no local Fleet.
- The session list follows every listed Fleet: one `fleet watch` each, as
  one list. A row's id is `<fleet>:<task>`, since two Fleets can each hold a
  Task of one name, and the row carries its Fleet (`fleet` in the interface's
  API). A row of a Fleet on another machine shows that Fleet where a host
  would be shown, with the place when it is not that Fleet's `local`
  (`box · vm/sandbox`).
- When a Fleet's watch ends (its `ssh` exits 255, say), that Fleet's rows stay
  in the list, marked `unreachable`, and the snapshot's error names the
  Fleet. A Fleet that cannot be asked has not lost its sessions. The watch is
  started again every 2 seconds, and the rows clear once it reads.
- Commands (create, send, rename, delete) and automations go to one Fleet,
  the Fleet in force: the only one listed, or the first by name when several
  are. A command for a session of another Fleet is refused by name.
  `thurbox-cli doctor` names the Fleet in force. The creation flow offers
  that Fleet's harnesses and places.
- thurbox never starts `fleet up` on a Fleet on another machine: a user
  systemd unit there runs it
  ([the unit](../where-sessions-run.md#2-laptop-thurbox-fleet-on-a-remote-box)).
- v3 reads no `[fleet] program`: it is reported as replaced by `[fleets]`.

| The interface | Fleet's command |
| --- | --- |
| reads the sessions | `fleet watch --json` of each listed Fleet, as one list: each Task is a session row whose id is `<fleet>:<task>` |
| creates a session | `fleet apply`: an interactive Task (no brief, dispatched) over the chosen repositories, with the chosen agent as its harness and the chosen host |
| sends to a session | `fleet send` |
| deletes a session | `fleet delete task`; Fleet has no undo window |
| shows a session's terminal | `fleet attach --json`: the stream it names, as a stream pane |
| keeps a manager | `fleet up`, started whenever none holds the lease, for a Fleet on this machine only |

- Fleet reads its own config (`FLEET_CONFIG`, then its per-user file), unless
  `config` names one. Run `fleet init` before the first `fleet up`.
- `thurbox-cli doctor` checks each listed Fleet: the program and its version,
  the harnesses it has profiles for, and the places it defines. A Fleet whose
  `fleet version --json` lacks `fleet/v1alpha1` in `apiVersions` is refused:
  thurbox writes its Tasks in that version. For a Fleet on this machine it
  also checks the config it reads (and which rule named it) and
  `runtime-mux` on `PATH`. A Fleet reached over ssh whose `ssh` exits 255 is
  `unreachable`, and is asked only once; the probes of a Fleet on another
  machine wait 15 seconds, not 5. thurbox cannot yet tell whether a remote
  Fleet's manager runs: `doctor` points at the unit. The Fleet the interface
  uses fails the report when it does not answer; another listed Fleet only
  warns. Each check of a Fleet names it (`fleet` in `--json`), and `fleets`
  lists each Fleet with `transport`, `in_force` and `unreachable`. The
  multiplexer is checked only when a listed Fleet runs on this machine.
  `thurbox-cli config show` and `thurbox-cli version --json` report the Fleet
  in use under `fleet`, with Fleet's data directory.
- A `fleet up` the interface started runs in a process group of its own and
  outlives the interface: Tasks keep reconciling with no interface open.
- A session row's status is the Task's `agentState` (`working`, `attention`
  as `blocked`, `idle`) over its `sessionState`: `Paused` as `stopped`,
  `Lost` and `Gone` as `failed` whatever the agent said, then `Running`,
  `Idle`, and absent as `unreported`. The row names the Task's
  `activity.source`: a hook's state is drawn solid, a screen rule's dimmed,
  and neither as `?` ([Session status](#session-status)).
- A Task is named after the session, made a Fleet name: lowercase, with each
  run of other characters one `-` (`Read the code` is `read-the-code`). The
  name as given is kept in the Task's `thurbox.dev/name` label, and the row
  is published under it. A name whose Task name is taken (`review` beside
  `Review`) is refused, and the Task already there is left as it was: the
  create is `fleet apply --create-only`, whose exclusive create gives two
  interfaces racing for one name one Task and one refusal.
- The creation flow asks for what a Task takes: the host, the repositories,
  a base branch and a branch name when a repository gets a worktree (the
  base picker fetches `origin` once when it opens, notes a fetch that fails
  or stalls for 10 seconds, and preselects `origin/<default>`), the
  name and the agent. A worktree is the URL
  `git+file://<repo>?branch=<branch>#<base>`, and the runtime makes it; a
  repository without one is `file://<repo>`. A confirmed delete is checked
  against the Task as it is then.
- The host picker's route names a Fleet place: a Fleet row carries the
  place's id, `<runtime>/<place>`, and this machine is `local`. A place
  Fleet's config does not define (`fleet runtime hosts`) is refused.
- The chosen agent is the Task's harness, and only an agent Fleet has a
  harness profile for (`fleet version --json`'s `harnesses`: the
  [bundled ones](agents.md) and each user profile) is taken. Any
  other, `shell` included, is refused by name: Fleet would run a program
  named as the agent.
- A rename sets the Task's `thurbox.dev/name` label (`fleet label`). The
  manual order and each session's companion shell pane are `interface.json`.
  A saved shell is re-adopted on the multiplexer it was opened on; a host's
  `multiplexer` in `hosts.toml` applies to the shells opened after it.
- Fleet revives a session whose terminal is lost. An automation is `thurbox-cli automation`'s,
  in `automations.json`. A todo is a Fleet Task that is not dispatched
  (`fleet apply`). Settings are `settings.toml`; the
  repositories and folders the creation flow remembers are `bookmarks.json`.
- A Task's terminal is the stream `fleet attach` names, opened as a stream
  pane. A Task with no session yet has no pane; a stream that ends is opened
  again. Closing the pane detaches; the agent keeps running. A stream with no
  snapshot to send (a `tty` answer through `fleet term-bridge`) starts on a
  blank screen of the pane's size.
- A Task on another host (`spec.host` not `local`) names a workspace there,
  which no route of thurbox's reaches: its files, diff, git state and editor
  are not read on this machine, even where the same path exists here. The
  diff says where the workspace is. A Task on `local` reads like any local
  session.
- `fleet up`'s standard error goes to a file of its own in thurbox's data
  directory (`$XDG_DATA_HOME/thurbox`, or `THURBOX_DATA_DIR`), created
  exclusively and named for the manager: `fleet-up-<pid>.log`. It is read
  back and removed when the manager exits. A manager that outlives the
  interface keeps writing to it there, and the next start removes the logs
  whose process is gone.
- A watch or a manager that fails is the snapshot's error, beside the last
  rows read. Both are started again: the watch resumes with `--since`. A
  store the running watch cannot read is the error too, until a read works
  again.
- Typing or pasting into a Fleet session's pane waits for a stream that has
  stopped reading up to 1 MiB, and the interface does not wait. Each write
  is queued whole or not at all, so a paste is never cut in its middle: one
  that does not fit is dropped whole, and the interface reports
  `input dropped: <session> isn't reading` where it reports a command's
  error, not in the pane's grid, which a full-screen agent would paint
  over. The same holds for a local tmux pane under `stream_panes`. The pane
  stays open,
  and what is typed once the stream reads again arrives.

Read at startup only.

### `[telemetry]` — OpenTelemetry for the interface

Configures the interface's own diagnostic logs over OTLP (ADR-38). Session
metrics and session event logs are Fleet's (`fleet-telemetry`).

```toml
[telemetry]
enabled = false
endpoint = "http://localhost:4318"
protocol = "http/protobuf"
headers = {}                 # e.g. { authorization = "Bearer …" }
export_diagnostic_logs = false
```

The agents' own telemetry is Fleet's to configure.

#### thurbox's own diagnostic logs

`export_diagnostic_logs = true` (with `enabled`) sends the interface's own
log lines at `WARN` and above to the collector's `/v1/logs`. The resource is
`service.name=thurbox`, `service.version`, `host.name` and `thurbox.instance`
(the multiplexer socket, which tells a sandbox apart from the real instance).
They serve debugging thurbox itself, so the switch is off by default.

- **Interface only.** `thurbox-cli` never sends its log lines.
- **The log file is unchanged.** `thurbox.log.<date>` keeps every level it had
  and is the only log while telemetry is off or the collector is unreachable.
- **Restart-only.** The interface builds its logging once, at start; changing
  the switch, or the endpoint, protocol or headers while it is on, applies on
  the next start.
- **What they contain**: the same text as the log file, which can name paths,
  sessions and hosts.
- **Plain HTTP and `http/protobuf` only.** Reach an `https` backend through a
  local Collector. Each request has a 5-second timeout. Records still queued
  when the interface is killed by a signal are lost; the log file has them.

### `[notifications]` — OS notification settings

Surfaces an OS notification when a session **transitions into a state
that needs your attention**: into `blocked` (`<name> needs you`). The edge is
detected once per tick in `kernel::notify`, reading the same state the
session-list status icon draws, so the banner can never drift from the list
— then deduped per session (`min_interval_secs`) and skipped for the session
you're currently viewing (`suppress_for_active`).

A Fleet session is `blocked` when its Task's `agentState` is `attention` (see
[Session status](#session-status)). Fleet reports no `done` state, so a
finished turn raises nothing.

These session notifications **only fire while the TUI is open** — the
dispatcher thread runs only inside the TUI. `thurbox-cli notify send` notifies
headlessly (see [Scripted notifications](#scripted-notifications)).

**Delivery backend** (`backend`, default `auto`) is detected at startup:

| Backend | When | Click-to-focus |
|---------|------|----------------|
| `dbus` | normal Linux desktop with a running notification daemon (`org.freedesktop.Notifications`) | **yes** — clicking the banner writes a focus request the running TUI reads next tick and switches to that session |
| `windows` (toast) | **native Windows**, or **WSL** / any Linux with no dbus daemon — delivers a Windows toast via `powershell.exe` (WSL needs interop, on by default) | no (a Windows toast can't call back into the thurbox process) |
| macOS banner (what `auto` selects on macOS; not a `backend` value) | macOS native banner. Uses **`terminal-notifier`** when it's in `PATH` (own bundle + icon, looks like a real app notification — `brew install terminal-notifier`), otherwise the built-in **`osascript`** `display notification` (attributed to Apple's Script Editor). The `UNUserNotificationCenter` click API needs a signed `.app` bundle, which thurbox is not, so the `osascript` path is informational | **with `terminal-notifier`** — clicking the banner runs `thurbox-cli ui action session.focus --session <id>`, which asks the running interface to focus it. Without `terminal-notifier` (osascript fallback): no |

`auto` prefers `dbus` whenever a daemon answers, and only falls back to
the Windows toast when no dbus service is reachable. Force a specific
path or disable delivery with `backend = "dbus" | "windows" | "off"`
(`off` is a soft switch distinct from `[features] notifications`, which
stops the dispatcher thread entirely).

Delivery errors are recorded and surfaced by the diagnostic:

```bash
thurbox-cli notify          # show the detected backend + last delivery error
thurbox-cli notify --test   # fire a sample notification to confirm it works
```

| Key | Default | Purpose |
|-----|---------|---------|
| `suppress_for_active` | `true` | skip the notification for the session you're currently viewing |
| `sound` | `true` | play the OS default notification sound |
| `min_interval_secs` | `5` | per-session floor between two notifications (dedup) |
| `backend` | `auto` | delivery backend: `auto` \| `dbus` \| `windows` \| `off` |

### Scripted notifications

`notify send` delivers one notification through the same backend, for an Exec
automation, a CI job or an orchestration script:

```bash
thurbox-cli notify send --title "nightly sync failed" --body "see the run log" \
  [--key nightly-sync --min-interval 3600] [--json]
```

- It honours `[features] notifications = false` and `backend = "off"`: it does
  nothing and exits 0, reporting `"suppressed": "disabled"`.
- `--key` with `--min-interval` (seconds, at most a year; one needs the other)
  drops a repeat of the same key inside the interval, between separate
  processes, and reports `"suppressed": "deduped"`. The key is held in
  `notify-dedupe.json` in the data directory, under a lock every process takes,
  from before the delivery; a delivery that fails gives back its own hold, and
  only that one, so the retry goes out and a newer hold survives a late failure.
- A delivery that fails exits non-zero with `"error"` set. One the backend has
  not finished after 10 seconds counts as failed (`delivery timed out after
  10s`), so a hung notifier never holds up the script that sent it. `--json`
  always reports `delivered`, `suppressed` and `error`.
- It names no session: a click on the banner focuses nothing.
- **The session bus.** On Linux the dbus backend reads
  `DBUS_SESSION_BUS_ADDRESS`, and without it the per-user bus at
  `$XDG_RUNTIME_DIR/bus`. One run from a bare cron or a systemd system unit has
  neither, and must export `XDG_RUNTIME_DIR` (or the bus address) for a
  notification to reach the desktop. `thurbox-cli notify` shows the backend the
  current environment resolves to.

An Exec automation that wants an alert on failure runs `thurbox-cli notify
send` itself.

## Session status

A session's state is its Task's `status.agentState` and `status.sessionState`.
Fleet derives both from the agent's own hooks: each Fleet harness profile
passes its harness status hooks at launch, and they report through
`fleet signal`. Below the hooks, Fleet reads `agentState` off the session's
screen with the harness's screen rules, and `status.activity.source` says
which (`hook`, `screen` or `none`). The interface reads them as follows,
first match wins:

| Task status | Shown as |
| --- | --- |
| `sessionState: Paused` | stopped |
| `sessionState: Lost`, `Gone` | failed |
| `agentState: working` | working |
| `agentState: attention` | blocked |
| `agentState: idle` | idle |
| `sessionState: Running` | running |
| `sessionState: Idle` | idle |
| anything else, or absent | unreported |

thurbox installs no hooks into an agent's own config: Fleet's
[harness profiles](../fleet/controllers/task.md#harness-profiles) do.

The status colours are tunable theme keys (`status_working` /
`status_blocked` / `status_done` / `status_idle` / `status_unreachable` /
`status_running` / `status_unknown` / `status_background` — see
`themes.toml`). `status_unknown` is `unreported`, spelled apart from
`status_idle`, which is the agent's own report that it is at rest.
A screen rule's state keeps its state's colour and is drawn dimmed, with
blocked's diamond hollow (`◇`); a state from neither source draws `?` in
`status_unknown`, and the row says `unknown · no hook, no screen rule`. A
session with no hooks and no screen rules, such as a shell, is never unknown:
it keeps the dot of what the runtime proved.
`status_error` is the role of a failed *command* in the list and of the one
session state that is a failure: `failed`.

## themes.toml

User-defined themes, offered in the `Ctrl+Y` picker alongside the thirty-six
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

## Key bindings

Rebindings live in `ui.json`'s `bindings` (action id → one chord), which
`thurbox-cli config validate` checks.

- Preferred editing path is the **F1 panel** (live capture, conflict
  stealing, immediate persistence), which writes `ui.json`.
- Chord syntax: `[ctrl+][alt+][shift+][cmd+]<key>` where `<key>` is a
  letter, `f1`–`f12`, or a named key (`enter`, `esc`, `tab`, arrows,
  `home`, `end`, `pageup`, `pagedown`, `backspace`, `delete`,
  `insert`). Case-insensitive. `cmd` (aliases `super`, `command`,
  `win`) is the macOS Command key — delivered only by
  kitty-keyboard-protocol terminals (iTerm2 3.5+, kitty, WezTerm,
  Ghostty; not Terminal.app), and only for chords the emulator
  doesn't claim itself.
- Invalid chords and the same chord bound to two actions in overlapping
  contexts are reported at startup (bad entries fall back to defaults).
- **Terminal passthrough.** When a session **terminal is focused**, the
  readline / shell line-editing chords (`Ctrl+A` start-of-line, `Ctrl+E`
  end-of-line, `Ctrl+W` delete-word, `Ctrl+U` kill-line, `Ctrl+R`
  reverse-search, `Ctrl+D` EOF, plus `Ctrl+B/F/O/P/S`) are **forwarded to the
  agent CLI** instead of triggering their thurbox command, so your terminal
  muscle memory works inside a session. Those thurbox commands stay reachable
  from the **session list** (focus it with `Ctrl+H`) and via their `F`-key
  alternates (`F4` theme picker, `F6` settings, `F8` shell tab, `F9` session
  list). Rebinding such an
  action to a key that isn't a bare `Ctrl+<letter>` makes it work in the
  terminal too. Navigation/quit chords (`Ctrl+H/J/K/L`, `Ctrl+Q`, `Ctrl+N`) are
  **never** forwarded — they're how you leave the terminal.
- Action names and defaults: see the
  [Keybindings page](https://thurbox.thurbeen.eu/docs/keybindings.html), the
  live `F1` registry, or `src/kernel/registry.rs`.

## Environment variables

User-set (read by thurbox):

| Variable | Used for |
|----------|----------|
| `XDG_CONFIG_HOME`, `XDG_DATA_HOME` | config/data roots |
| `VISUAL`, then `EDITOR` | `Ctrl+O` editor when `interface.json` names none |
| `SHELL` | the `Ctrl+T` companion shell pane (fallback `/bin/sh`). For a remote/WSL session the pane uses the **host's** `$SHELL` as an interactive login shell (the SSH-login environment), not the local one. |
| `RUST_LOG` | log filter for `thurbox.log` |
| `THURBOX_PERF_LOG` | opt-in performance logging: a one-shot `startup` phase breakdown at first paint, steady-state `perf_window` lines (~10 s cadence), and wall-clock frame/tick timing collection. Any value enables it. See `docs/thurbox/performance.md`. |
| `THURBOX_SOCKET` | overrides the **local** multiplexer socket name, winning over the data-dir derivation below. For test/sandbox tooling: Unix scoping uses `TMUX_TMPDIR`, but psmux (Windows) resolves every `-L <name>` machine-wide, so this is the only way to fully scope an instance there. Remote hosts are unaffected (socket from `hosts.toml`). Empty = unset. |
| `TMUX_TMPDIR` | tmux's own: the directory its `tmux-<uid>/<socket>` goes under. When set, thurbox leaves it alone (test and sandbox tooling scope their servers with it); unset, thurbox sets it to `<data dir>/tmux` for every tmux command it runs — see [Where the tmux socket lives](#where-the-tmux-socket-lives). Empty = unset. |
| `THURBOX_CONFIG_DIR`, `THURBOX_DATA_DIR` | move thurbox's config and data dirs — see [Relocating an instance](#relocating-an-instance-thurbox_data_dir) |
| `THURBOX_UI_DIR` | the interface directory to load instead of `<config dir>/ui`, e.g. a checkout's `ui/` ([plugins.md](plugins.md)) |
| `THURBOX_UI_INSTANCE` | selects a running TUI for `thurbox-cli ui state` and `ui action` when `--instance` is omitted. |
| `WSL_DISTRO_NAME` | set by WSL itself, not by you: it is how thurbox knows which distro it is running inside. That distro is *this machine*, so it is never offered as a host and a `hosts.toml` entry pointing at it is ignored — see [hosts.toml](#hoststoml) |

Set **by** thurbox into every `exec` automation's command, on top of the
firer's environment and the automation's own variables:

| Variable | Set into the command |
|----------|----------------------|
| `THURBOX_AUTOMATION_NAME` | the automation's name |

It runs in its own directory, or your home directory — never in whatever
directory the firer was started in.

Set **at build time** (not runtime):

| Variable | Used for |
|----------|----------|
| `THURBOX_RELEASE_VERSION` | read by `build.rs` to inject the binary version at build (CI release workflow sets it, e.g. `v1.0.0`); absent → falls back to `CARGO_PKG_VERSION` |

Editor resolution order: `interface.json`'s `editor_command` → `$VISUAL` →
`$EDITOR` → error toast.

### Relocating an instance (`THURBOX_DATA_DIR`)

`THURBOX_CONFIG_DIR` and `THURBOX_DATA_DIR` move thurbox's config and data.
An instance whose data dir has moved also gets a **multiplexer socket of its
own** — `thurbox-<digest of the data dir>` (`thurbox-dev-…` on a dev build) —
instead of opening its windows on the server holding your everyday ones.
Killing that instance's server (`tmux -L "$(thurbox-cli version --json | jq -r
.tmux_socket)" kill-server`) touches nothing else.

Three rules keep the ordinary case ordinary:

- **The default instance never moves.** It stays on `thurbox`
  (`thurbox-dev` for a dev build), including when `THURBOX_DATA_DIR` merely
  restates the default.
- **A relocated *config* dir alone changes nothing.** The socket follows the
  data dir.
- **`THURBOX_SOCKET` wins over both**, so anything that needs the socket *by
  name* (the dev sandbox, whose teardown kills it) keeps naming it — **unless it
  was inherited from another instance.** thurbox injects the socket into every
  pane it spawns, paired with `THURBOX_SOCKET_FOR`, the data dir it belongs to.
  A `thurbox-cli` inside that pane which relocates *itself* — a sandbox, a test
  harness, an agent exporting its own `THURBOX_DATA_DIR` — does not match
  that pairing, so the inherited name is dropped and the derivation runs. An
  override you export yourself carries no pairing and still wins outright.

Ask a build which server it is on rather than assuming: `thurbox-cli version
--json` reports the socket in force as `tmux_socket`, and `thurbox-cli config
show` prints it under `Sessions`.

The key is `tmux_socket` on every platform, including native Windows where
psmux serves it: it is the `-L` name every tmux-protocol multiplexer takes.

### Several instances on one server

Two thurbox instances that share a server — the same data dir on one machine,
or a lead over ssh and one locally — show the same sessions, and a pane can be
only one size. There is **no setting** for this: the instance you type into
sizes the pane, and the others show its screen as it is, with blank margins or
cropped to its bottom rows, and say so on the pane's bottom row (`120×41 · sized
by another thurbox · type here to resize`). Focusing a pane there, or typing into
it, hands the size over; when the sizing instance goes, the one left takes its own size back.
The name of the instance sizing a window is the window option `@thurbox_sizer`
(`tmux -L <socket> show-options -w -t <pane> @thurbox_sizer`). Why it works
this way, and what it costs, is ADR-27 in `docs/thurbox/architecture.md`. On a Windows
host (psmux) the last instance to paint a pane still sizes it.

### Where the tmux socket lives

thurbox runs tmux as `tmux -L <socket>`, and tmux puts that socket at
`$TMUX_TMPDIR/tmux-<uid>/<socket>` — `/tmp/tmux-<uid>/thurbox` when nothing
sets `TMUX_TMPDIR`. macOS sweeps old entries out of `/private/tmp`, and a unix
socket's access time is not bumped by `connect()`, so a server in daily use
loses its socket file after a few days (measured: 3 days 18 hours). The server
keeps running with every agent in it, but nothing can reach it any more.

So thurbox keeps the socket out of `/tmp`. Every tmux command it runs carries
`TMUX_TMPDIR=<data dir>/tmux` (created `0700`):

| Where | Socket |
|-------|--------|
| this machine | `~/.local/share/thurbox/tmux/tmux-<uid>/thurbox` (`$XDG_DATA_HOME` respected; `thurbox-dev` for a dev build; a relocated `THURBOX_DATA_DIR` moves it too) |
| an SSH host or WSL distro | the host's own data dir: `${XDG_DATA_HOME:-$HOME/.local/share}/thurbox/tmux/tmux-<uid>/<socket>`, created there by the host's `sh`, or the directory the host's own `thurbox-cli version --json` reports as `tmux_tmpdir` |

The name is unchanged (`-L <socket>`); only the directory moves. Three things
leave it where tmux would put it: a `TMUX_TMPDIR` already set in thurbox's
environment, a data dir so deep that the socket path would not fit in a unix
socket address (~100 bytes; thurbox logs a warning, and a host's `sh` makes the
same check, falling back the same way for every command), and psmux, which has
no socket directory. `thurbox-cli version --json` reports the directory in force
as `tmux_tmpdir` (`null` = tmux's default).

**Finding a server again.** Before thurbox reads a server as absent — when it
first talks to it, when a listing comes back empty, and before every
`new-session` — it looks for the one already running, on the machine the
server runs on (over ssh for a host): first a server answering in the new
directory or in tmux's default one, then a live tmux server still bound to a
missing socket path in either. That one is sent `SIGUSR1`, which tmux documents
as the way to recreate a removed socket, and thurbox reattaches to it. A path
where a socket file exists is never signalled — tmux would unlink it and orphan
whichever server owns it now. Only when no server exists is a new one started.
`thurbox.log` says at INFO which way it went.

A server that exists but cannot be reached — it does not answer `SIGUSR1`
(stopped, or wedged), or a socket file is there and does not answer within
three seconds while a server is bound to it — is an **error**, not an absence.
The three seconds are for a server another relaunch is starting at that moment,
as every session's does at once after a reboot. The session list fails
rather than reading as empty, and no `new-session` runs. A second server beside
it would be the leak again, and the first one's pending `SIGUSR1` would take
the path from the second once it resumed. The error names the pids; see below.

### An orphaned tmux server

A tmux server whose socket file was swept keeps running with every agent in
it, and nothing can reach it: not thurbox, not `tmux -L thurbox ls`. A second
server started under the same name then takes every new window, and the first
keeps its processes running unseen.

Find one by looking for a tmux server whose socket file is not there:

```bash
ps -axo pid,lstart,args | grep '[t]mux -L thurbox'
ls -l /tmp/tmux-$(id -u)/ ~/.local/share/thurbox/tmux/tmux-$(id -u)/
# which file each server is bound to (macOS; on Linux, `ss -xlp | grep tmux`):
lsof -a -U -c tmux -F pn
```

A server whose bound path has no file is orphaned. While nothing else owns that
path, `kill -USR1 <pid>` brings it back, and thurbox does that itself. Once a
second server owns the path, there is no way back in — signalling the old one
would orphan the new one instead — so **killing the orphan is the only
option**: `kill <pid>`. Its agents go with it; their work in a worktree stays on
disk.

## Local UI control

Every running TUI advertises a random instance ID under its data profile's
`ui-control/` directory. Run `thurbox-cli ui instances --json` to list reachable
screens with their PID, local/SSH label and terminal hint.
The control socket sits beside its record, unless that path is too long to
bind (a data profile deep in a checkout's `target/`). Then it sits in a private
`thurbox-ui-<uid>-<digest>` directory under `$XDG_RUNTIME_DIR`, else `/tmp`,
named after the data profile. The record names it, and a reader refuses a
record that names any other place.
`thurbox-cli ui --instance <id> state --json` reports the focused pane,
selected session ID, arranged slots with rects and visible panes, panel state, kernel modal and its
selection, open plugin floats, active search query and result selection, and
the action catalog revision. Plugin-owned details are limited to each plugin's
optional `ui_state()` projection of small scalar values; the Lua store and
terminal contents are never copied into this snapshot. `plugin_state` keys
projections by plugin file path, so two panes with the same display name remain
distinct. If a combined snapshot exceeds the local reply limit, the command
returns an explicit `state_too_large` error. A command
without `--instance` uses the sole reachable screen, or refuses with an
ambiguity error listing IDs when several are running. `THURBOX_UI_INSTANCE`
selects a default for scripts. Closed or crashed screens cannot be targeted;
stale discovery records are removed when discovered.

`thurbox-cli ui --instance <id> watch --json` writes an initial snapshot and
then JSON lines for changes to focus, layout, overlays, selection, search, and
action outcomes. `--since <revision>` resumes after a known revision. The
per-instance buffer holds 256 deltas; an expired or future cursor yields a
`resync_required` response containing a fresh snapshot. `--once` returns one batch as
a JSON document for polling callers. This stream is UI state only; session
events are Fleet's (`fleet watch --json`).

List the target's live actions and their argument types, ownership, effect,
destructive classification, availability and current chords with
`thurbox-cli ui --instance <id> actions --json`. `thurbox-cli schema --instance
<id> --json` includes those same descriptors beside the CLI command tree.
Both read the running interface, so reloading a plugin updates them together.
When no TUI is running, `schema` marks its UI portion `no_running_ui` and still
lists headless CLI commands. With several running TUIs, select one explicitly.
The two initial typed actions are:

```sh
thurbox-cli ui --instance <id> action session.focus --session <fleet>:<task> --json
thurbox-cli ui --instance <id> action search.open --query 'error handling' --json
```

`session.focus` requires a session visible to the target TUI and a focusable
agent pane. `--session` takes a session's id (`box:review`), or a bare Task
name when exactly one Fleet holds a Task of that name; a name several Fleets
hold is refused (`ambiguous_target`), naming the ids it could mean. `search.open`
opens the strip and sets its query; calling it again replaces the query without
closing the strip. The reply comes from the target's event loop after it applies
or refuses the action.

Any non-destructive declared action can be called by its catalog name. Use
`--arg name=value` for an argument; `--session` and `--query` remain shortcuts
for the first two actions. A destructive action needs an explicit session, named the same way.
Its first request exits with a structured `confirmation_required` result and an
opaque `result.error.ticket`; it changes nothing. Run `thurbox-cli ui --instance
<id> confirm <ticket> --json` as a separate step. A ticket expires after 30
seconds, works once for the same local peer and instance, and is refused if
the action catalog or target changes. A confirmation queues the normal session
operation; its later success or failure is reported through the ordinary TUI
command status. An addressed destructive key cannot bypass this step.
For input owned by an active modal or plugin, use addressed operations such as
`thurbox-cli ui --instance <id> input modal --key esc`,
`input search --input-text 'term'`, or `input agent --scroll down`. These operations
refuse an inactive target. Keys and text are refused for session terminals;
agent terminal bytes stay on the session input path.

The local control audit lives at `ui-control/audit.jsonl` in the data profile.
It is owner-only and bounded to roughly 1 MiB. It records time, instance,
peer, action ID, target session ID, outcome and request ID for external action
attempts and decisions. It omits arguments such as search queries, addressed
input, terminal text and secrets. If audit storage is unavailable, destructive
control refuses before dispatch. A bounded worker queue writes the records;
destructive requests wait for its result with a short deadline. A file lock
keeps rollover and append together across running instances.

The `ui actions` descriptor schema is versioned separately from the CLI. For
schema version 1, bundled action names, argument names and meanings remain
stable; new optional fields and new actions can appear. Clients should ignore
unknown fields and rediscover the live catalog after `catalog_revision` moves.
Plugin-owned actions and `plugin_state` projections are governed by their
plugins and may disappear on reload. `ui state` exposes the active search query,
so scripts should treat its output and the event stream as private even though
terminal contents are omitted. No `ui capture` command is provided: visible
terminal cells can contain credentials or private prompts, and bounded output
alone would not make a useful redaction boundary.

The control channel is a Unix socket in a user-owned `0700` directory with a
peer UID check, or a local Windows named pipe with a current-user ACL and remote
clients rejected. Requests are length-framed JSON, limited to 16 KiB requests
and 256 KiB replies, with a
bounded queue; each client has a two-second deadline. The interface does not
listen on TCP. A CLI on another machine must be run on the TUI's host.
If the local endpoint cannot start, the TUI still runs and shows a notice after
other startup notices;
`ui instances` will not list it.

## Versioning

The TOML files carry a `config_version = 1` marker so a future format change
can migrate them; current files are version 1 and the field is optional.
thurbox keeps no database: sessions are Fleet's (ADR-41).
