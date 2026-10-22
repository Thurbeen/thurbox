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
| `~/.config/thurbox/agents.toml` | TOML | you | **live** (content poll) | coding-agent CLI definitions |
| `~/.config/thurbox/hosts.toml` | TOML | you | startup | remote SSH hosts + local WSL distros |
| `~/.config/thurbox/settings.toml` | TOML | you + `Ctrl+,` panel | **live** (feature flags) / startup (rest) | tuning knobs + feature flags |
| `~/.config/thurbox/themes.toml` | TOML | you | startup | custom theme palettes |
| `~/.config/thurbox/hooks.toml` | TOML | you | on each session operation | **session lifecycle hooks** — your commands, run before/after a session is created, deleted, restarted or restored |
| `~/.config/thurbox/ui/` | Lua | you | **live** (watched, 120 ms debounce; `F10` forces) | **the interface itself** — one file per pane, plus `layout.lua`, `lib/`, `AGENTS.md`/`README.md` for whoever edits it, and a directory per plugin installed from a repository (its own working copy, `.git` included) |
| `~/.config/thurbox/ui/plugins.toml` | TOML | you (or `thurbox-cli plugin`) | on each `plugin` command | **what the interface is composed of**: a source, a destination file and an optional pin, per installed pane |
| `~/.config/thurbox/ui/plugins.lock` | TOML | `thurbox-cli plugin` | on each `plugin` command | what each entry resolved to, and the digest of every file delivered. Machine-written — commit it beside the spec and the same interface reproduces elsewhere |
| `~/.config/thurbox/ui.json` | JSON | `F1` / Interface tab (or you) | startup | your decisions *about* the interface: rebound chords, plugins turned off, files trusted, plugin settings, and the action band's order (`"pills": { "<action>": <priority> }`, replacing what the declaring plugin chose — an entry naming an action nothing declares is ignored, not drawn). Written back only by a registry that **read** it (`registry::Origin`), so a process holding no decisions cannot empty it |
| `~/.config/thurbox/extensions/<name>.toml` | TOML | `thurbox-cli extension install` | startup + tick | extension manifests (self-healed resources) |
| `~/.config/thurbox/keybindings.json` | JSON | — | **never** | v1's chord overrides. **Ignored**: rebindings live in `ui.json`. Left alone rather than deleted, so going back to 1.x still finds it |
| `~/.local/share/thurbox/thurbox.db` | SQLite | thurbox | live | sessions, automations, tasks, theme, editor command |
| `~/.local/share/thurbox/thurbox.log` | text | thurbox | — | logs (incl. config warnings). Rotated daily into `thurbox.log.<date>`; the 30 most recent are kept and older ones deleted at startup |
| `~/.local/share/thurbox/automation-tick.json` | JSON | `automation tick` | each tick | the last tick's time, outcome, error, fire counts and the automations it could not fire, which `thurbox-cli runtime status` reports — the heartbeat keeper throws the tick's own output away |
| `~/.local/share/thurbox/automation-tick.log` | text | `automation tick` | each failed tick | one timestamped line per tick that failed (the database would not open, the due scan failed, a panic), and per automation a tick could not fire (its lock or claim could not be written). Capped at 64 KB, its older half dropped past that |

`agents.toml` and `settings.toml` reload **live**: the TUI checks them about
once per second and applies edits without a restart. The agent registry is
compared by content; settings use mtime. A successful edit shows a toast.
An `agents.toml` reload updates the picker, default, and session-status
coverage together. A missing file, an invalid file, or one with no usable agents
leaves the last good registry in force and reports the error; correcting it is picked
up by the next poll. Existing session records are not changed.
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
| Reorder the action band's buttons | `ui.json` `pills` | [Files at a glance](#files-at-a-glance) |
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
additional_dir_args = ["--add-dir", "{dir}"] # repeated per multi-repo member
resume_latest = false       # true = id-less "resume last session in cwd"
# hook_schema = "claude"    # optional: name the hook FAMILY this CLI speaks so
                            #   the built-in hooks extension wires its status
                            #   hooks under this custom agent's name too
