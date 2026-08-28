# Configuration Reference

Every knob thurbox reads, where it lives, and how it behaves. One file
per audience/lifecycle: hand-edited registries are TOML, the
machine-written keybindings are JSON, and concurrently-written runtime
state lives in SQLite (see ADR-8/ADR-19 in `ARCHITECTURE.md` for the
rationale).

`ui/plugins.toml` and `ui/plugins.lock` are the one pair that share a
format and split on lifecycle instead: the spec is composition you
maintain, the lock is a record nothing hand-edits. It is the same split
`.bundled.json` and `ui.json` already make, and the reason is the same —
a merge conflict in the record must not dirty the half a person wrote.

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
| `~/.config/thurbox/hooks.toml` | TOML | you | on each session operation | **session lifecycle hooks** — your commands, run before/after a session is created, deleted, restarted or restored |
| `~/.config/thurbox/ui/` | Lua | you | **live** (watched, 120 ms debounce; `F10` forces) | **the interface itself** — one file per pane, plus `layout.lua`, `lib/`, `AGENTS.md`/`README.md` for whoever edits it, and a directory per plugin installed from a repository (its own working copy, `.git` included) |
| `~/.config/thurbox/ui/plugins.toml` | TOML | you (or `thurbox-cli plugin`) | on each `plugin` command | **what the interface is composed of**: a source, a destination file and an optional pin, per installed pane |
| `~/.config/thurbox/ui/plugins.lock` | TOML | `thurbox-cli plugin` | on each `plugin` command | what each entry resolved to, and the digest of every file delivered. Machine-written — commit it beside the spec and the same interface reproduces elsewhere |
| `~/.config/thurbox/ui.json` | JSON | `F1` / Interface tab (or you) | startup | your decisions *about* the interface: rebound chords, plugins turned off, files trusted, plugin settings. Written back only by a registry that **read** it (`registry::Origin`), so a process holding no decisions cannot empty it |
| `~/.config/thurbox/extensions/<name>.toml` | TOML | `thurbox-cli extension install` | startup + tick | extension manifests (self-healed resources) |
| `~/.config/thurbox/keybindings.json` | JSON | — | **never** | v1's chord overrides. **Ignored**: rebindings live in `ui.json`. Left alone rather than deleted, so going back to 1.x still finds it |
| `~/.local/share/thurbox/thurbox.db` | SQLite | thurbox | live | sessions, automations, tasks, theme, editor command |
| `~/.local/share/thurbox/thurbox.log` | text | thurbox | — | logs (incl. config warnings) |