telemetry_env = { CLAUDE_CODE_ENABLE_TELEMETRY = "1" }  # only while [telemetry] is on
# telemetry_args = []       # appended only while [telemetry] is on
```

`telemetry_env` and `telemetry_args` apply only while `settings.toml`'s
[`[telemetry]`](#telemetry--opentelemetry-for-sessions) is on, with
`{otlp_endpoint}` and `{otlp_protocol}` substituted. The built-ins `claude`,
`codex` and `vibe` carry them; an entry that omits both keys and matches a
built-in's `name` and `command` gets the built-in's in memory, and an explicit
empty value opts out.

`additional_dir_args` is optional and normally empty. It scopes each resolved
member checkout of a multi-repo session, with `{dir}` substituted as one
argument; single-repo launches emit none. The built-in `claude` group above is
also supplied in memory to existing `name = "claude"`, `command = "claude"`
entries that omit it. Explicit `[]` opts out, and custom definitions can declare
their own group. Restart affected sessions after upgrading. This changes
directory scope, preserving the permission mode and deny/ask rules; see
[Multi-repo directory scope](AGENTS.md#multi-repo-directory-scope).

`{id}` is substituted with the thurbox-generated session UUID. `{home}`
is substituted with the resolved home dir at spawn time (the remote home
for an SSH/WSL host) — for an agent that wants a session *file path*
rather than a bare id. The built-in `omp` (Oh My Pi) uses it: it generates
its own internal id and won't take thurbox's, but its `--session-dir <dir>`
keeps a session in a directory of thurbox's choosing, so thurbox maps its
UUID to a deterministic `--session-dir {home}/.omp/agent/sessions/thurbox-{id}`
(creation) / the same plus `--continue` (restart). An `agents.toml` seeded
by an older thurbox still carries `--session`/`--resume` on a `.jsonl` file,
which OMP refuses for a file that does not exist yet; the loader replaces
that exact pair in memory, and any hand-edited omp entry is left as is. `{home}` is expanded by
thurbox, not the shell — args are POSIX-quoted, so a literal `~` would
never expand. `omp` ships no `fork_args`, so `Ctrl+F` starts a fresh
session (OMP has no way to pin a fork's target file to a thurbox UUID).
Groups
are emitted only when their driving value exists; precedence is
fork > resume > new-session. See the seeded file's comments and
the `thurbox-agents` skill for the `resume_latest` semantics.

### How `command` is resolved

For a **local** session, thurbox resolves a bare `command` against **its own
`PATH`** and hands the multiplexer the absolute path it found. It does that
because the multiplexer would otherwise resolve the name itself, in an
environment thurbox neither chose nor can see:

- tmux copies the *client's* `PATH` into a new pane only for an **unattached**
  client, so a window created over thurbox's (attached) control-mode connection
  — a restart, a plugin program, the companion shell pane — got the `PATH` of
  whatever first started the tmux **server**.
- a window command that reaches tmux as a **single** argument is run by tmux's
  `default-shell`, not `execvp` — so an agent with no args was launched by a
  shell thurbox never chose, under that shell's `PATH` and quoting.

Under zsh/bash those `PATH`s agree, because the interactive additions live in
`~/.zshenv` / `~/.profile`, which any shell that starts a tmux server sources.
Under **fish** they do not: `fish_add_path` writes `fish_user_paths`, which only
fish applies — so a server started from anything else never sees the agent, for
the life of that server, and the spawn fails with a shell's `command not found`
(exit **127**; an `execvp` failure is exit 1).

That is not the only thing a 127 there can mean, and thurbox does not read the
exit status as the verdict at all — see
[FEATURES.md](FEATURES.md#session-creation) for the hook that also produces
one.

Resolution is **best-effort and never a new way to fail**: a `command` that is
already a path, that nothing on `PATH` matches (a shell function or alias, or a
binary installed after thurbox started), or that runs on Windows is passed
through verbatim, exactly as before. A **remote** (SSH/WSL) session is not
resolved either — its `PATH` is the host's: the host's login-shell `PATH`
(read once per host, see [hosts.toml](#hoststoml) → `path_prepend`) ahead of
what ssh / `wsl.exe` give a command, and its window command is wrapped in a
login shell.

Only **absolute** `PATH` entries are considered. That excludes the empty entry
POSIX reads as "the current directory" (`:/usr/bin`, or a stray trailing colon),
because the resolved path is handed to a process with a working directory of its
own — honouring it would let a `claude` sitting in the repo you are working on
shadow the real one, which is the dependence this removes rather than moves.

### What happens when it is *not* installed

thurbox starts with no multiplexer and no agent on the machine, and that is
deliberate — browsing, reading and configuring must work on a fresh box. So the
same lookup runs a second time, as a **preflight**, in the places where knowing
early is worth something:

- the **create-session flow** marks an agent whose `command` resolves nowhere
  (`⚠ not installed`) and says so on the step that offers it, and states a
  missing multiplexer from the flow's first step. It never blocks: the answer is
  a warning on the choice, because a `command` may still be launchable (a shell
  function, or something installed a second later).
- the **empty session list** — the one screen a first run always reaches — says
  the multiplexer is missing, with the fix.
- a **spawn failure** caused by a missing binary names the binary, the
  directories thurbox searched and the one thing to do about it, instead of the
  launcher's errno or the multiplexer's exit code.
- `thurbox-cli doctor` answers the whole question directly: the multiplexer and
  its version, every registered agent's `command`, the launcher for each
  configured host, and the full search path. For a host that shares sessions it
  also asks the host's `thurbox-cli` for its version (one round trip, never an
  install) and warns when sharing cannot work — a CLI behind this one's schema,
  none at all, or no answer — naming `thurbox-cli session sync --host <name>`,
  which installs a matching one or reports why it cannot. It is the companion to
  `thurbox-cli session doctor`, which asks whether an *existing* session's status
  hooks are wired.

The probe is a `stat` per absolute `PATH` entry per binary — no process spawn —
and it runs on the kernel's own schedule behind a 10-second window, never on a
render, a keystroke or a list row. Two things are reported as **unknown** rather
than missing, because in both cases nothing was looked at: a **remote host**,
whose binaries live on the host, and a **relative** `command` such as
`./bin/agent`, which is resolved by whoever launches it from the *session's*
own directory. Answering the latter from thurbox's working directory would call
a binary that launches fine missing, and one that does not present — the same
"absolute only" rule the resolver above is written under, for the same reason.

The advice is platform-specific and never invented. A Windows user is pointed at
[psmux](https://github.com/psmux/psmux), never at `tmux`; where the install
command depends on a distribution, the package name is given and the link is
tmux's own [install page](https://github.com/tmux/tmux/wiki/Installing) rather
than a guessed invocation. A missing **agent** is answered with the `agents.toml`
entry that decides what gets run, because thurbox bakes in no knowledge of any
agent's installer.

`hook_schema` is optional. Custom agents are agent-neutral, so the built-in
**hooks** extension normally wires status hooks only for the built-ins it knows
by name. Set `hook_schema = "claude"` on a **rebranded** agent (one whose
`command` runs `claude` under a different `name`) and it inherits claude's hook
wiring — the `--settings` patch locally, and the same rewrite on a remote/WSL
host. It names the *family* to imitate, not a boolean; see
[AGENTS.md](AGENTS.md#hook_schema-custom-rebrands-only) for which family is
actually useful today and why.

The seeded file also ships two commented, copy-pasteable templates
below the built-ins — **Add your own agent** (every field annotated)
and **Pin a model** (a `claude-opus` variant baking `--model opus`
into `args`). Both stay commented, so a fresh install still resolves
to exactly the ten built-ins (nine coding agents plus `shell`).

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
| `socket` | no | `thurbox` | host `tmux -L` socket name. The socket file lives under the host's thurbox data dir, `~/.local/share/thurbox/tmux/tmux-<uid>/<socket>` — see [Where the tmux socket lives](#where-the-tmux-socket-lives) |
| `session` | no | `thurbox` | host tmux session name |
| `worktrees_dir` | no | host `$HOME/.local/share/thurbox/worktrees` | absolute worktrees dir on the host/distro |
| `multiplexer` | no | platform default | host preference for new sessions; an explicit per-create choice wins. Names are `tmux`, `psmux`, `rmux`, and `herdr` (no Herdr adapter yet). Use `psmux` for a native Windows SSH host |
| `platform` | no | `windows` if `multiplexer = "psmux"`, else `posix` | the host's OS: `posix` or `windows`. Decides its shell and paths, independent of `multiplexer` (ssh only — a WSL distro is always `posix`) |
| `share_sessions` | no | `true` | the host's own database is the record of its sessions: mirrored here, operated through the host's `thurbox-cli` (provisioned under `~/.local/share/thurbox/bin/` — `thurbox-dev/bin/` for a dev build — there when missing); `false` = drive the host from here as before |
| `path_prepend` | no | `[]` | directories put first on the `PATH` of the multiplexer commands, the delegated `thurbox-cli` and the agent's pane on a POSIX host (not git, the usage probe or an image drop) — absolute or `~/`-rooted (`~` = the host's `$HOME`); for what the host's login shell cannot report |
| `otel_endpoint` | no | — | the OTLP endpoint, as the host reaches it, that a session on this host exports to while [`[telemetry]`](#telemetry--opentelemetry-for-sessions) configures agents; unset = resource attributes only. On a shared host the agent's pane is launched by the host's own CLI from the host's `[telemetry]`; this endpoint still applies to `session exec` and `agent launch-args` run from here |

**The agent's `PATH` on a host.** ssh and `wsl.exe` hand a command a
non-login environment — `wsl.exe -e` skips the user's shell entirely, so
not even `~/.zshenv` is read — which has none of `~/.local/bin`,
`~/.cargo/bin`, nvm/fnm and the like. So thurbox reads the host's login
`PATH` once per host and process (`$SHELL -lc`, the passwd shell when
`$SHELL` is unset, and `/bin/sh -lc`; stdin closed, never `-i`, 5 s each
under `timeout`, 15 s overall). The multiplexer commands (so a Homebrew
tmux in `/opt/homebrew/bin`, which only a macOS login shell puts on the
`PATH`, is found), the delegated `thurbox-cli`, and so the agent's pane,
run there on `path_prepend`, then the login shells' `PATH`, then the
launcher's own (on WSL that includes the Windows `PATH` WSL appends unless
`appendWindowsPath = false`), each directory once. git, the usage probe
and an image drop keep the launcher's `PATH`. A host whose shells cannot be read keeps
the launcher's `PATH` with a warning in the log; session creation never
waits longer than the timeout on it, and never fails for it.

**SSH** auth comes entirely from your `~/.ssh/config`; thurbox never
handles credentials. **WSL** distros are reached with
`wsl.exe -d <distro>` and need no config entry at all — they are
**auto-discovered** (`wsl.exe -l -q`) and appear in the host picker
and `--host` automatically; add a `kind = "wsl"` entry only to override
a default (e.g. `worktrees_dir`). Discovery also works from *inside* a
distro (interop puts `wsl.exe` on `PATH` there), and excludes the distro
thurbox is itself running in: that one is this machine, sessions on it
are ordinary **local** sessions created with no `--host`. An entry whose
`distro` is that one is ignored with a startup warning. An entry merely
*named* after it — reaching a sibling — works as written and keeps its
sessions, but it records them under the very name an older thurbox
mislabelled *local* sessions with; the two are indistinguishable, so
thurbox warns and leaves every row under that name alone (any local
session still recorded there stays recorded there). Name a **new** entry
after the distro it reaches and there is nothing to warn about; renaming
an **existing** one moves no row, and leaves its recorded sessions
behind under a name no host registers.
For both kinds, the multiplexer, git, the agent,
and worktrees all run **on the host / inside the distro** at native
paths (a WSL distro's worktrees live in its own Linux filesystem, not on
`/mnt/c`); the distro needs its selected multiplexer (`tmux` >= 3.2 by default,
or opt-in RMUX >= 0.10.0) and `git`. Host changes require a restart
(the registry is read once and each host's `$HOME` is
cached for the process lifetime).

### Host platform

A host's **platform** (`posix` or `windows`) is its operating system, and it is
a choice of its own: not the multiplexer, not the way the host is reached, and
not the OS thurbox itself runs on. It decides the host's shell and path
semantics — PowerShell, `%USERPROFILE%` and no `/bin/sh` on Windows; `sh -c`,
`$HOME` and `/bin/sh` as the server's `default-command` on POSIX — so a Windows
thurbox driving a WSL distro pins `/bin/sh` there, and a Windows host on a
multiplexer other than psmux still gets PowerShell.

An entry that does not set `platform` keeps the meaning it always had:
`multiplexer = "psmux"` is Windows, anything else POSIX. A WSL distro is POSIX
whatever its entry says. What a multiplexer can do is not the platform's to
say either: whether thurbox polls a backend for dead panes is whether that
multiplexer reports a closed window (tmux does; psmux and RMUX need polling).

### Multiplexer choice and existing sessions

Host and multiplexer are separate choices. `hosts.toml` names the host's
preference; top-level `multiplexer` in `settings.toml` names the local
preference. `thurbox-cli session create --multiplexer <name>` overrides either
for one creation. The TUI asks for a host and then shows the registered
multiplexers for it. The order is explicit choice, configured host or local
preference, then platform default (`tmux` on POSIX, `psmux` on native Windows).

The TUI offers only the multiplexers **available** on the chosen host:
registered, native to its platform (tmux on POSIX and WSL, psmux on native
Windows, RMUX on either), and installed there. Locally that is a `PATH`
lookup. For an SSH or WSL host it is the probe that already reads the host's
login `PATH` (see `path_prepend` above), which also names each multiplexer it
finds there — on the same `PATH`, `path_prepend` first, that thurbox then runs
it on; it runs in the background only while the new-session flow is open,
and is asked again once its answer is a minute old, so the picker never waits
on a host and a host that was down, or has since had a multiplexer installed,
is read again. Until it answers, and on a native-Windows host, which
is not probed, a host offers its platform default and its configured
`multiplexer`.

The picker appears only when that leaves a real choice. With exactly one
available it is skipped and that one is used, so a machine with only RMUX
runs RMUX. With none, it says so with an install hint. A configured choice
that is available skips it too; one that is not requires an explicit
selection of another offered multiplexer. With two or more, the platform
default stays selected and `rmux` sits immediately after `tmux`. A local
`thurbox-cli session create` with no `--multiplexer` and no configured one
applies the same rule: the platform default when it is installed, otherwise
the one that is.
Names are `tmux`, `psmux`, `rmux`, and `herdr`. A configured choice without a
registered implementation is shown as unavailable and creation refuses it
before making a worktree or pane.
When creation is delegated to a host's own Thurbox CLI, that CLI advertises
whether it accepts a multiplexer choice. Older compatible CLIs can still use
their platform default; a non-default choice requires updating the host CLI.

The resolved choice is recorded in each new session's `backend_type`, its
**route**. One parser (`session::Route`) reads every spelling a row has ever
carried:

| `backend_type` | machine | multiplexer |
|---|---|---|
| `""`, `tmux`, `local-tmux` | this one | unqualified |
| `local:<mux>` | this one | `<mux>` |
| `local-<mux>` (read only, any mux but tmux) | this one | `<mux>` |
| `ssh:<host>`, `wsl:<distro>` | that host | unqualified |
| `ssh:<host>:<mux>`, `wsl:<distro>:<mux>` | that host | `<mux>` |

Every machine is qualified the same way, `<machine>:<mux>`. New rows always
name their multiplexer (`ssh:devbox:tmux`, `local:psmux`), so
a later change of preference cannot reinterpret them. Rows written before
routes did are **unqualified**, are never migrated, and keep the meaning they
were written with: a local one is the platform default, and a remote one is
psmux when the host's `multiplexer` is `psmux` and tmux otherwise. So a host
whose preference moves to `rmux` still has its old rows attached, deleted,
torn down and polled with tmux.

`local-tmux` was written for the platform default before routes named a
multiplexer, which is psmux on native Windows, so it still reads that way
there. An explicit local tmux is `local:tmux`, so the two can no longer be
confused. An older build does not know the `local:` spelling and cannot attach
a local session created by this one.

A host name may not contain `:`, which separates it from the multiplexer in a
route. `hosts.toml` ignores such an entry with a warning, and
`config validate` fails naming it.

Which multiplexers work is what is **registered**, never the OS: every
multiplexer an adapter implements (`tmux`, `psmux`, and `rmux`) is registered for
this machine and for every host, whatever either's platform or preference — a
binary that is not installed is reported by name when a session first needs
it (the TUI's picker narrows this further to what is installed; see above).
A route naming a multiplexer no adapter implements (`herdr` today)
is refused by name, and is neither created nor driven with another binary. A
force-delete or reap of a local row on a multiplexer this machine does not run
refuses rather than removing a
checkout or killing a local window of the same name; a delete without
`--force` still works and leaves it restorable. An adapter for another multiplexer
registers its own routes; it must read them on restart, restore, delete,
input, capture, and fork.

### Multiplexer requirements and RMUX setup

Use the latest stable release of the selected multiplexer. These are the
minimums enforced by the adapters, checked against upstream on 2026-10-22,
when psmux's floor rose to 3.3.8 (#1557).

| Multiplexer | Minimum | Code enforcing it | Upstream evidence |
| --- | --- | --- | --- |
| [tmux](https://github.com/tmux/tmux) | 3.2 | [`MIN_TMUX_VERSION` and `check_min_version`](../src/backend/tmux.rs) | [3.2 changes](https://github.com/tmux/tmux/blob/3.2/CHANGES): control-client flow control and format subscriptions |
| [psmux](https://github.com/psmux/psmux) | 3.3.8 | [`MIN_PSMUX_VERSION`, `check_psmux_version` (server) and `check_psmux_binary` (binary)](../src/backend/psmux.rs) | [3.3.8 release](https://github.com/psmux/psmux/releases/tag/v3.3.8): pane-slot targets (`thurbox:.%3`) resolve to that pane; [3.3.7](https://github.com/psmux/psmux/releases/tag/v3.3.7): dead warm-pane fix (psmux#450) |
| [RMUX](https://github.com/Helvesec/rmux) | 0.10.0 | [`Rmux::check_banner`](../src/backend/rmux.rs) | [0.10.0 release](https://github.com/Helvesec/rmux/releases/tag/v0.10.0): wire protocol 8, incompatible with 0.9.x daemons |

For tmux, the limiting features are `refresh-client -f pause-after=5`,
`refresh-client -A` (pane monitoring/resume), and `refresh-client -B`
(format subscriptions), added in 3.2. Control mode arrived in 1.8;
`send-keys -H` and pane options in 3.0; `set-clipboard external` in 2.6;
`resize-window` in 2.9. These are recorded in the same upstream changelog.
The mouse-state seed in
[`pane_state_seed`](../src/backend/tmux_compat/server.rs) uses six
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

Recorded integration evidence is tmux 3.2/3.2a and 3.5a/3.7c (ADR-12),
psmux 3.3.8 on Windows 11 (ADR-13), and RMUX 0.10.0 on Linux
([#1322](https://github.com/Thurbeen/thurbox/pull/1322)). This is historical
evidence, not a claim that every platform was retested for this doc change.
The latest upstream stable releases checked on 2026-10-03 were tmux 3.7c,
psmux 3.3.8, and RMUX 0.10.0; psmux 3.4.0 followed on 2026-10-13, and
[`psmux-pane-routing.ps1`](../scripts/dev/e2e/psmux-pane-routing.ps1) passes
against 3.3.8 and 3.4.0 on Windows 11.

RMUX is a Rust terminal multiplexer with a tmux-compatible command interface.
It is opt-in and never changes the platform defaults. Install it separately
on the machine running the sessions, following its
[installation guide](https://github.com/Helvesec/rmux/tree/v0.10.0#installation).
For example, `cargo install rmux --locked` installs the latest published
crate; check `rmux -V` afterwards. Release archives need their complete
package layout: on Unix run the archive's `./install.sh --prefix ~/.local`,
and keep its `bin/` and `libexec/`; do not copy just the executable.

Press **Ctrl+N**, choose the host if prompted, then choose **RMUX** immediately
after tmux in the multiplexer picker. RMUX appears only where an `rmux` binary
is found — on `PATH` locally, by the host probe on a host — and when it is the
only multiplexer installed, the picker is skipped and RMUX is used.
To make it the default for new local sessions, set this top-level value in
`settings.toml`:

```toml
multiplexer = "rmux"
```

For an SSH host, add `multiplexer = "rmux"` to its `[[hosts]]` entry. For WSL,
add a configured `kind = "wsl"` entry matching the discovered distro and set
that same preference; install RMUX inside the distro. An explicit
`thurbox-cli session create --multiplexer rmux` overrides the preference
(add the usual session name, repository and agent arguments).
Existing tmux and psmux rows retain their recorded routes.

| RMUX placement | Thurbox integration evidence |
| --- | --- |
| Local Linux | Live creation, TUI attachment, input and pane-deletion/relaunch in #1322 |
| Local macOS | Upstream ships macOS binaries; thurbox integration unverified |
| SSH host | Adapter and route construction covered; real-host execution unverified |
| WSL distro | Picker and route construction covered; distro execution unverified |
| Native Windows | Upstream ships Windows binaries; thurbox integration unverified, so keep the psmux default |

RMUX 0.10.0 rejects `pause-after`, `refresh-client -A`, and subscriptions
(`refresh-client -B`). Thurbox omits those commands, uses one reply block for
command lists, and polls pane liveness and remote hook options. Output buffering
therefore relies on RMUX rather than tmux's pause/resume mechanism. See
[ADR-31](ARCHITECTURE.md#adr-31-tmux-and-psmux-are-peer-adapters-over-one-tmux-protocol-server)
and [#1320](https://github.com/Thurbeen/thurbox/pull/1320).

If RMUX is absent from the local picker, check `rmux -V` in the environment
that launches thurbox. For a host, check it on that host or inside the WSL
distro. A version rejection needs an upgrade on the session machine.
RMUX 0.9.x daemons cannot talk to a 0.10.0 client: stop the old server before
upgrading, using `rmux -L <socket> kill-server` for each affected socket.
This stops its sessions; finish or save the agents' work first. A change of
preference does not convert existing sessions, so use the recorded route's
multiplexer when attaching manually or stopping a server.

A socket learned from a host's own CLI (`version --json`'s `tmux_socket`) is
kept per **host**, not per route: it is the address of the thurbox instance
there (ADR-12), which every multiplexer on that host runs under. Why routes
are read this way, and what it costs, is ADR-28 in `docs/ARCHITECTURE.md`.

Manual deletion of an agent pane or window means the agent should run again.
Once the backend **confirms absence**, Thurbox relaunches the same session ID,
agent, and worktree once. An exited pane still held by the server remains
visible for inspection. A backend that is unreachable or has not verified the
window cannot authorize a relaunch. A session intentionally parked with
`session stop` stays stopped. A deleted companion shell is forgotten without
relaunching the agent. RMUX's local headless and TUI-open pane-deletion paths
have live tests; Herdr remains unverified.

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
| `event` | yes | — | one of the events below |
| `command` | yes | — | run through `sh -c` (`cmd /C` on Windows) |
| `timeout_secs` | no | `30` | the hook is killed after this long — with everything it started (its process group; the process tree on Windows) |
| `agents` | no | `[]` | exact agent names; empty matches all |
| `to` | no | `[]` | destination states for `session.state_changed`: `working`, `blocked`, `done`, `idle`, `stopped`, `lost`, `exited`; empty matches all; any other word fails validation |

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

**The operation-level `pre_*` hooks can refuse.** Exit non-zero (or exceed the timeout) and
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

**Preparation before launch.** `session.pre_launch` runs locally on create
(including fork), after worktrees and any multi-repo workspace exist, before
opening the agent's pane. Its process cwd is the final launch directory and its
stdin includes all worktrees and additional directories. `agents = ["claude"]`
limits a preparation command to that agent definition. Non-zero exit, spawn
failure or timeout refuses the launch. Prepared worktrees and any changes the
user script made remain available for inspection; no pane or session row is
created. Use an existing prepared checkout as `--repo-path` when retrying.
It does not run on restart, start or
restore, and is not run locally for a remote session; host-side preparation is
tracked in #1473. It is a user hook, rather than an agent-definition field.

**State edges without an interface.** `session.state_changed` is a post-only
notification from the session event log, consumed by `automation tick` after
the status poll. It includes state edges and stopped/lost/exited notifications;
`to = ["done", "blocked"]` filters the destinations. The stdin object uses the
same `seq`, `event`, `reason`, `from_state`, `to_state`, session identity and
coverage fields as `watch --json`, plus `gap`. The environment adds
`THURBOX_FROM_STATE`, `THURBOX_TO_STATE` and `THURBOX_EVENT_SEQ` to the usual
identity variables. Terminal lifecycle reasons can have null state fields;
filter these by `to = ["stopped", "lost", "exited"]`.

Delivery is **at-least-once**, not exactly-once: scripts deduplicate side
effects on `seq`. A crash after a script runs and before the cursor commits
replays that batch. Ordinary hook failure is logged and returned in the tick's
`hooks.failures`; later hooks still run. One lease and compare-and-swap protect
the `hooks` row in `event_cursors` from overlapping ticks. Each tick reads at
most 512 events and spends at most 30 seconds on hooks, including their own
timeouts; each remaining hook gets a share of the remaining phase budget so
one slow hook cannot starve the rest of its event. No hook starts on less
than 100 ms of budget; an event cut short that way is not committed and is
delivered again, whole, on the next tick. Reaping and telemetry run
before this phase, so a slow hook delays only hooks. First use starts at the
head, skipping history. Pruned history produces `gap: true` once, on the first
payload a hook receives in that tick; when no hook's filter matches anything
in that batch, only the tick's own report carries it. The cursor outlives the
file: removing every state hook pauses delivery, and adding one back resumes
from where it stopped (at most 512 edges a tick, within retention) rather
than from the head. An edge whose session row was force-deleted before the
tick reached it is skipped, since its agent and context are gone with it.

Keeper latency is normally 60 seconds. The existing `features.automations`
off switch disables automatic keeper startup; manual `automation tick` still
consumes state hooks. After editing the file, `config validate` arms the keeper
when state hooks exist and the feature is enabled; startup and session creation
also arm it. A driver needing lower latency uses `watch` or `session wait`.
Extensions cannot declare these hook entries; keep them in the user's own file.

**What a hook receives.** Environment variables — **unset** (never
empty) when the fact is not known at that moment:

| Variable | Meaning |
|----------|---------|
| `THURBOX_HOOK_EVENT` | the event name, e.g. `session.post_create` |
| `THURBOX_SESSION` | the thurbox session id (at `pre_create`: the id it will have if creation succeeds) |
| `THURBOX_SESSION_ID` | the agent's own conversation id |
| `THURBOX_SESSION_NAME` | the session name |
| `THURBOX_SESSION_KEY` | the session's key (`session create --key`); unset when it has none |
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
>
> A top-level `layout` key is the one v2.32.0 wrote for its layout presets,
> which the next release rolled back (#1227). The interface removes that key at
> start (the key and its value, however spelt; the comments around it stay) and, unless it named
> `classic`, says once in the message band that the classic layout is back.

Scalar tuning knobs plus the `[features]` switches, seeded fully
commented-out (defaults apply when absent). Only knobs a user plausibly
wants are exposed; internals stay hardcoded. The seed closes with a
**Common recipes** block — copy-pasteable groupings (bigger scrollback,
a minimal/focused TUI, notification tuning, enabling the update badge),
all commented so defaults still apply out of the box.

| Key | Default | Purpose |
|-----|---------|---------|
| `multiplexer` | platform default | multiplexer preference for new local sessions; explicit per-create choice wins |
| `scrollback_lines` | `1000` | terminal scrollback kept per session — and how far back global search reaches |
| `hidden_terminal_secs` | `30` | how long a session can be off screen before its terminal grid is dropped (rebuilt from tmux when shown or searched); `0` keeps every grid |
| `two_panel_min_cols` | `80` | width below which only the terminal renders |
| `three_panel_min_cols` | `120` | width unlocking the optional third column |
| `audit_retention_days` | `90` | audit + session-event history kept (pruned on startup) |
| `deleted_retention_days` | `0` | days a deleted session stays restorable before the sweep purges its row; `0` keeps all |
| `git_poll_secs` | `5` | how often each session's git worktree is re-statted; `0` turns it off |

A complete `settings.toml` showing every knob at its default — copy
this, uncomment what you want to change, and restart:

```toml
config_version = 1
multiplexer = "tmux"     # use "psmux" on native Windows; "rmux" is opt-in

# Scalar tuning knobs (top level)
scrollback_lines      = 1000   # terminal scrollback kept per session
hidden_terminal_secs  = 30     # seconds off screen before a grid is dropped; 0 = keep all
two_panel_min_cols    = 80     # width below which only the terminal renders
three_panel_min_cols  = 120    # accepted and ignored (v1's third column)
audit_retention_days  = 90     # audit + session-event history kept (pruned on startup)
deleted_retention_days = 0     # days a deleted session stays restorable; 0 = keep all
git_poll_secs         = 5      # seconds between git stats of a session; 0 = off

[features]
shell_pane    = true
perf_hud      = true
soft_delete   = false
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
on_automation_failure = false  # notify when an automation run fails (once per failing streak)

[clipboard]
provider       = "auto"        # auto | native | osc52 | none
copy_on_select = true          # releasing a drag copies; false = Ctrl+C copies a selection

[remote]
transitive_sessions = true     # list sessions a host mirrors from hosts of its own
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
session runs, which is how thurbox behaved before (ADR-P27 in
[PERFORMANCE.md](PERFORMANCE.md)). psmux (Windows) cannot hand a pane back in
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
| `perf_hud` | `true` | perf HUD overlay (`F12`): live perf counters, frame/tick timing and the per-pane cost table (see `docs/PERFORMANCE.md`) |
| `automations` | `true` | heartbeat arming, by the TUI on startup and by `automation create`/`edit` (the CLI stays fully functional) |
| `mouse` | `true` | mouse capture: clicks, wheel, drag-select, hover, scrollbars |
| `notifications` | `true` | OS desktop notifications when a session needs attention |
| `soft_delete` | `false` | TUI and CLI delete immediately; on = soft delete with an undo window (CLI `--soft` / `--force` override) |
| `version_check` | `true` | GitHub update check: TUI header "update available" badge + `thurbox-cli version --check` |
| `auto_update` | `true` | Silent self-update **within the current major**: download + verify + replace the binaries on startup + `thurbox-cli update`; also auto-refreshes stale extensions |