`agents.toml` and `settings.toml` reload **live**: the TUI polls their
mtime (~1/s) and applies edits with a confirmation toast — no restart.
The `ui/` directory reloads live too, but on a **filesystem watcher**
(120 ms debounce) rather than a poll, and `F10` forces one. For
`settings.toml` the flags that apply live are `shell_pane`, `perf_hud`
and `soft_delete`; the restart-only values stay
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
| Run my own command when a session is created/deleted/restarted/restored, or refuse one | `hooks.toml` | [hooks.toml](#hookstoml) |
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
(`claude`, `codex`, `antigravity`, `opencode`, `aider`, `copilot`, `vibe`, `pi`,
`omp`) on
first run; edit or add `[[agents]]` entries to support any CLI — no
recompile. A malformed `[[agents]]` entry is skipped (with a toast
naming it) and the rest still load; only a document-level syntax error
falls back to the built-ins. Either way the error is shown.

The file is **seeded once** and never rewritten — so it stays yours to
edit, but a thurbox **update that adds a new built-in agent does not
merge it into an existing `agents.toml`**. To pick up a newly-bundled
agent, add its `[[agents]]` block by hand (copy it from this file's
built-in list) or delete `agents.toml` to re-seed the full set. The
matching status hook is wired automatically once the agent's config dir
exists — the built-in hooks extension self-heals on every startup/tick,
independent of `agents.toml`.

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
# hook_schema = "claude"    # optional: name the hook FAMILY this CLI speaks so
                            #   the built-in hooks extension wires its status
                            #   hooks under this custom agent's name too
```

`{id}` is substituted with the thurbox-generated session UUID. `{home}`
is substituted with the resolved home dir at spawn time (the remote home
for an SSH/WSL host) — for an agent that wants a session *file path*
rather than a bare id. The built-in `omp` (Oh My Pi) uses it: it generates
its own internal id and won't take thurbox's, but its `--session <path>`
creates a fresh session at a missing path, so thurbox maps its UUID to a
deterministic `--session {home}/.omp/agent/sessions/thurbox-{id}.jsonl`
(creation) / `--resume` the same (restart). `{home}` is expanded by
thurbox, not the shell — args are POSIX-quoted, so a literal `~` would
never expand. `omp` ships no `fork_args`, so `Ctrl+F` starts a fresh
session (OMP has no way to pin a fork's target file to a thurbox UUID).
Groups
are emitted only when their driving value exists; precedence is
fork > resume > new-session. See the seeded file's comments and
CLAUDE.md's *Agent Definitions* section for the `resume_latest`
semantics.

`hook_schema` is optional. Custom agents are agent-neutral, so the built-in
**hooks** extension normally wires status hooks only for the built-ins it knows
by name. Set `hook_schema = "claude"` on a **rebranded** agent (one whose
`command` runs `claude` under a different `name`) and it inherits claude's hook
wiring — the `--settings` patch locally, and the same rewrite on a remote/WSL
host. It names the *family* to imitate, not a boolean; today the useful value is
`"claude"` (the only family wired via a per-agent arg patch — codex/opencode/
antigravity/vibe/copilot are wired through their own config dir, so a rebrand
sharing that dir already reports status).

The seeded file also ships two commented, copy-pasteable templates
below the built-ins — **Add your own agent** (every field annotated)
and **Pin a model** (a `claude-opus` variant baking `--model opus`
into `args`). Both stay commented, so a fresh install still resolves
to exactly the nine built-ins.

For **each built-in's** exact config and runtime behavior (resume/fork
semantics, ID model, and which status-hook mechanism it uses), and the
checklist for **adding a new built-in**, see
[AGENTS.md](AGENTS.md).

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
| `share_sessions` | no | `true` | the host's own database is the record of its sessions: mirrored here, operated through the host's `thurbox-cli` (provisioned under `~/.local/share/thurbox/bin/` — `thurbox-dev/bin/` for a dev build — there when missing); `false` = drive the host from here as before |

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

## hooks.toml

Declares **session lifecycle hooks**: your own shell commands, run by
thurbox before and after it creates, deletes, restarts or restores a
session. Seeded fully commented-out (a fresh install runs nothing); read
**each time an event fires**, so an edit is in force at the next
operation with no restart. Malformed file → no hooks run, warning in the
log, and `config validate` fails.

> Not the `hooks/` **directory** beside it. That is the home of the
> built-in `hooks` *extension*, which installs status-hook files **into**
> the agent CLIs so they can tell thurbox what they are doing
> (see [Session status](#session-status)). `hooks.toml` is the other
> direction: thurbox telling *your* scripts what *it* is doing.

```toml
[[hooks]]
event = "session.pre_create"
command = 'case "$THURBOX_BRANCH" in main|master) echo "refusing: protected branch" >&2; exit 1;; esac'

[[hooks]]
event = "session.post_create"
command = '[ -n "$THURBOX_CWD" ] && cp -n .env.local "$THURBOX_CWD/.env"; true'
timeout_secs = 120
```

| Field | Required | Default | Purpose |
|-------|----------|---------|---------|
| `event` | yes | — | one of the eight events below |
| `command` | yes | — | run through `sh -c` (`cmd /C` on Windows) |
| `timeout_secs` | no | `30` | the hook is killed after this long |

**Events** — a `pre_*` fires before the operation has any side effect, a
`post_*` after it has fully succeeded (never after a failure):

| Operation | Events | Fired by |
|-----------|--------|----------|
| create (incl. fork) | `session.pre_create` / `session.post_create` | the creation flow, `Ctrl+F`, `thurbox-cli session create`, a `spawn` automation, an extension's sessions |
| delete (soft or force) | `session.pre_delete` / `session.post_delete` | `Ctrl+D`, `thurbox-cli session delete [--force]`, extension uninstall |
| restart | `session.pre_restart` / `session.post_restart` | `Ctrl+R`, `thurbox-cli session restart` |
| restore (incl. undo) | `session.pre_restore` / `session.post_restore` | `Ctrl+Z`/`Ctrl+U`, `thurbox-cli session restore` |

Hooks fire **once per operation whichever interface asked**, because
every interface ends in the same pipeline (`session_ops`). Hooks for one
event run one at a time, in file order.

**A `pre_*` hook can refuse.** Exit non-zero (or exceed the timeout) and
the operation is aborted before it has done anything — no worktree, no
process, no row changed — with the hook's command, exit status and the
tail of its stderr as the reported reason (the in-flight error in the
TUI, the error and exit status of `thurbox-cli`). Later hooks for that
event do not run. **A `post_*` hook is informational**: every one runs,
a failure is logged to `thurbox.log` and carried in the CLI's JSON
(`hook_failures`), and never fails the operation. Each `post_*` is also
delivered to interface plugins as an event of the same name (`events = {
"session.post_create" }` + `on_event`; `docs/PLUGINS.md` → Events) — the
Lua side of the same vocabulary, informational for the same reason.

**What a hook receives.** Environment variables — **unset** (never
empty) when the fact is not known at that moment:

| Variable | Meaning |
|----------|---------|
| `THURBOX_HOOK_EVENT` | the event name, e.g. `session.post_create` |
| `THURBOX_SESSION` | the thurbox session id (at `pre_create`: the id it will have if creation succeeds) |
| `THURBOX_SESSION_ID` | the agent's own conversation id |
| `THURBOX_SESSION_NAME` | the session name |
| `THURBOX_AGENT` | the agent name |
| `THURBOX_REPO` | the primary repository path |
| `THURBOX_CWD` | the directory the agent runs in (the worktree, or the symlink workspace of a multi-repo session); unset at `pre_create` |
| `THURBOX_BRANCH` / `THURBOX_BASE_BRANCH` | the worktree branch and what it was created from (base: create events only) |
| `THURBOX_HOST` | the remote host name; unset for a local session |
| `THURBOX_PARENT_SESSION` | the parent session id (a fork, or `--parent`) |
| `THURBOX_TASK` | the originating task id, for a task-spawned session |
| `THURBOX_CONFIG_DIR` / `THURBOX_DATA_DIR` | so a `thurbox-cli` run inside the hook hits the database of the thurbox that fired it |

The same facts — plus `worktrees` (`repo_path`, `worktree_path`,
`branch`), `additional_dirs`, `force` (delete) and `force_deleted`
(restore) — arrive as **one JSON object on stdin** (`jq -r .cwd`).

**Where and how it runs.** In the primary repository when that is a
directory on this machine, otherwise in thurbox's own working directory
— the repository is the one path that exists at every event (at
`pre_create` the worktree is not made; at `post_delete` it is gone).
With **no terminal**: stdin is the JSON, stdout/stderr are captured and
only their tail (500 chars) is reported, so a hook can neither draw on
nor read from the TUI's screen. A hook for a **remote** (SSH/WSL)
session still runs **locally**; `THURBOX_HOST` names the host and the
paths are the host's — `ssh "$THURBOX_HOST" …` from the hook is your
call.

`thurbox-cli config validate` strict-parses the file; `config show`
lists the hooks in force.

## settings.toml

> The `F6` / `Ctrl+,` panel edits this file: core settings are listed above
> whatever plugins declared, `⟳` marks the ones that wait for the next
> launch, and `Ctrl+S` saves — written back through `toml_edit`, so the
> comments below survive.
>
> Four flags gate surfaces the interface **no longer draws**: `code_review`,
> `file_viewer`, `info_panel` and `tasks` (which still gates its CLI). They
> are accepted and preserved untouched so an existing file does not fail
> `thurbox-cli config validate`, and simply not listed in the panel, since a
> row that gates nothing reads as broken. `automations` is honoured, but it
> arms the headless heartbeat rather than an in-TUI scheduler.

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
three_panel_min_cols  = 120    # accepted and ignored (v1's third column)
audit_retention_days  = 90     # audit-log history kept (pruned on startup)

[features]
shell_pane    = true
perf_hud      = true
soft_delete   = true
automations   = true
mouse         = true
notifications = true
version_check = true           # on by default (1.0): makes a network call
auto_update   = true           # on by default (1.0): downloads + replaces binaries

[notifications]
also_on_waiting     = false    # also fire when a session finishes (Working → Done)
suppress_for_active = true     # skip the session you're currently viewing
sound               = true     # play the OS default notification sound
min_interval_secs   = 5        # per-session floor between notifications
```

### `[features]` — whole-feature switches

Switch off behaviour that reaches outside the interface. All default to
`true` — as of 1.0 that includes `version_check` and `auto_update`, which
both reach the network (they were opt-in before 1.0). `shell_pane`,
`perf_hud` and `soft_delete` apply **live** on save; `automations`,
`mouse`, `notifications`, `version_check` and `auto_update` take effect on
the next launch. Data is never touched, so re-enabling a flag is lossless.

**A pane is not switched off here.** Panes are files, so turning one off
is `space` on its row in the Interface tab (`Ctrl+,` then `]`), recorded
in `ui.json`. That does strictly more than a flag could: it works for a
plugin *you* wrote, which no compiled-in flag could know about.

Five keys are **accepted and ignored**: `tasks`, `file_viewer`,
`info_panel`, `code_review` and `global_search`. They gated v1 panes the
binary no longer draws, and nothing reads them now — not even the plugins
that give `code_review` and `info_panel` back
([`thurbox-code-review`](https://github.com/Thurbeen/thurbox-code-review),
[`thurbox-info-panel`](https://github.com/Thurbeen/thurbox-info-panel)),
which are switched on and off from the Interface tab like any other pane.
They are still parsed rather than rejected, so an existing `settings.toml`
keeps loading instead of failing on an unknown key — but setting one has no
effect in either direction. Same for `three_panel_min_cols` above.

| Key | Default | Controls |
|-----|---------|----------|
| `shell_pane` | `true` | per-session shell toggle (`Ctrl+T`) |
| `perf_hud` | `true` | perf HUD overlay (`F12`): live perf counters + frame/tick timing (see `docs/PERFORMANCE.md`) |
| `automations` | `true` | TUI schedule firing + heartbeat arming (the CLI stays fully functional) |
| `mouse` | `true` | mouse capture: clicks, wheel, drag-select, hover, scrollbars |
| `notifications` | `true` | OS desktop notifications when a session needs attention |
| `soft_delete` | `true` | TUI `Ctrl+D` soft-deletes (Ctrl+Z undo); off = hard delete after a confirmation prompt |
| `version_check` | `true` | GitHub update check: TUI header "update available" badge + `thurbox-cli version --check` |
| `auto_update` | `true` | Silent self-update **within the current major**: download + verify + replace the binaries on startup + `thurbox-cli update`; also auto-refreshes stale extensions |

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

`version_check` (on by default for 1.0) enables the update check — it
makes a network call. On launch the TUI reads a cached result
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

`auto_update` (on by default for 1.0) goes a step further than
`version_check`: instead of just showing a badge, the TUI **silently
updates itself** on startup. On every
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
same update on demand (with `--force` to bypass the up-to-date, dev-build and
major-version guards); dev builds (`0.0.0-dev`) never auto-update. The default install
location (`~/.local/bin`) is user-writable; a system-wide install in a
root-owned directory will fail the replace (logged, non-fatal). `version_check`
and `auto_update` are independent — enable either or both.

**Auto-update never crosses a major version.** A 1.x install is told that 2.x
exists (the badge, and `thurbox-cli version --check`, both report it) and is
never moved onto it; it keeps updating along 1.x. This is not conservatism about
version numbers — 2.x replaced v1's compiled-in interface with the Lua plugin
kernel, so a silent crossing would hand someone a different program under the
same binary name. `thurbox-cli update --force` is the deliberate way across, and
the first v2 launch of a profile with v1 history still asks before anything
changes. The guard is `agent::version_check::crosses_major`, and it applies to
every future major too.

> Only a binary that *carries* the guard is protected by it. Releases up to
> v1.8.7 predate it, so an existing 1.8.x install with `auto_update = true` will
> still take 2.x on its next launch until a 1.x maintenance cut ships this
> commit. Until then, `auto_update = false` in `settings.toml` is the reliable
> hold — which is exactly what the v1→v2 consent gate writes when you decline.

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
`status_blocked` / `status_done` / `status_idle` / `status_error` /
`status_unreachable` — see `themes.toml`).

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
repo_path = "{home}"            # absolute, `~`-relative, or `{home}`; resolved
                                #   to an absolute path at install

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
| `editor_mode` | `thurbox-cli editor mode <auto\|terminal\|gui>` | how `Ctrl+O` runs the editor: `auto` (default) detects terminal vs GUI and gives terminal editors a real TTY (tmux popup / TUI suspend); `terminal` forces the TTY path; `gui` forces detached |
| `active_extensions` | `thurbox-cli extension activate/deactivate` | JSON array of active extensions to self-heal |
| `builtin_hooks_optout` | `thurbox-cli extension deactivate hooks` | `1` when the user opted out of the auto-activated hooks extension |
| `perf_snapshot` | the TUI, while perf timing is active (`THURBOX_PERF_LOG` or an open perf HUD) | JSON perf snapshot read by `thurbox-cli perf` (see `docs/PERFORMANCE.md`) |

These are in the DB rather than a file because they are written
concurrently by multiple thurbox processes (TUI, CLI, MCP) and picked
up live via `PRAGMA data_version` polling.

## Environment variables

User-set (read by thurbox):

| Variable | Used for |
|----------|----------|
| `XDG_CONFIG_HOME`, `XDG_DATA_HOME` | config/data roots |
| `VISUAL`, then `EDITOR` | `Ctrl+O` editor when `editor_command` is unset |
| `SHELL` | the `Ctrl+T` companion shell pane (fallback `/bin/sh`). For a remote/WSL session the pane uses the **host's** `$SHELL` as an interactive login shell (the SSH-login environment), not the local one. |
| `RUST_LOG` | log filter for `thurbox.log` |
| `THURBOX_PERF_LOG` | opt-in performance logging: a one-shot `startup` phase breakdown at first paint, per-session `restore_adopt`/`adopt_split` lines, steady-state `perf_window` lines (~10 s cadence), and wall-clock frame/tick timing collection. Any value enables it. See `docs/PERFORMANCE.md`. |
| `THURBOX_SOCKET` | overrides the **local** multiplexer socket name, winning over the data-dir derivation below. For test/sandbox tooling: Unix scoping uses `TMUX_TMPDIR`, but psmux (Windows) resolves every `-L <name>` machine-wide, so this is the only way to fully scope an instance there. Remote hosts are unaffected (socket from `hosts.toml`). Empty = unset. |

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
| `THURBOX_SOCKET` | the multiplexer socket that instance's sessions live on, so an in-session `thurbox-cli` reaches the same server instead of re-deriving one from the session's own environment |

Set **by** thurbox into every [lifecycle hook](#hookstoml) it runs
(`session_ops::lifecycle_hooks`), beside `THURBOX_SESSION`,
`THURBOX_SESSION_ID`, `THURBOX_TASK` and the location overrides above:

| Variable | Set into hook process |
|----------|-----------------------|
| `THURBOX_HOOK_EVENT` | the event, e.g. `session.pre_delete` |
| `THURBOX_SESSION_NAME`, `THURBOX_AGENT` | the session's name and agent |
| `THURBOX_REPO`, `THURBOX_CWD` | the primary repository, and the directory the agent runs in |
| `THURBOX_BRANCH`, `THURBOX_BASE_BRANCH` | the worktree branch and its base |
| `THURBOX_HOST` | the remote host name (local: unset) |
| `THURBOX_PARENT_SESSION` | the parent session id |

Set **at build time** (not runtime):

| Variable | Used for |
|----------|----------|
| `THURBOX_RELEASE_VERSION` | read by `build.rs` to inject the binary version at build (CI release workflow sets it, e.g. `v1.0.0`); absent → falls back to `CARGO_PKG_VERSION` |

Editor resolution order: DB `editor_command` → `$VISUAL` → `$EDITOR` →
error toast.

### Relocating an instance (`THURBOX_DATA_DIR`)

`THURBOX_CONFIG_DIR` and `THURBOX_DATA_DIR` move thurbox's config and its
database. Because the database *is* the record of which sessions exist, an
instance whose data dir has moved also gets a **multiplexer socket of its own**
— `thurbox-<digest of the data dir>` (`thurbox-dev-…` on a dev build) — instead
of creating its windows on the server holding your everyday sessions. That is
what makes a scratch instance safe to run a real `session create` in, and it is
also why its `automation-heartbeat` window is reclaimable: killing that
instance's server (`tmux -L "$(thurbox-cli version --json | jq -r .tmux_socket)"
kill-server`) takes the whole instance with it and touches nothing else.

Three rules keep the ordinary case ordinary:

- **The default instance never moves.** It stays on `thurbox`
  (`thurbox-dev` for a dev build), including when `THURBOX_DATA_DIR` merely
  restates the default — which is exactly what thurbox injects into every
  session it spawns.
- **A relocated *config* dir alone changes nothing.** It shares the default
  instance's database, and therefore its sessions and their server.
- **`THURBOX_SOCKET` wins over both**, so anything that needs the socket *by
  name* (the dev sandbox, whose teardown kills it) keeps naming it.

Ask a build which server it is on rather than assuming: `thurbox-cli version
--json` reports the socket in force as `tmux_socket`, and `thurbox-cli config
show` prints it under `Sessions`.

**An instance relocated before this existed is a new instance.** Its old
sessions are still on the default server, and its database still lists them —
on the new socket they read as sessions whose window is gone. There is no
migration: point that instance back at the old server with
`THURBOX_SOCKET=thurbox` if you want them, or let it create fresh ones.

## Versioning

The SQLite schema migrates automatically (`schema_version` in
`metadata`). The TOML files carry a `config_version = 1` marker so a
future format change can migrate them too; current files are version 1
and the field is optional.