`automations = false` stops thurbox arming the heartbeat keeper: the TUI
does not on startup, and `automation create`/`edit` do not either. The
interface runs no scheduler of its own, so nothing fires unless a keeper
window armed earlier, an OS timer from `packaging/`, a hand-run `automation
tick` or `automation run --wait` fires it. Every other `thurbox-cli
automation` command still works. Disabling
`shell_pane` hides existing shell panes but never kills their
processes. `mouse = false` skips terminal mouse capture entirely, so
the terminal keeps its native mouse behavior (its own text selection,
URL handling, etc.) and no click/wheel/hover handling runs in the TUI.
`notifications = false` keeps the background dispatcher thread from
ever starting (zero overhead) and silently no-ops every transition;
the session status display itself is unaffected.

`soft_delete = false` (the default in release, debug and sandbox builds) makes
TUI `Ctrl+D` and `thurbox-cli session delete` a destructive
**hard delete**: instead of marking the row deleted with a `Ctrl+Z`
undo window, it kills the session's tmux window, removes its worktrees
and symlink workspace, and disables any pending `Send` automations (a soft
delete disables those too, and a restore does not re-enable them). The TUI
asks first (`Enter`/`y` to delete, `Esc`/`n` to cancel), since the teardown
is irreversible; the CLI performs the requested delete without a prompt. The row is marked
**first**, before anything comes down: a crash partway through the
teardown would otherwise leave an active session whose worktrees are
already gone. It is marked force-deleted with it, so `Ctrl+U` lists it
and refuses — unless every worktree was one thurbox merely opened
rather than created, in which case nothing was lost and the restore
stands (which re-spawns it fresh). Tombstones remain for multi-instance sync
and remote cleanup retries; this is not a lossless undo. The CLI reads the
same setting on each call: `--soft` explicitly opts into the undo window,
while `--force` always tears down immediately. These flags are mutually exclusive.
For mixed-version session sharing, new peers always send the selected mode:
`--force` for hard deletion, `--soft` for peers advertising `soft_delete_choice`.
Older hosts do not understand `--soft`, so the new peer uses their existing
flagless soft-delete request instead. An old peer's flagless `--json` delete
remains soft on an upgraded host, preserving both the peer's undo and the host's
windows/worktrees. That request carries no caller marker and is identical to a
local JSON command: **local flagless `--json` deletes also remain soft**.
JSON scripts must pass `--force` for immediate cleanup or `--soft` for undo.
Plain local CLI and TUI deletes still follow `soft_delete`, defaulting to hard.
The capability bit selects flags understood by the receiver; major/schema
compatibility alone does not identify the caller or its intended deletion mode.
Users who never set the flag now get immediate teardown for plain CLI/TUI
deletes; an explicit
`[features] soft_delete = true` keeps the previous undoable behavior.
`extension deactivate` follows the same preference for its sessions.
Task and automation deletion have no undo window; their storage bookkeeping
is independent of this session preference.
With `soft_delete = true`, the TUI also asks before the reversible delete and
records the `Ctrl+Z` undo target only after the answer is yes.

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
dependency), verifies the SHA256 **in process** (`sha2`, so the check does not
depend on the local `PATH` — shelling out to `sha256sum`/`shasum` made it
impossible on native Windows, issue #1182), extracts it
(`tar`, or PowerShell's `Expand-Archive` for Windows' zip), and atomically
replaces the installed `thurbox`/`thurbox-cli`
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

**Windows takes the same path, with two differences.** It used to be refused
outright, which made a default-on `auto_update` silently mean nothing there
(issue #1172). The release artifact is the `.zip` `install.ps1` extracts, so the
unpacker is chosen by the archive's extension. And a rename cannot replace an
executable a process is running from — Windows keeps its image mapped — so the
swap there moves each installed binary aside to `.thurbox.exe.old` first (which
Windows allows for a running image) and then moves the new one in. The running
process keeps its old image until the next launch, as on Unix. A thurbox still
running from the last update's backup — a hook or heartbeat `thurbox-cli.exe`
that has not exited — keeps that file, so the next update moves the binary to
`.thurbox.exe.1.old` (and so on) instead, and deletes every backup nothing runs
from any more. The two binaries are committed together: if either cannot be
swapped, the one already swapped is put back from its backup, so a failed update
normally leaves the TUI and CLI on the version they had (issue #1432). The
put-back is best effort: if it is refused too — say a process has just started
from the new binary — that binary stays on the new version, and the update
fails with an error saying it may be partial. Re-running `install.ps1` over a
running thurbox works the same way: it renames each installed binary to `.<name>.old` rather
than deleting it, and names the process to close when even that is refused.

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

### `[remote]` — sessions on hosts of hosts

A shareable host (see [hosts.toml](#hoststoml)) lists its own sessions and,
when it mirrors hosts of its own, theirs. `transitive_sessions` (default
`true`) keeps those: each session is listed once, on the most direct path this
instance has to it — a host reached directly wins over the same session seen
through another — and every action on it reaches the host that owns it. A row
seen through another host has no terminal attached here. `false` lists only
each host's own sessions and drops the rows already taken on, without deleting
anything anywhere. Read by every mirror pass, so a change applies on the next
one (within 10 s in the TUI). [FEATURES.md](FEATURES.md) → *Shared sessions*
has the rule.

### `[telemetry]` — OpenTelemetry for sessions

Turns on each agent's **own** OpenTelemetry exporter for the sessions thurbox
spawns, pointed at one OTLP collector (ADR-36), and thurbox's own session
metrics and session event logs, exported by `automation tick` (ADR-37), and,
opt-in, the interface's own diagnostic logs (ADR-38).

```toml
[telemetry]
enabled = false
endpoint = "http://localhost:4318"
protocol = "http/protobuf"
headers = {}                 # e.g. { authorization = "Bearer …" }
configure_agents = true
export_thurbox_metrics = true
export_session_events = true
export_diagnostic_logs = false
per_session = false
```

With `enabled` and `configure_agents` both on, every local session gets:

- the OTel spec's `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_PROTOCOL`
  and, when `headers` is set, `OTEL_EXPORTER_OTLP_HEADERS` — enough on their own
  for `copilot`, `omp` and `opencode`;
- `OTEL_RESOURCE_ATTRIBUTES` with `thurbox.session.id`, `thurbox.session.name`
  and `thurbox.agent` appended to any value already in the environment, so a
  backend can group an agent's telemetry by session;
- the agent's own `telemetry_env` / `telemetry_args` from
  [agents.toml](#agentstoml): `CLAUDE_CODE_ENABLE_TELEMETRY` and the metrics
  and logs exporters for `claude`, per-session `-c otel.*` overrides for `codex`
  (it ignores the spec's endpoint variable and is always sent `binary`, which
  is protobuf), and `VIBE_ENABLE_OTEL` / `VIBE_OTEL_ENDPOINT` for `vibe`.

A variable the session's own `--env` sets wins, except
`OTEL_RESOURCE_ATTRIBUTES`: the session's value is merged into the inherited
one, its keys winning a conflict, and thurbox's three keys still win over both.
A remote session exports to its host's `otel_endpoint` from
[hosts.toml](#hoststoml) instead of `endpoint` (`localhost` there is the host),
with this table's `protocol` and `headers`; a host that sets none gets only the
resource attributes. On a shared host the agent's pane is created and restarted
by the host's own CLI, which applies the host's own `[telemetry]`; `session
exec` and `agent launch-args` run from here still use this machine's
`[telemetry]` with the host's `otel_endpoint`. `antigravity`, `pi` and
`aider` have no native OTel.

The table is read at every spawn and restart, so a change applies to the next
one; a running agent keeps the environment it launched with. The variables are
in the pane's environment, so everything run in the session inherits them,
including any OTel-instrumented tool the agent starts, and the agent process can
read `headers`.

#### thurbox's own session metrics and session event logs

What only thurbox knows, the same for every agent: the only telemetry at all
for `antigravity`, `pi` and `aider`. `automation tick` sends it, so it flows
whenever the heartbeat runs, with or without the interface open. The tick
re-reads the table on every run, so a change applies within a minute.

| Metric | Kind | Unit | Attributes |
|---|---|---|---|
| `thurbox.sessions` | gauge | `{session}` | `state`, `agent`, `backend` |
| `thurbox.session.transitions` | counter | `{transition}` | `from`, `to`, `agent` |
| `thurbox.session.lifecycle` | counter | `{event}` | `reason`, `agent` |
| `thurbox.session.exits` | counter | `{exit}` | `exit_code`, `agent` |
| `thurbox.turn.duration` | histogram | `s` | `outcome`, `agent` |
| `thurbox.blocked.duration` | histogram | `s` | `agent` |
| `thurbox.session.lifetime` | histogram | `s` | `reason`, `agent` |

- `thurbox.sessions` counts live sessions by the state word `session list`
  shows, or `lost`. Each state word is reported for every agent and backend
  that has a session, at zero when no session is in it. Once an agent's last
  session goes, its series are no longer reported and expire in the backend.
- A turn runs from entering `working` to leaving it, and `outcome` is the state
  it left to. `thurbox.blocked.duration` is time spent waiting on a human. A
  stop, a loss or a delete in between ends either one unmeasured.
- `reason` is the [`watch`](ORCHESTRATION.md#watch-for-everything-that-is-not-a-report)
  vocabulary: `spawned`, `restored`,
  `stopped`, `lost`, `soft_deleted`, …
- `per_session = true` adds `thurbox.session.id` to every metric. Each session
  then becomes a series of its own in the backend.

Each `session_events` row becomes one log record, stamped with when the event
happened rather than when it was exported: `thurbox.session.created`,
`thurbox.session.state_changed` (`from`, `to`), `thurbox.session.changed` and
`thurbox.session.gone` (`reason`, and `exit_code` for an exit). Every record
carries `thurbox.session.id`, `thurbox.session.name` and `agent`, so a backend
interleaves them with the agent's own events into one timeline per session. The
resource is `service.name=thurbox`, `service.version`, `host.name` and
`thurbox.instance` (the multiplexer socket, which tells a sandbox apart from the
real instance).

How the export behaves:

- **Plain HTTP and `http/protobuf` only.** Reach an `https` backend through a
  local Collector. With another `protocol`, thurbox exports nothing of its own
  and the tick's report says why.
- **Delta temporality.** Each tick is a fresh process. OTLP backends take
  deltas as they come; Prometheus needs the Collector's `deltatocumulative`
  processor (below).
- **At least once.** The first export starts at the head of the event log, so
  history from before telemetry was on is not replayed. After that a cursor in
  the database records how far the export has got, and only one tick exports at
  a time. A failed export moves nothing, so the same rows go out on the next
  tick. If metrics are accepted and logs refused, that batch's metrics are
  counted twice. A tick sends at most 1,000 events; a backlog drains over the
  following ticks.
- **Shared hosts** (ADR-24): their sessions are left out. The host's own tick
  exports them under its own `host.name`.
- **Never in the way.** A failure is a `WARN` line on the tick's stderr and a
  `telemetry.error` in `automation tick --json`. Each request has a 5-second
  timeout, and the first one that fails ends the export for that tick.

A Collector that takes thurbox's metrics to Prometheus and prints its session
event logs (`otelcol-contrib`, which ships `deltatocumulative`):

```yaml
receivers:
  otlp:
    protocols:
      http:
        endpoint: localhost:4318
processors:
  deltatocumulative: {}
exporters:
  prometheus:
    endpoint: localhost:9464
  debug: {}
service:
  pipelines:
    metrics:
      receivers: [otlp]
      processors: [deltatocumulative]
      exporters: [prometheus]
    logs:
      receivers: [otlp]
      exporters: [debug]
```

#### thurbox's own diagnostic logs

`export_diagnostic_logs = true` sends the interface's own log lines at `WARN`
and above to the collector's `/v1/logs`, under the same resource as the tick's
export. They serve debugging thurbox itself, not observing sessions, so the
switch is off by default.

- **Interface only.** `thurbox-cli` never sends its log lines: a command an
  agent hook runs must not wait on a collector.
- **The log file is unchanged.** `thurbox.log.<date>` keeps every level it had
  and is the only log while telemetry is off or the collector is unreachable.
- **Restart-only.** The interface builds its logging once, at start; changing
  the switch, or the endpoint, protocol or headers while it is on, applies on
  the next start.
- **What they contain**: the same text as the log file, which can name paths,
  sessions and hosts.
- Plain HTTP and `http/protobuf` only, as above. Records still queued when the
  interface is killed by a signal are lost; the log file has them.

### `[notifications]` — OS notification settings

Surfaces an OS notification when a session **transitions into a state
that needs your attention**. The trigger is the hooks-driven
[session status](#session-status) (reported by the agent's hooks, *not*
the terminal bell / output): the notification fires when a session crosses
into `Blocked` (the agent needs input or approval) and, with
`also_on_waiting = true`, also when it finishes a turn (`Working → Done`).
The edge is detected once per tick in `kernel::notify`, reading the same
`SessionState` the session-list status icon draws, so the banner can
never drift from the list (see `docs/FEATURES.md` → *Why the transition is
observed in one place*) — then deduped per session (`min_interval_secs`)
and skipped for the session you're currently viewing (`suppress_for_active`).
The notification body is the agent's last OSC 9 / OSC 777 message when
present (truncated to 200 chars), otherwise `Waiting for input` — the OSC
message is kept only for the body text, no longer for the trigger.
These session-state notifications **only fire while the TUI is open** — the
dispatcher thread runs only inside the TUI. Two things do notify headlessly,
both explicitly: `thurbox-cli notify send` and the opt-in failing-run alert
(see [Scripted notifications](#scripted-notifications)).

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
| `on_automation_failure` | `false` | notify when an automation run fails or times out, from the headless `automation tick` too; once per failing streak |

Notifications fire on the hooks-driven status transitions (see
[Session status](#session-status)): always on `→ Blocked` (the agent needs
you), and with `also_on_waiting = true` also on `Working → Done`.

### Scripted notifications

`notify send` delivers one notification through the same backend, for an Exec
automation, a lifecycle hook, a CI job or an orchestration script:

```bash
thurbox-cli notify send --title "nightly sync failed" --body "see the run log" \
  [--session <id|key|name>] [--key nightly-sync --min-interval 3600] [--json]
```

- It honours `[features] notifications = false` and `backend = "off"`: it does
  nothing and exits 0, reporting `"suppressed": "disabled"` — before it looks
  at `--session`, so a hook naming a session that is already gone still exits 0.
- `--key` with `--min-interval` (seconds, at most a year; one needs the other) drops a repeat of
  the same key inside the interval, between separate processes, and reports
  `"suppressed": "deduped"`. The key is held in the database's lease table from
  the moment of a delivery; a delivery that fails does not hold it, so the retry
  goes out.
- A delivery that fails exits non-zero with `"error"` set. One the backend has
  not finished after 10 seconds counts as failed (`delivery timed out after
  10s`), so a hung notifier never holds up the tick that sends an alert. `--json` always
  reports `delivered`, `suppressed`, `error` and `session_id`.
- It never changes a session's state. `--session` only names the session the
  notification is about; an unknown reference is an error.
- **Click-to-focus.** On macOS with `terminal-notifier`, clicking runs
  `thurbox-cli session focus <id>` for the `--session` given, as the TUI's
  banners do. On Linux the click is answered by the process that sent the
  notification, and `notify send` exits as soon as it is shown, so the banner
  is informational there; elsewhere it always is.
- **The session bus.** On Linux the dbus backend reads
  `DBUS_SESSION_BUS_ADDRESS`, and without it the per-user bus at
  `$XDG_RUNTIME_DIR/bus`. An Exec automation fired by the heartbeat inherits the
  environment of the tmux server that runs it, which normally carries both; one
  run from a bare cron or a systemd system unit has neither, and must export
  `XDG_RUNTIME_DIR` (or the bus address) for a notification to reach the
  desktop. `thurbox-cli notify` shows the backend the current environment
  resolves to.

**The failing-run alert.** With `on_automation_failure = true`, an automation
run that ends `error` or `timed_out` raises a notification titled
`Automation <name> failed` whose body is the first line of the run's detail
(`exit 3`, `timed out after 60s`). It is sent the way `notify send` sends one,
from the headless `automation tick` as well, and deduplicated per automation:
the failures after the first are dropped until a tick sees the automation
succeed again, or for at most a day while it keeps failing. Only a run that is
still the automation's latest counts, so a slow tick reporting an older run
cannot reopen a streak a newer success closed. It covers the runs
a tick fires; `automation run --wait` reports its own outcome to its caller and
raises no alert. `automation tick --json` lists each
alert under `alerts`. Off by default.

## Session status

Each session's state (Blocked / Working / Background / Done / Idle) is driven
by **agent hooks** that call `thurbox-cli session signal --state
<working|blocked|done|idle|background>`. Sub-work a turn may outlive is
reported by its edges, `--work-start <id>` / `--work-end <id>`; a `done` while
any is in flight is stored as `background` (see
[AGENTS.md](AGENTS.md#background-sub-work)). The state is persisted on the `sessions` row
(`hook_state`, `hook_state_at`, `seen_at` — schema v34) and survives the TUI
being closed; a hook fired headlessly is picked up via `PRAGMA data_version`.
Identity comes from the injected `THURBOX_SESSION` env var, so a hook passes
no id. A finished turn shows `Done` (blue) — for the session you're watching too
— and becomes `Idle` once you switch focus off it.

The hooks are wired up automatically by the built-in **hooks** extension
(auto-activated on first run). Opt out with `thurbox-cli extension deactivate
hooks`. The status colours are tunable theme keys (`status_working` /
`status_blocked` / `status_done` / `status_idle` / `status_unreachable` /
`status_running` / `status_unknown` / `status_background` — see
`themes.toml`). `status_running` is
an agent seen holding a pane that has reported nothing, and `status_unknown`
the two silences behind it (`uncovered`, `unreported`); both are spelled apart
from `status_idle`, which is the agent's own report that it is at rest.
`status_error` is a separate role, for a failed *command* in the list, not a
session state.

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
TUI start, heartbeat tick, or `thurbox-cli` command that launches an agent
(`session create`/`start`/`restart`/`fork`, `task run`); to customize, deactivate
the extension and wire the hook yourself, or edit the payload under
`extensions/hooks/` and reinstall. Full per-agent detail:
`extensions/hooks/README.md`.

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
- Action names and defaults: see the
  [Keybindings page](https://thurbox.thurbeen.eu/docs/keybindings.html), the
  live `F1` registry, or `src/session/keybindings.rs`.

## extensions/

Each opt-in extension is described by a single `extension.toml` manifest.
`thurbox-cli extension install` writes the home-resolved copy to
`~/.config/thurbox/extensions/<name>.toml` (thurbox never seeds this dir). The
install **home** (where payload files land and the session runs) defaults to
`~/.config/thurbox/extensions/<name>/` — a sibling dir of that manifest —
unless the manifest pins a `home` or you pass `--home`. The manifest has two
halves — an **install** spec and a **runtime** spec.

A relative install home is made absolute against the install command's
directory. Explicit automation `cwd` paths are stored absolute: relative
paths resolve against that install home, `~` against the user's home, and
`{home}` names the install home. Activation and heartbeat ticks therefore use
the same directory. A hand-written discovery manifest must supply an absolute
or resolvable `~` cwd; install it first to resolve relative paths.
Reactivation repairs previously stored relative cwd values from the resolved
manifest while preserving existing absolute overrides and run history.

The example below is modelled on
[fleet](https://github.com/Thurbeen/fleet), the control-plane template that is
the worked example of an installable extension (`docs/ORCHESTRATION.md`). Its
real manifest uses `[[agents]]`, one `[[files]]`, three `[[symlinks]]` and one
`[[sessions]]`; the other entries here are illustrative, so every field the
format supports is shown once:

```toml
name = "fleet"
description = "Control-plane session: the repo map and thurbox orchestration"
config_version = 1              # manifest *format* version (for migrations)
version = "1.0.0"              # the extension's own version (bumped by its author)
min_thurbox_version = "2.19.0"  # minimum thurbox; older binaries get a warning
# home = "~/fleet"              # OPTIONAL; default is <config>/extensions/<name>.
                                # {home} is substituted everywhere it appears

# install spec ---------------------------------------------------------------
[[agents]]                      # registered in agents.toml (existing kept)
name = "fleet"
command = "claude"
args = ["--model", "claude-haiku-4-5"]

[[files]]                       # fetched from the source, written under home
path = "FLEET.md"
[[files]]
path = "scripts/sync-registry.sh"
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
target = "FLEET.md"

# Reaching OUTSIDE the extension home (used by the built-in hooks extension):
[[external_files]]              # write a file into an agent's OWN config dir
path = "~/.config/opencode/plugin/x.js"
source = "x.js"
requires_dir = "~/.config/opencode"  # skip when that agent isn't installed

[[agent_patches]]               # append args to an EXISTING agent (reversible)
name = "claude"
append_args = ["--settings", "{home}/claude.json"]

[[config_merges]]               # reversibly deep-merge into an agent's own
path = "~/.gemini/settings.json"  #   SHARED config file (never clobbered)
source = "antigravity-hooks.json"  # objects recurse, arrays union; uninstall prunes
requires_dir = "~/.gemini"      #   exactly our entries (by marker). no-op write
                                #   when unchanged; malformed target soft-skipped
# format = "toml"                # OPTIONAL; JSON by default. Set for a TOML
                                #   shared config (agent::toml_merge via toml_edit).
                                #   TOML owns its entries by a comment on each, so
                                #   the payload must stamp every one of them

# runtime spec (ensured on activate, self-healed if deleted) -----------------
[[sessions]]
name = "fleet"
agent = "fleet"
repo_path = "{home}"            # absolute, `~`-relative, or `{home}`; resolved
                                #   to an absolute path at install. `{home}` is
                                #   the extension home, so an extension whose
                                #   session must open the USER's checkout ships
                                #   a placeholder its installer renders instead

# [[automations]] is an OPTIONAL runtime resource (fleet ships none on purpose —
# its only scheduled candidate pushes to `main`). An extension that wants a
# scheduled tick declares:
[[automations]]
name = "example-tick"
trigger = "cron:*/10 * * * *"   # same grammar as `automation create --trigger`
session_ref = "fleet"          # must match a [[sessions]] name above
prompt = "tick"

# Or an exec automation: a command, no session. `timeout_secs` (default 600),
# `cwd` (default user's home; relative to install home; `{home}` substituted)
# and `env` are how it runs, as
# `automation create --timeout/--cwd/--env` set them; `cwd` and `env` are
# refused on a send automation.
[[automations]]
name = "example-sync"
trigger = "hourly"
command = "{home}/sync.sh"
timeout_secs = 120
cwd = "{home}"
env = { SYNC_MODE = "incremental" }
```

Manage extensions with the CLI:

```bash
thurbox-cli extension install ./my-ext     # from a local dir: fetch + lay files
                                           #   + agents + activate
thurbox-cli extension install <url> --home ~/x    # from a URL, custom home
thurbox-cli extension install git+https://github.com/you/my-ext  # clone a repo
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
(`main` for dev builds), so a fetched extension matches your binary.
**No extension ships under a bare name today**: `extensions/` holds only the
two built-ins (`hooks`, `ui-skill`), which are embedded in the binary and
activate themselves, so `extension available` lists nothing and every install
is a path, an `http(s)://` URL, or a repository (`git+https://…`, or a URL
ending in `.git`, or the scp-like `git@host:path` — recognised explicitly, so a
bare `https://` URL keeps meaning "a base to fetch files from"). Payload paths are
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

A **bare-name** install (`extension install <name>`) fetches from the
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
back, install from a URL or repository that names the version you want —
for a bare-name extension that is a specific thurbox tag, `extension install
https://raw.githubusercontent.com/Thurbeen/thurbox/v0.112.0/extensions/<name>`
— or downgrade the binary and run `extension update`, which re-resolves
the bare name to that older tag.

## SQLite-backed settings

Live in the `metadata` table and apply immediately (no restart), except
`active_theme`: the process that picks it applies it at once, and every other
thurbox process reads it when it next starts.

| Key | Set via | Purpose |
|-----|---------|---------|
| `active_theme` | `Ctrl+Y` / `F4` picker | TUI palette (fifteen built-ins) |
| `editor_command` | `thurbox-cli editor set "<cmd>"` | what `Ctrl+O` runs |
| `editor_mode` | `thurbox-cli editor mode <auto\|terminal\|gui>` | how `Ctrl+O` runs the editor: `auto` (default) detects terminal vs GUI and gives terminal editors a real TTY (tmux popup / TUI suspend); `terminal` forces the TTY path; `gui` forces detached |
| `active_extensions` | `thurbox-cli extension activate/deactivate` | JSON array of active extensions to self-heal |
| `builtin_hooks_optout` | `thurbox-cli extension deactivate hooks` | `1` when the user opted out of the auto-activated hooks extension |
| `perf_snapshot` | the TUI, while perf timing is active (`THURBOX_PERF_LOG` or an open perf HUD) | JSON perf snapshot read by `thurbox-cli perf` (see `docs/PERFORMANCE.md`) |
| `host_probe_backoff:<backend>` | the teardown sweep, when a host's windows could not be listed | `<attempted_at_millis>:<failures>` — how long that host is left alone before the sweep asks again (ADR-26). Not user-set; the row is deleted the first time the host answers |
| `session_reap_backoff:<session id>` | the teardown sweep, when a soft-deleted row's reap did not reach its host | `<attempted_at_millis>:<failures>` — the same curve, per row rather than per host, for a host that answers `list-windows` while its own `thurbox-cli` does not run (ADR-26). Not user-set; the row is deleted the moment the reap comes off, and by a restore, which ends the delete it belonged to |

These are in the DB rather than a file because they are written
concurrently by multiple thurbox processes (TUI, CLI, MCP) and picked
up live via `PRAGMA data_version` polling — `active_theme` aside, which
another process reads when it next starts. The two backoff keys rebuild no
snapshot when they change (schema v55): only the teardown sweep reads them.

`event_cursors` (schema v55) holds what processes take turns at, one row per
`consumer`, none of it user-set:

| `consumer` | Written by | `cursor` / `lease_until` |
|---|---|---|
| `telemetry` | the export in `automation tick` (ADR-37) | the last `session_events.seq` exported / the epoch ms the exporting tick's lease ends |
| `lock:name:<server>:<name>` | `session_ops::lock`, while a session name is decided: extension self-heal, `session restore`, `session create --on-existing fail\|adopt\|replace` | `0` / the epoch ms the lock lapses. Deleted when the operation ends, taken over once it lapses |
| `lock:name:key@<host>:<key>` | the same lock, on a session key, for every writer that gives one out | as above |
| `lock:session:<id>` | a restart, while it replaces the session's window | as above |
| `lock:run:<automation id>` | a fire of one automation — the tick's or `automation run --wait`'s — held for the automation's timeout plus 5 s at least, so a second fire of it is skipped (tick) or refused with exit 5 (`run --wait`) | as above |

Until v55 the name and restart locks were `session_name_claim:*` and
`session_restart_claim:*` keys in `metadata`, and v55 moves any still there. A
thurbox from before v55 still takes those keys, so it and a newer one running
side by side do not see each other's locks.

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
| `TMUX_TMPDIR` | tmux's own: the directory its `tmux-<uid>/<socket>` goes under. When set, thurbox leaves it alone (test and sandbox tooling scope their servers with it); unset, thurbox sets it to `<data dir>/tmux` for every tmux command it runs — see [Where the tmux socket lives](#where-the-tmux-socket-lives). Empty = unset. |
| `THURBOX_UI_INSTANCE` | selects a running TUI for `thurbox-cli ui state` and `ui action` when `--instance` is omitted. |
| `WSL_DISTRO_NAME` | set by WSL itself, not by you: it is how thurbox knows which distro it is running inside. That distro is *this machine*, so it is never offered as a host and a `hosts.toml` entry pointing at it is ignored — see [hosts.toml](#hoststoml) |

Set **by** thurbox into every spawned agent process (not user-set;
`session_ops::inject_thurbox_env` / `App::build_spawn_inputs`). An
agent — or a `thurbox-cli` call running inside the session — reads
these to prove its own identity without scraping panes or names:

| Variable | Set into agent process |
|----------|------------------------|
| `THURBOX_SESSION` | the stable thurbox `SessionId` (the registry key); read back by `thurbox-cli message`/`inbox` for self-identity, and by `session signal` — see below |
| `THURBOX_SESSION_ID` | the agent's own conversation id (`agent_session_id`); consumed by the metrics statusline. Distinct from `THURBOX_SESSION` |
| `THURBOX_TASK` | the originating task id; task-spawned sessions only (headless `task run`) |
| `THURBOX_SESSION_KEY` | the session's key (`session create --key`, issue #1408), so a process in the pane knows which role it plays without a lookup; unset when it has none, and never taken from `--env` |
| `THURBOX_METRICS_DIR` | metrics output dir |
| `THURBOX_CONFIG_DIR` / `THURBOX_DATA_DIR` | the resolved config/data dirs, so the agent's `thurbox-cli` (its status hook) targets the same DB the TUI reads — independent of XDG, which `thurbox-cli` is on PATH, or a stale tmux-server env. Also honored if you set them yourself to relocate thurbox's state. |
| `THURBOX_SOCKET` | the multiplexer socket that instance's sessions live on, so an in-session `thurbox-cli` reaches the same server instead of re-deriving one from the session's own environment |
| `THURBOX_SOCKET_FOR` | the data dir that injected `THURBOX_SOCKET` belongs to. Read only to tell an inherited socket from one you exported: a child that points `THURBOX_DATA_DIR` somewhere else no longer matches, and derives its own socket instead of creating windows on the spawning instance's server |

A pane's **`PATH`** is the `PATH` of the thurbox that spawned it, which tmux
copies in by itself. thurbox adds one thing to it: the directory holding its own
`thurbox-cli`, in front, prepended and never replacing what was there. Without
it a session spawned by a `thurbox-cli` running over ssh — which on a
shared-sessions host is every session, since the local TUI delegates
`session create` to the host's CLI — inherits sshd's `PATH` for a
non-interactive command (`/usr/local/bin:/usr/bin:/bin:/usr/games`), which has
no `~/.local/bin` on it. The bare `thurbox-cli` the status hooks call then
resolves to nothing and the `|| true` after it hides that. It arrives as an
`env PATH=…` prefix on the window command rather than as one more injected
variable, because `PATH` is the one tmux will not take that way: both
`new-window -e PATH=…` and `set-environment -g PATH …` are ignored (verified
against tmux 3.5a).

`THURBOX_SESSION` is a **stable integration contract**, not an internal
detail of the hooks extension. It is set on the pane, so every process
started inside it inherits it — including an agent a driver of your own
launches there, and that agent's own hooks. Anything in the pane can
therefore report state with

```bash
thurbox-cli session signal --state <working|blocked|done|idle|background>
```

and no arguments at all; from outside the pane, pass `--session <uuid>`.
Sub-work the turn may outlive is reported the same way, by its edges
(`--work-start <id>`, `--work-end <id>`), and a `--state` report that comes
from that work names it with `--work <id>` or `THURBOX_SIGNAL_WORK=<id>` in
the command's environment, and the `working` that starts a turn says so with
`--turn-start` or `THURBOX_SIGNAL_TURN=start` (it forgets work earlier turns
left counted). The variables are what the shipped claude hooks use, because a
remote pane's rewrite keeps a hook to one bare state word.
That is the supported way to give thurbox agent state for a session it
did not wire — for instance one created with a bare interactive shell as
its "agent" because a harness owns the real launch.

Two caveats. A **remote** session's `thurbox-cli` writes the *host's*
database: on a shared host (`share_sessions = true`, the default) that is
correct and the observer mirrors it, but on a non-shared host the signal
never reaches the local database — thurbox rewrites its own hooks to a
tmux pane option there for exactly this reason. And every shipped hook
command ends in `|| true`, so a failed signal is silent; `thurbox-cli
session doctor` is how to check the wiring.

Set **by** thurbox into every [lifecycle hook](#hookstoml) it runs
(`session_ops::lifecycle_hooks`), beside `THURBOX_SESSION`,
`THURBOX_SESSION_ID`, `THURBOX_TASK` and the location overrides above:

| Variable | Set into hook process |
|----------|-----------------------|
| `THURBOX_HOOK_EVENT` | the event, e.g. `session.pre_delete` |
| `THURBOX_SESSION_NAME`, `THURBOX_AGENT` | the session's name and agent |
| `THURBOX_SESSION_KEY` | the session's key, when it has one |
| `THURBOX_REPO`, `THURBOX_CWD` | the primary repository, and the directory the agent runs in |
| `THURBOX_BRANCH`, `THURBOX_BASE_BRANCH` | the worktree branch and its base |
| `THURBOX_HOST` | the remote host name (local: unset) |
| `THURBOX_PARENT_SESSION` | the parent session id |

Set **by** thurbox into every `exec` automation's command
(`session_ops::automations::fire`), on top of the firer's environment and the
automation's own `--env`. These win over both, so a `thurbox-cli` inside
reaches the instance that fired it, whoever fired it (the heartbeat, an OS
timer, a hand-run tick, `run --wait`):

| Variable | Set into the command |
|----------|----------------------|
| `THURBOX_AUTOMATION_ID`, `THURBOX_AUTOMATION_NAME` | the automation's id and name |
| `THURBOX_CONFIG_DIR` / `THURBOX_DATA_DIR`, `THURBOX_SOCKET`, `THURBOX_SOCKET_FOR` | the location overrides above |

It runs in its `--cwd`, or your home directory — never in whatever directory
the firer was started in. `THURBOX_SESSION`, `THURBOX_SESSION_ID`,
`THURBOX_SESSION_KEY` and `THURBOX_TASK` are removed: an exec fired from a pane
does not act as that pane's session.

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
  name* (the dev sandbox, whose teardown kills it) keeps naming it — **unless it
  was inherited from another instance.** thurbox injects the socket into every
  pane it spawns, paired with `THURBOX_SOCKET_FOR`, the data dir it belongs to.
  A `thurbox-cli` inside that pane which relocates *itself* — a sandbox, a test
  harness, an agent exporting its own `THURBOX_DATA_DIR` — no longer matches
  that pairing, so the inherited name is dropped and the derivation runs. An
  override you export yourself carries no pairing and still wins outright.
  Without this, isolating the database silently left the tmux server shared,
  which looks contained and is not.

Ask a build which server it is on rather than assuming: `thurbox-cli version
--json` reports the socket in force as `tmux_socket`, and `thurbox-cli config
show` prints it under `Sessions`.

`session create --json` reports `tmux_socket` too, as the socket of the server
the new session's pane is on: the host's for a `--host` session (its `socket`
in `hosts.toml`, else what its own CLI reported), this instance's for a local
one, and `null` for a row on a host `hosts.toml` no longer describes. Hand it
to `tmux -L` together with `backend_id`, which is the pane id.

The key keeps the name `tmux_socket` on every platform, including native
Windows where psmux serves it. It is public JSON that scripts already read, and
it is the `-L` name every tmux-protocol multiplexer takes. A rename would break
those scripts for no new information, so there is no alias. A multiplexer that
is not addressed by `-L` would have to add its own key rather than reuse this
one.

**An instance relocated before this existed is a new instance.** Its old
sessions are still on the default server, and its database still lists them —
on the new socket they read as sessions whose window is gone. There is no
migration: point that instance back at the old server with
`THURBOX_SOCKET=thurbox` if you want them, or let it create fresh ones.

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
this way, and what it costs, is ADR-27 in `docs/ARCHITECTURE.md`. On a Windows
host (psmux) the last instance to paint a pane still sizes it.

### Where the tmux socket lives

thurbox runs tmux as `tmux -L <socket>`, and tmux puts that socket at
`$TMUX_TMPDIR/tmux-<uid>/<socket>` — `/tmp/tmux-<uid>/thurbox` when nothing
sets `TMUX_TMPDIR`. macOS sweeps old entries out of `/private/tmp`, and a unix
socket's access time is not bumped by `connect()`, so a server in daily use
loses its socket file after a few days (measured: 3 days 18 hours). The server
keeps running with every agent in it, but nothing can reach it any more.

So thurbox keeps the socket out of `/tmp`. Every tmux command it runs carries
`TMUX_TMPDIR=<data dir>/tmux` (created `0700`), beside `thurbox.db`:

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

**Upgrading.** A server started by an earlier build is in `/tmp`. The first
thurbox with the new directory finds it there and keeps using it for as long as
it runs; the next server, after it exits (or a reboot), starts in the new
directory. Nothing has to be restarted, and no second server is started beside
the old one.

### An orphaned tmux server

A thurbox older than the above, whose socket was swept, started a **second**
server under the same name and resumed every session into it. The first one
kept running and nobody could see it: not thurbox, not `tmux -L thurbox ls`.
One machine found 93 processes and 3.1 GB of resident memory in one four days
later, among them 22 agent processes — 13 duplicates of sessions also running
in the new server, and 9 of sessions since deleted, since a deletion only
reaches the server thurbox can talk to.

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
a JSON document for polling callers. This stream is UI state only;
`thurbox-cli watch` continues to stream durable session events.

List the target's live actions and their argument types, ownership, effect,
destructive classification, availability and current chords with
`thurbox-cli ui --instance <id> actions --json`. `thurbox-cli schema --instance
<id> --json` includes those same descriptors beside the CLI command tree.
Both read the running interface, so reloading a plugin updates them together.
When no TUI is running, `schema` marks its UI portion `no_running_ui` and still
lists headless CLI commands. With several running TUIs, select one explicitly.
The two initial typed actions are:

```sh
thurbox-cli ui --instance <id> action session.focus --session <session-uuid> --json
thurbox-cli ui --instance <id> action search.open --query 'error handling' --json
```

`session.focus` requires a session visible to the target TUI and a focusable
agent pane. `search.open`
opens the strip and sets its query; calling it again replaces the query without
closing the strip. The reply comes from the target's event loop after it applies
or refuses the action. The older `session focus` command still uses its shared
notification request and does not select a TUI instance.

Any non-destructive declared action can be called by its catalog name. Use
`--arg name=value` for an argument; `--session` and `--query` remain shortcuts
for the first two actions. A destructive action needs an explicit session UUID.
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

The SQLite schema migrates automatically (`schema_version` in
`metadata`). The TOML files carry a `config_version = 1` marker so a
future format change can migrate them too; current files are version 1
and the field is optional.

`schema_version` records what *ran*, which is not the same as what is *there*,
and the two came apart: migrations were originally written as
`let _ = conn.execute("ALTER …")`, which swallowed a real failure while the
version advanced anyway. One database in the field reported the current version
with none of v41's four columns on `sessions`, so every status signal failed
with `no such column: stopped_at` and no session on that machine ever reported
a state. The version gate could not repair it — the version was the thing that
was wrong — so the **additive** steps — those that add a column or a table of
their own if absent — are re-asserted on any open that runs the pass, not only
on an upgrade. A healthy database changes nothing and pays a handful of
catalogue reads.

An open whose database is already current skips the pass, and takes no write
lock at all: every agent hook runs a `thurbox-cli`, and each of those used to
queue behind any writer for a pass with nothing to do. "Current" is decided
from SQLite's own schema cookie (`PRAGMA schema_version`), which any DDL by
any binary changes. The last full pass records it in `metadata` as
`schema_cookie`. An open whose cookie, version and `min_reader_version` all
still agree skips the pass. A column dropped by hand, or a thurbox from before
this rule re-adding its own triggers, changes the cookie and gets the full pass
again. The retention sweeps on open also read before they delete, for the same
reason. Steps that rewrite or seed data stay gated on the version, because
applying one twice does not mean what applying it once meant.

The TUI, every agent hook's `thurbox-cli` and the host CLIs open the same file,
so the whole pass on open runs in one `BEGIN IMMEDIATE` transaction and reads
the version inside it: a second process opening an old database at the same
time waits, then finds it already migrated, instead of running a data step
again. Switching a brand-new file to WAL is retried for the same reason: two
processes doing it at once deadlock on the file lock, and SQLite fails one
without waiting.

A downgrade keeps working. A database whose `schema_version` is **newer** than
the binary knows still opens: the older thurbox runs none of its own steps
against it, leaves the version alone, and writes only what it knows. That is
safe because migrations stay **additive** — new columns nullable or defaulted,
new tables, nothing renamed, retyped or dropped. A migration that has to break
that also raises `min_reader_version` in `metadata` (`MIN_READER_VERSION` in
`src/storage/schema/mod.rs`) to its own schema version. A thurbox whose schema
version is below that value then refuses the database with an error naming
both versions and saying to update, rather than writing rows the schema no
longer describes. The value only ever rises. Binaries released before
`min_reader_version` existed do not read it.
