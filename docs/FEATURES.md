# Feature Decisions

Design rationale for user-facing behavior.
For architectural choices, see [ARCHITECTURE.md](ARCHITECTURE.md).

---

## Session Sidebar

### Single session list

The left sidebar holds a single flat list of sessions — there is no
project grouping layer above it. Sessions are top-level, identified
by a UUID v4, and labeled with their name, agent, branch (when in a
worktree), and cwd.

**Why no projects?**

- An earlier design grouped sessions under projects (one project →
  many sessions, with shared repos). In practice users tended to
  create one session per task, so the project layer was pure
  overhead: an extra navigation level, an extra creation step, and
  an extra deletion guard.
- Removing the project layer (storage migration v16 dropped
  `projects` and `project_repos`) collapses the model to "sessions
  own their own configuration". Each session picks its own agent
  and repos at creation time.

**Why a sidebar at all instead of a popup?**

- Sessions are persistent context, not transient selections. An
  always-visible list shows each session's status and live agent
  activity at a glance — useful for monitoring multiple parallel
  agent sessions.
- The sidebar fits cleanly into the existing 3-tier responsive
  layout (`<80`, `>=80`, `>=120`); a popup would require its own
  open/close keybinding and dismissal logic.

### Fuzzy search

Searching is unified into the **global search** (`Ctrl+A`) — see the
*Global Search* section below. There is no separate per-list `/`
filter; instead the global strip highlights matches live across the
session list, tasks panel, and automations pane at once. Sessions are
matched on name, agent, branch name, and cwd.

**Why all four fields?** Users remember sessions by whichever
attribute is most distinctive — sometimes the branch name, often
the agent ("the codex one"), occasionally the repo path. Indexing
all four makes the search hit on the first attempt without forcing
the user to remember which field to type into.

### Live status & "needs attention"

Each row is a single line: `<status-dot> <name> [<agent-status>]`
(worktree sessions get a `⑂` mark before the name). The agent-reported
status — the OSC activity title or an attention notification — is
appended after the name when present, muted and truncated with `…` to
fit the panel. There is no timing-based `Waiting`/`Busy` text: the
colored status dot already conveys that state, so a session with no
agent-reported status is just the dot and the name. The repo/branch
and agent live in the info panel, not the list row.

The status is richer than raw output-timing. We parse the agent's
own terminal signals from its PTY (via the `vt100` callbacks already
used for the window title):

- **OSC title** (`0`/`1`/`2`) → shown as live activity when the agent
  reports one (e.g. Gemini's `◇ Ready`).
- **Attention** — a terminal bell (`BEL`) or a desktop-notification
  escape (**OSC 9**, **OSC 777**) means the agent finished or is
  waiting for input. This latches a distinct `Needs attention` status
  (`▲`, accent colour) that shows the notification's message text when
  present, and **floats the session to the top** so blocked or finished
  agents surface first (see *Smart ordering* below). It clears
  automatically once you select the session (you're now looking at it).

### Smart ordering & repo groups

The list is ordered by **activity and status**, then **grouped by
repository** under subtle headers (`── webapp ─────`):

- Within a group, sessions sort by status rank
  (`Attention → running → Idle → Error`), tie-broken by stable insertion
  order. `Busy` and `Waiting` deliberately **share one "running" rank**: a
  live agent flickers across the ~1s output boundary every tick, so ranking
  them apart (or sorting by live recency) would make active sessions churn up
  and down the list endlessly. Ordering is a pure function of *status* and
  *stable order*, never of live timing — so the list only re-sorts on a real
  transition (a session needs attention, or exits).
- Groups are ordered by their **most-urgent member**, then by name, so a repo
  holding an `Attention` session bubbles above a merely-running one.
- The group key is the **set of repos a session spans** (order-independent), so
  a multi-repo session forms its own group with a combined header
  (`webapp + infra`) rather than being filed arbitrarily under one repo;
  sessions touching the same set cluster together. Sessions with no repo share a
  `(no repo)` group.

**Why group by repo?** With several parallel agents the dominant question
is "which project is this?" — clustering same-repo sessions answers it at a
glance while urgency still wins globally (an `Attention` session never hides
below a quiet repo). A single comparator
(`ui::project_list::compute_session_order`) drives both rendering and
`Ctrl+J`/`Ctrl+K` navigation, so the keyboard always steps through the exact
order shown.

**Why signals instead of guessing?** Pure output-timing can only say
"quiet for >1s"; it can't tell a thinking pause from "done" or "needs
you". The agents already emit these signals — we just read them. This
mirrors how dashboards like Orca surface working / waiting / finished.

**Caveat (Claude in tmux):** Claude Code only emits the OSC 9 desktop
notification for Ghostty/Kitty/iTerm2, so inside thurbox's tmux pane
set `claude config set --global preferredNotifChannel terminal_bell`
to get the bell we can detect. We capture bell + OSC 9 + OSC 777,
whichever the agent produces.

---

## Session Creation

![Session creation workflow](media/thurbox-session-creation.gif)

`Ctrl+N` walks through a series of modals to configure a new
session. Each step has a sensible default and can be skipped when
not applicable.

1. **Host picker** — choose where the session runs: `local`, or any
   remote SSH host defined in `hosts.toml`. Skipped entirely when no
   remote hosts are configured (preserving the local-only flow). For
   a remote host the repo picker opens to a typed remote path and the
   worktree + tmux window are created on that host over SSH.
2. **Repo picker** — fuzzy-searchable list of bookmarked repo
   paths. `Space` toggles selection, `w` marks the selected repo
   as a worktree base, `d` deletes the bookmark, and a path-input
   field with filesystem autocomplete adds new bookmarks. The
   first selected repo becomes the session's `cwd`; the rest may be
   exposed to the agent depending on the agent's own flags.
3. **Base branch selector** — worktree mode only.
4. **Session name** — free text identifier shown in the sidebar.
5. **New branch name** — worktree mode only.
6. **Agent picker** — choose which coding agent runs in this
   session. Skipped when only one agent is defined in
   `agents.toml`.

A session is fully described by its repos and agent. There is no
per-session model selection, permissions, prompt, tool, or skill
configuration — those concerns belong to the agent CLI itself,
which runs with its own default config.

**Why per-session repo selection?** Each session is its own context,
so it makes sense to pick repos at creation time rather than
inheriting from a parent grouping. Mixed sessions are supported:
some repos may be worktree-based (new branch created) while others
are added as-is.

**How does one agent reach multiple repos?** Agent CLIs disagree on
how (or whether) to accept extra directories, so thurbox stays
agent-neutral: a multi-repo session is launched in a per-session
**symlink workspace** (`~/.local/share/thurbox/workspaces/<id>/`)
holding one symlink per repo, with the agent's cwd set there. Every
agent then sees each repo as a subdirectory — no per-agent flags and
no `agents.toml` changes. The workspace is only symlinks, rebuilt
idempotently on each launch and removed (without touching the repos)
when the session is deleted. Single-repo sessions launch directly in
the repo as before.

**Why per-session agent?** Different tasks suit different agents.
Choosing the agent at creation time keeps each session
self-describing and lets you mix agents across the sidebar
(Claude here, Codex there) with no shared global configuration.

**Why a bookmark list rather than a path picker every time?** Users
work on the same handful of repos repeatedly. Bookmarks make the
common case a 2-keystroke selection while still allowing arbitrary
paths via the input field. Bookmark deletion (`d`) keeps the list
from accumulating stale entries.

### Agent definitions

The set of available agents is **data**, not code. On first run
Thurbox seeds `~/.config/thurbox/agents.toml` with built-in
definitions for claude, codex, gemini, opencode, aider, and vibe
(`agent::agent_config::load_or_seed`). Editing the file — adding an
`[[agents]]` entry or tweaking an existing one — extends the agent
picker with no recompile.

Each definition (`session::AgentDef`) carries:

- `name` — display + lookup key, unique in the registry.
- `command` — the CLI executable to launch.
- argument-template groups: `args` (always passed — bake in flags
  like a model here if you want) and `resume_args` / `fork_args` /
  `new_session_args` (with `{id}`).
- `resume_latest` — when true, restart resumes the agent's most
  recent session in the launch directory via **id-less** flags
  (see below).

`agent::GenericProvider` builds the launch arguments by appending
each group **only when its driving value is present**, substituting
`{id}` token-by-token. Selection precedence is fork > resume >
new-session id; static `args` follow. A group with no value is
simply omitted — no unresolved-placeholder heuristics.

Only `claude` accepts the thurbox-generated id at creation
(`--session-id {id}`), so only it resumes/forks by that exact id.
The other built-ins can't pin or report their session id, so they
set `resume_latest = true` and resume/fork via id-less, cwd-scoped
flags (`codex resume --last`, `opencode --continue`, `gemini
--resume latest`, `aider --restore-chat-history`); the agent
resolves "the last session in this directory" itself, which works
because restart reuses the session cwd and a single-repo fork reuses
the parent cwd. Agents that declare no `resume_args` start fresh on
restart instead of resuming.

Example:

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
resume_args = ["resume", "--last"]   # id-less: last session in cwd
fork_args = ["fork", "--last"]
resume_latest = true
```

### Remote SSH sessions

Like agents, remote hosts are **data**. A session can run on a
remote machine over SSH while the TUI stays local. Hosts are
declared in `~/.config/thurbox/hosts.toml` (seeded commented-out, so
a fresh install has none and behaves exactly as before):

```toml
[[hosts]]
name = "devbox"            # selectable as backend "ssh:devbox"
destination = "me@devbox"  # resolved via ~/.ssh/config
ssh_opts = ["-o", "ControlMaster=auto", "-o", "ControlPersist=10m"]
```

Each `[[hosts]]` entry (`session::HostDef`) has two required fields
and four optional ones — the seeded `hosts.toml` documents each
inline:

| Field | Required | Default | Meaning |
|-------|----------|---------|---------|
| `name` | yes | — | unique id; registers the backend `ssh:<name>` and is what `--host` expects |
| `destination` | yes | — | ssh target (`user@host` or a `~/.ssh/config` alias) |
| `ssh_opts` | no | `[]` | extra `ssh` flags, one token per array element; no `~` expansion (use absolute paths) |
| `socket` | no | `thurbox` | remote `tmux -L` socket name |
| `session` | no | `thurbox` | remote tmux session name |
| `worktrees_dir` | no | `$HOME/.local/share/thurbox/worktrees` | absolute remote dir for git worktrees |

Each host becomes a session backend named `ssh:<name>`. Thurbox
shells out to the system `ssh` binary, so authentication, keys, and
connection multiplexing come from your `~/.ssh/config` — thurbox
never handles credentials. The same tmux control-mode protocol runs
over the SSH pipe, so remote sessions get identical persistence,
multi-instance sharing, and restore-on-startup as local ones; the
worktree and agent process live on the remote host. In the session
list a remote session is marked with a `☁` glyph (and the info panel
shows its `Host:`), mirroring the worktree `⑂` mark.

**Why a config file rather than ad-hoc destinations?** Named hosts
give the picker stable, readable entries and let `backend_type`
round-trip cleanly through the database so a remote session re-adopts
on the correct host after a restart.

**Why lean on `~/.ssh/config`?** Re-implementing SSH auth, agent
forwarding, and ControlMaster multiplexing would be a large, fragile
surface. Deferring to the system `ssh` keeps thurbox out of the
credential path and inherits whatever the user already configured.

Headless: `thurbox-cli session create --host devbox --repo-path
/srv/repo --worktree-branch feat/x` does the same over the CLI.

---

## Keybinding Design

### Philosophy: Ctrl = global, everything else = PTY

When the terminal panel is focused, **all keys are forwarded to the
PTY** except those with a `Ctrl` modifier (intercepted as global
commands) and `Shift+arrow/page` keys (intercepted for scrollback).

**Why Ctrl, not Alt?**

- Coding-agent CLIs and shell programs heavily use Alt-key
  combinations. Intercepting Alt would break readline, vim, and the
  agent's own keybindings.
- Ctrl has well-established precedent for "meta" actions in
  terminal multiplexers (tmux uses `Ctrl+B`, screen uses `Ctrl+A`).
- Ctrl combos are easier to type one-handed, which matters for a
  tool used alongside other terminals.

### Keybinding Table

All global keybindings use `Ctrl` and follow Vim conventions where
applicable: `h/j/k/l` for navigation, semantic letters for actions
(`D`=delete, `N`=new, `R`=restart, `Q`=quit).

| Key | Context | Action | Mnemonic |
|-----|---------|--------|----------|
| `Ctrl+Q` | Global | Quit Thurbox (detach sessions) | **Q**uit |
| `Ctrl+N` | Global | New session (opens repo picker) | **N**ew |
| `Ctrl+C` | Terminal | Copy selection, or send SIGINT if none | **C**opy |
| `Ctrl+V` | Terminal | Paste from clipboard into PTY | Paste |
| `Ctrl+P` | Global | Automations (scheduled agent runs) | **P**rogram |
| `Ctrl+W` / `F5` | Global | Toggle tasks panel (todo list) | Work items |
| `Ctrl+A` | Global | Global search across every scope | search **A**ll |
| `Ctrl+T` | Global | Toggle shell pane alongside the agent session | **T**erminal |
| `Ctrl+H` | Global | Focus previous pane (cycle backward) | Vim: **h** = left |
| `Ctrl+J` | Global | Select next session | Vim: **j** = down |
| `Ctrl+K` | Global | Select previous session | Vim: **k** = up |
| `Ctrl+L` | Global | Focus next pane (cycle forward) | Vim: **l** = right |
| `Ctrl+D` | Session list | Delete selected session | Vim: **d** = delete |
| `Ctrl+O` | Global | Open active session's worktrees in editor | **O**pen |
| `Ctrl+R` | Global | Restart active session | **R**estart |
| `Ctrl+F` | Global | Fork active session | **F**ork |
| `Ctrl+S` | Global | Sync all worktree sessions with origin/main | **S**ync |
| `Ctrl+Z` | Global | Undo session delete | **Z** = undo |
| `Ctrl+U` | Global | Restore deleted sessions list | **U**ndelete |
| `Ctrl+Y` / `F4` | Global | Pick TUI theme | Color **Y**oke |
| `F1` / `Ctrl+G` | Global | Keybindings help + interactive editor | Universal help |
| `F2` | Global | Toggle info panel | Next to F1 |
| `F3` | Global | Toggle file viewer | Next to F2 |
| `j` / `k` | F1 editor | Select action to rebind | |
| `Enter` / `r` | F1 editor | Capture a new chord for the selected action | **R**ebind |
| `d` | F1 editor | Reset selected action to its default chord(s) | **D**efault |
| `Shift+D` | F1 editor | Reset all actions to their defaults | Reset all |
| `Esc` | F1 editor | Close (or cancel an in-progress capture) | |
| `j` / `Down` | Lists | Next item | |
| `k` / `Up` | Lists | Previous item | |
| `Enter` | Global search | Jump to selected result | |
| `Esc` | Global search | Close search | |
| `Enter` | Session list | Focus terminal | |
| `j` / `Down` | Repo picker | Next repo | |
| `k` / `Up` | Repo picker | Previous repo | |
| `Space` | Repo picker | Toggle repo selection | |
| `w` | Repo picker | Toggle worktree mode for repo | |
| `d` | Repo picker | Delete bookmark | |
| `Tab` | Repo picker | Switch to path input | |
| `Enter` | Repo picker | Confirm selection | |
| `Esc` | Repo picker | Cancel | |
| `Shift+Up` | Focused terminal | Scroll up 1 line | |
| `Shift+Down` | Focused terminal | Scroll down 1 line | |
| `Shift+PageUp` | Focused terminal | Scroll up half page | |
| `Shift+PageDown` | Focused terminal | Scroll down half page | |
| Mouse wheel | Focused terminal | Scroll up/down 3 lines | |
| All other keys | Focused terminal | Forwarded to PTY (snaps to bottom if scrolled) | |

### Customizing shortcuts

Nearly every shortcut can be remapped, including copy/paste, file-viewer
navigation, session-list navigation, and terminal scroll. The F1 panel doubles
as a live editor: select an action with `j`/`k`, press `Enter`/`r`, then press
the chord you want — the next physical keypress (including chords like
`Ctrl+Q`) becomes that action's sole binding. `d` restores the selected
action's defaults, and `Shift+D` resets every action at once (removing the
override file). If the chord conflicts it is reassigned from the other action
and a status toast reports the move. Changes persist immediately to
`~/.config/thurbox/keybindings.json` (`Action` name → chord strings, e.g.
`{ "QuitApp": ["ctrl+x"] }`) and take effect on the next keystroke — no
restart. The file can also be hand-edited directly.

**Context-scoped keys.** Each action belongs to a scope — `Global`,
`SessionList`, `FileViewer`, or `Terminal`. Global actions fire anywhere;
scoped actions fire only while their pane is focused, so the same single-letter
key (e.g. `j`) can drive both the file viewer and the session list while the
terminal still forwards it to the shell. Conflicts are only flagged between
actions whose scopes overlap. A handful of stateful keys stay fixed (shown in
the F1 panel under *Fixed (not rebindable)*): modal selectors
(`j`/`k`/`Enter`/`Esc`), the automations/tasks panes, the file-viewer search
sub-mode, and the terminal's catch-all PTY forwarding.

---

## Session Lifecycle

```text
Create (UUID v4) → Running → Idle / Error
                      ↓
                  Shutdown (SIGHUP)
```

### States

- **Running**: PTY is alive, read loop is active, output is
  streaming to the terminal widget.
- **Idle**: the agent CLI has exited cleanly (exit code 0). Session
  is still displayed but no longer accepts input.
- **Error**: PTY or the agent CLI exited with a non-zero code. Error
  details shown in status bar.
- **Shutdown**: Triggered by the user closing a session or quitting
  the app. Sends `SIGHUP` to the PTY child process, then waits for
  clean exit before dropping resources.

### Session Restart (`Ctrl+R`)

Restarts the active session's tmux pane while preserving the
conversation history. The session is killed and respawned with the
agent's resume arguments (e.g. `--resume <id>` for Claude, or
id-less `resume --last` for a `resume_latest` agent like codex),
reusing the session's stored agent. Agents that define no
`resume_args` simply start a fresh conversation.

**Why restart instead of close + new?**

- Closing destroys the agent's session ID. Restarting uses the
  agent's resume arguments so the conversation context is
  preserved (when the agent supports it).
- The session's `SessionInfo` (ID, name, agent, repos)
  stays intact — only the backend pane and I/O are replaced.

### Why UUID v4?

Sessions need unique identifiers for the lifetime of the process.
UUIDs are collision-free without coordination, simple to generate,
and usable as map keys. Sequential IDs would work too, but UUIDs
prevent bugs where an old session ID accidentally refers to a new
session after recycling.

---

## Editor Integration (`Ctrl+O`)

`Ctrl+O` opens the active session's working directories in a
configured external editor. The editor command is a global setting
stored in SQLite, defaulting to a sensible value on first run.

**Why a configurable command rather than `$EDITOR`?** `$EDITOR` is
typically a terminal editor (vim, nano) — wrong for "open this
folder in my GUI". A separate setting lets users point at
`code`, `cursor`, `idea`, etc. without disrupting their shell
environment.

**Why all worktrees, not just cwd?** Multi-repo sessions touch
several directories at once; opening only the cwd would hide the
rest. The editor command receives every working path so the user's
editor of choice can open them as a workspace.

---

## Automations

`Ctrl+P` opens the automations list. An **automation** is a named,
enable/disable-able task that fires on a schedule (one-shot or
recurring) and, when it fires, either pastes a prompt into an
existing session (**send**) or spawns a new session — optionally on
a fresh git worktree — and prompts it (**spawn**). This is the
Thurbox analogue of "scheduled agent runs": queue follow-up
prompts, run nightly maintenance, or kick off a fresh triage
session every weekday morning.

Automations replace the older one-shot "scheduled commands"
feature; a one-shot is simply an automation with a `once` schedule.

### Schedules

A schedule is either:

- **once** — fire a single time at an absolute timestamp
  (`at:<unix_millis>`), then disable itself.
- **cron** — a standard 5-field Unix cron expression (day-of-week
  `0`–`6`, `0` = Sunday). Friendly presets compile to cron:
  `hourly`, `daily`, `weekdays`, `weekly`, combined with an
  `HH:MM` time and optional IANA timezone (DST-correct via
  `chrono-tz`; defaults to system local time).

`next_run_at` (unix millis) is computed from the schedule and is
the dispatcher's scan key. After each fire it is recomputed; a
spent one-shot clears it and disables the automation.

### Actions

- **send** — bracketed-paste the prompt into the target session,
  followed by a deferred Enter. Skipped (and logged as such) if the
  target session is not currently running.
- **spawn** — create a session named `auto-<id>` (reusing it on
  later fires, including after a TUI restart where it is restored
  by name), optionally on a worktree off a base branch, with the
  chosen agent. The prompt is delivered after a short boot delay so
  the agent CLI has time to start. Worktree provisioning is
  **idempotent** (`git::create_or_attach_worktree`): if the session
  was closed but its worktree/branch still exist, a later fire
  reuses them rather than failing with "branch already exists".

### Execution model

Automations fire from **three** places, all going through the same
`thurbox-cli automation tick` logic and made safe by **claim-based
firing** (see below):

1. **TUI tick loop** (`process_automations`, ~1 s cadence) — while
   the TUI is open. On startup it runs an immediate catch-up pass
   so runs missed while the TUI was down fire once on boot.
2. **tmux heartbeat keeper** — a detached `automation-heartbeat`
   window (armed on TUI startup and on `thurbox-cli automation
   create`) that loops `thurbox-cli automation tick` every 60 s.
   Because it is a live tmux window it also keeps the tmux server
   alive, so automations — **including spawn** — fire even after
   the TUI is closed and even with no other sessions open. This
   restores (and generalizes) the old scheduled-command behavior
   of firing while the TUI is shut down.
3. **Optional OS timer** — `packaging/systemd` / `packaging/launchd`
   units run the same `tick` for reboot-proof, tmux-independent
   firing. Opt-in.

**Claim-based firing (no double-fire).** Before acting, every firer
performs an atomic compare-and-swap
(`Database::claim_due_automation`): it advances `next_run_at` *only
if* the row still holds the value it observed as due. Exactly one
firer wins; the rest skip. So the TUI, the keeper, and an OS timer
can all run at once without an automation firing twice. Ordering is
claim-then-act (at-most-once): a crash between claim and side effect
loses a run rather than duplicating one.

**Headless send vs spawn.** `send` types into the still-alive tmux
window (`send_prompt_now`). `spawn` creates the session headlessly
(`spawn_session_headless`); the prompt is delivered via a short
deferred `tmux run-shell` timer once the agent boots, and the TUI
adopts the `auto-<id>` session by name on its next startup. All of
this is local-tmux scoped today; a future remote/SSH `SessionBackend`
would plug into the same dispatch seam.

### Automations pane

A dedicated **Automations** pane sits beneath the session list in
the left column. It is **always present** (showing `none` when
empty) as long as the column is tall enough for both lists; its
height grows with the automation count (capped). Each row reads
`● name — schedule · action · next-run`. It is treated as **part of
the session pane**: it forms one continuous vertical list with the
session list, so `j` past the last session drops focus into the
pane and `k` at the top automation hands focus back to the last
session. Once focused: `j`/`k` select, `Space` toggle enabled, `r`
run-now, `d` (or `Ctrl+D`) delete, and **`Ctrl+N`/`n` create a new
automation** (works even on an empty pane).

The pane behaves **exactly like the session list**, with the
central pane as its terminal-equivalent: while the pane is focused,
the central pane shows a **single editor** for the selected
automation (a live, read-only-looking preview — no separate "info"
screen). Pressing **`Enter`** (or **`Ctrl+L`**, or `e`) moves focus
*into* that editor — just like `Enter`/`Ctrl+L` on a session focuses
its terminal — where you can change fields; **`Ctrl+H`** (or `Esc`)
returns to the list. `Enter` in the editor saves; `Esc` discards.
`Ctrl+E` toggles the automation's enabled flag from inside the
editor (the global file-viewer binding is suppressed there).

The scoped automation's **run history** is shown beneath the editor:
each row reads `<status> <clock time> <relative age> <detail>` with
the status (`ok`/`error`/`skipped`) colour-coded and bold. Press
`Ctrl+L` again (from the editor) to focus the history panel, then
`j`/`k` to move the cursor over runs; the panel footer shows its
shortcuts — **`r` runs the automation now**, **`Enter` jumps to the
session that run touched** (the send target / spawned session, when
it's still open), `Esc` returns to the editor. While in this whole
context the session list above
de-emphasises itself (no accent border, no selected-row highlight)
since the active session is irrelevant there.

`Ctrl+L`/`Ctrl+H` cycle **within the current context only** — the
automation ring is `Automations → editor → run history` and wraps
back to `Automations` (it never jumps off to a session; returning to
the list discards unsaved edits, just like `Esc`). The session ring
is the usual `SessionList → Terminal` (+ file viewer). Switching
*between* the two contexts is done with `j`/`k` in the left column,
not the focus cycle.

### Ctrl+P list + editor

`Ctrl+P` opens the same set over the full list (a modal, available
at any width). Keys: `n` new, `e`/`Enter` edit, `Space` toggle
enabled, `r` run-now, `d` delete, `Esc` close.

The editor avoids typing schedules by hand. **Trigger** is a
selector cycled with `←/→` — `once`, `hourly`, `daily`,
`weekdays`, `weekly`, or `cron` — and the form adapts to it:

- `once` → an **In** delay field (`30m`, `2h`, `1h30m`, `1d`).
- `hourly` → a **Minute** stepper.
- `daily`/`weekdays` → **Hour** + **Minute** steppers.
- `weekly` → a **Weekday** selector + Hour/Minute.
- `cron` → a raw expression field for power users.

**Action** is a `‹ send ›`/`‹ spawn ›` selector. For **send**, a
**Target** selector (also cycled with `←/→`) lets you pick which
running session receives the prompt — it defaults to the active
session and lists every session; saving is rejected if none exist.
For **spawn**, the **Repo**/**Worktree**/**Agent** text fields
appear instead (a leading `~` in the repo path is expanded).

`Hour`/`Minute`/`Weekday`/`Action`/`Target` are steppers/selectors
(`←/→` adjust, wrapping); `Tab`/`↑↓` move between fields; `Space`
also adjusts the focused selector/stepper; `^E` toggles enabled;
`Enter` saves. A live **next:** line previews when the automation
will fire (or shows the validation error for the current input).
Editing an existing automation reverse-maps its cron back into the
structured fields where it matches a known preset shape; otherwise
it opens as raw `cron`.

### Persistence

Automations live in the `automations` SQLite table (`name`,
`enabled`, `schedule_kind`/`schedule_spec`, `timezone`,
`action_kind` plus action columns, `prompt`, timestamps,
`last_run_at`, `next_run_at`), with a partial index on
`next_run_at` (where enabled and non-null) for the due-scan. Each
fire appends to `automation_runs` (`status` = success/skipped/error
plus a free-text `detail`) for history.

### Headless access (`thurbox-cli`)

`thurbox-cli automation` (alias `auto`) provides
`create`/`list`/`show`/`edit`/`remove`/`run`/`runs`/`tick` without
the TUI, sharing the same tables. `run` marks an automation due;
`tick` fires all currently-due automations headlessly (this is what
the tmux keeper and the optional OS timers invoke).

---

## Tasks (todo list)

A **task list** of todo items that can be **connected to a coding
agent**. Tasks deliberately reuse the automation **Send/Spawn** action
model: triggering a task either pastes its title into an existing
session (`Send`) or spawns a new session — optionally on a fresh
worktree — seeded with the title (`Spawn`). A task with no action is a
plain local todo. This keeps tasks and automations on one shared
dispatch path (`App::spawn_and_prompt`).

### Why mirror automations?

The agent linkage a task needs (*"send this to an agent"* / *"spin up
an agent for this"*) is exactly what `AutomationAction` already models.
Rather than a parallel `TaskAction`, a task stores
`Option<AutomationAction>` — the `Option` adds the only new case
(unconnected local todo). One enum, one column layout, one fire path.

### Where it lives in the UI

Tasks render in a **toggleable right-side column** that sits between
the terminal and the file viewer — it behaves exactly like the file
viewer pane. **F5**/`Ctrl+W` shows and hides it (showing it also
focuses it); while visible it is a stop in the session focus ring, so
`Ctrl+L`/`Ctrl+H` cycle `SessionList → Terminal → TaskList →
FileViewer` (each extra column appears only when shown). The column is
a 20% slice added by `compute_layout` at width ≥ 120.

The panel is focusable (`InputFocus::TaskList`). Its title and border use
the shared focus styling (highlighted title + accent border when focused),
matching the session list and file viewer. Checkbox glyphs show status
(☐ todo / ◐ in-progress / ☑ done). Searching/filtering is handled by the
global `Ctrl+A` search, not a per-panel `/`.

**Editing happens in the central pane, like automations — not a modal.**
Selecting a task previews its editor in the central pane; `Enter`/`e`
focuses that editor to change fields; `Enter` saves and returns to the
panel, `Esc` discards and returns. Beneath the editor a read-only
**Details** panel shows the task's agent linkage, status, source, and
created/updated times (tasks have no run history, so this takes the place
of the automations' run-history panel). The action field cycles
Local → Send → Spawn.

Focused keys: `j`/`k` select (live-preview the editor), `n` new,
`e`/`Enter` edit in the central pane, `Space` cycle status, `r` run the
action, `d`/`Ctrl+D` delete, `Esc` leave.

### Persistence

Tasks live in the `tasks` SQLite table (added in schema **v25**; the
markdown `description` column followed in **v26**): `title`,
`description`, `status`, the automation action columns (`action_kind`
nullable for local todos), `source`/`external_id`/`external_url`,
timestamps, and a
`deleted_at` soft-delete marker, with a partial index on `status`.
Mutations are recorded in `audit_log` under `EntityType::Task`. Tasks
do **not** join the cross-instance `SharedState` (like automations) and
have **no** run-history table.

### External sync (deferred)

The `source`/`external_id`/`external_url` columns are scaffolding for a
future sync with external trackers (Jira, GitHub Issues, …). Local
tasks use `source = "local"`; imported tasks will slot in with no
migration. No fetch logic ships yet.

### Headless access (`thurbox-cli`)

`thurbox-cli task` (alias `todo`) provides
`create`/`list`/`show`/`edit`/`remove`/`run`. `create` with neither
`--session` nor `--repo` is a plain local todo; `run` triggers the
task's Send/Spawn action headlessly (spawned sessions are named
`task-<id>-<title-slug>` via `Task::spawn_session_name`, adopted by the
TUI on next startup; `Task::matches_spawn_session` also recognizes the
legacy bare `task-<id>` form and spawns from a since-edited title).

---

## Flow Extension (experimental)

> **Status:** brand-new and under active testing — the spec, scripts,
> and installer are all expected to change between releases.

An opt-in add-on (`extensions/flow/`) that composes the task list,
sessions, worktrees, and automations into a **focus-protecting triage
workflow**: a dedicated cheap *flow session* captures brain-dumps into
tasks, dispatches the dispatchable ones to worker sessions, monitors
them, grooms the backlog, and ends every reply with the single next
thing to focus on (`🎯 Next: …`).

### Agent-agnostic by construction

Nothing in the extension names a vendor:

- The behavior is a plain context file, `FLOW.md`, installed into the
  flow home (`~/flow`) and surfaced to whichever CLI runs the session
  via symlinks to each CLI's context convention
  (`CLAUDE.md`/`AGENTS.md`/`GEMINI.md` → `FLOW.md`).
- The triager and workers are **agents.toml aliases** — `flow`,
  `flow-worker` (default), `flow-worker-heavy` (long/hard work) — that
  the installer seeds with defaults and the user remaps freely.
- All orchestration goes through `thurbox-cli` (`task create/run`,
  `session capture/send`, `automation create`) plus `jq`; the core
  binary has no flow-specific code.

### Dispatch model

Dispatch is **eager**: capture creates the task *and* spawns its
worker in one atomic helper call (`create-task.sh`); workers send a
`tick` back to the flow session when they finish so a freed capacity
slot dispatches the next task immediately; a `flow-tick` cron
automation (default every 5 min) is only the safety net that catches
crashed workers and stale state. Workers always get a
`flow/<task-slug>` worktree branch on git repos, so they never dirty
the main checkout and parallelize per repo. Completion is detected by
task status (workers self-mark done) with an orchestrate-style
`===RESULT===` JSON sentinel as the fallback, parsed from
`session capture` output.

### Install

`extensions/flow/install.sh` (curl-able, idempotent, POSIX sh) sets up
the flow home, appends missing agents.toml aliases (overridable via
`FLOW_CMD`/`WORKER_CMD`/`WORKER_HEAVY_CMD` + `*_ARGS` env vars),
creates the dedicated `flow` session and the `flow-tick` automation.
See `extensions/flow/README.md`.

---

## Global Search

`Ctrl+A` ("search **A**ll") opens a **non-modal bottom strip** that
searches every scope at once — a single place to find and jump to
anything. The opener is fully rebindable from the F1 editor
(`Action::GlobalSearch`).

### Scopes

- **Sessions** — name, agent, and branch (fuzzy), plus the live terminal
  **buffer content** so you can find *which session* mentioned a string
  ("deploy failed", an error, a file path) and switch straight to it.
- **Tasks** — title (fuzzy).
- **Automations** — name (fuzzy).
- **Files** — file/dir names under the active session's roots (bounded
  walk, same node/depth limits as the in-viewer search).

### Live preview & cancel

Moving through results (`↑`/`↓`) **previews** the selection in place: the
matching panel's cursor follows the highlighted result — the active session
switches, or the selected task / automation row moves — so you see where
`Enter` would land without leaving the search box. (Files aren't previewed
live; they only open the file viewer on `Enter`, since rebuilding the tree
per keystroke is heavy.)

The state the search touches is **snapshotted on open**, so `Esc` cancels
back to exactly where you were — selections, focus, and which optional
panels were visible all restore. `Enter` commits the jump and focuses the
result's pane: a session result switches the active session and focuses its
terminal; a task focuses the tasks panel with that row selected; an
automation focuses the automations pane; a file opens the file viewer and
reveals the path.

### Live in-place highlighting

As you type, matches highlight **where they live**: the session list, tasks
panel, and automations pane highlight the matched characters (accent, bold,
underlined) on matching rows and **dim** the rows that don't match — the
same treatment the session list's own `/` filter already uses. This is
driven by a shared highlight helper (`src/ui/highlight.rs`) and
`App::global_search_query()`, which the view feeds into each panel renderer
while the strip is open.

The strip itself shows: a query line, a one-line per-scope match summary
(`3 sessions · 1 task · 2 files [2/6]`), the **grouped result list**
(scrollable, with the selected row marked `▸` and highlighted; content
matches show a dim snippet), and a row of key hints. `↑`/`↓` move the
selection through the list and `Enter` jumps to it.

### Why a bottom strip (not a modal)

thurbox keeps heavy interactions out of centered modals where it can. The
search is a full-width strip docked above the footer — the content area
shrinks to make room (the same way the info/tasks/file columns share
width), so the rest of the UI stays visible, live, and **highlighted**
behind it.

### Responsiveness

Cheap metadata matches (names/titles, fuzzy) recompute on every keystroke.
The expensive part — scanning each running session's vt100 buffer — is
**debounced** (~150 ms of query-idle, measured with `Instant` since the
tick cadence varies with event load) and capped (≤ 8 results per group,
last ≤ 500 lines per session) so typing never stalls.

### Keys & bindings

Type to filter; `Up`/`Down` (or `Ctrl+P`/`Ctrl+N`) move the selection so
plain letters still edit the query; `Enter` jumps; `Esc` closes and
restores the previous focus. The default chord is `Ctrl+A`
(`Action::GlobalSearch`), which encodes reliably on every terminal and is
fully rebindable from the F1 editor like any other action.

Global search is the **only** list search now: the old per-pane `/`
filters (session list, tasks panel) were removed in its favour. The file
viewer's `/` is an unrelated in-file text search and is unchanged.

---

## Error Handling UX

### Rule: never crash, never modal

Errors are shown in the status bar footer as transient messages.
They do not block interaction, do not require dismissal, and
auto-clear after a timeout or on the next successful action.

**Why non-modal?**

- Modal error dialogs in a TUI are jarring — they steal focus from
  the terminal where the user is working.
- Most errors are recoverable (session failed to start, PTY read
  error). Showing them passively lets the user decide when to act.
- Fatal errors (can't initialize terminal) are the only case where
  the app exits, and those happen before the TUI is even rendered.

---

## Responsive Layout

The info panel (`Ctrl+B`) and file viewer (`Ctrl+E`) are the
optional columns that appear at wider widths:

![Info panel](media/thurbox-info-panel.gif)

![File viewer](media/thurbox-file-manager.gif)

### Breakpoint Rationale

| Width | Layout | Why |
|-------|--------|-----|
| `<80` | Terminal only | Sidebar would leave <60 cols — too narrow |
| `>=80` | Sidebar + terminal | 20-col sidebar + 60-col terminal min |
| `>=120` | Sidebar + terminal + info | Terminal still gets ~70+ cols |

### Why not user-configurable?

Configurable breakpoints add UI, storage, and edge-case complexity
for minimal gain. The fixed values cover standard terminal sizes
(80, 120, 160+). If a user resizes their terminal, the layout
adapts instantly. Custom breakpoints can be added later if real
demand emerges.

---

## Git Worktree Integration

Sessions can optionally run inside git worktrees for branch
isolation. This is opt-in by marking a repo with `w` in the repo
picker.

### Flow

1. `Ctrl+N` triggers session creation and opens the repo picker.
2. Marking a repo with `w` in the picker routes through the
   worktree branch flow.
3. A base branch selector lists local branches from the selected
   repo.
4. Selecting a base branch opens a prompt for the new branch name.
5. Confirming creates a new git branch (from the selected base) in
   a worktree and spawns the session inside it.
6. Mixed sessions are supported: worktree-marked repos get a new
   branch while normal repos are added as-is.

### Worktree storage

Worktrees are created at
`<repo>/.git/thurbox-worktrees/<sanitized-branch>`, where `/` in
branch names is replaced by `-`.

### Cleanup behavior

- Closing a worktree session (`Ctrl+D`) automatically removes the
  worktree via `git worktree remove --force`.
- Quitting Thurbox (`Ctrl+Q`) preserves worktrees on disk so they
  can be resumed on next launch (see [Session Persistence](#session-persistence)).
- Cleanup errors are logged but do not block session close or app
  shutdown.

### UI indicators

- **Terminal title**: Worktree sessions show the branch in the
  title bar: `my-session [feature/foo] [Running]`.
- **Session list**: Branch name appears next to worktree sessions
  with a green `[branch]` badge.
- **Info panel**: Shows a "Worktree" section with branch name and
  worktree path when viewing a worktree session.

---

## Worktree Sync

`Ctrl+S` synchronizes all worktree sessions with their upstream
default branch. The operation runs in the background — the TUI
stays responsive throughout.

### Algorithm

Sessions are grouped by repository path so that worktrees sharing
the same `.git` directory are synced sequentially (avoiding git
lock contention). Different repositories sync in parallel.

Per-worktree steps:

1. **Clean stale index locks** — removes `.git/index.lock` from
   crashed git processes (see below).
2. **Stash** — saves uncommitted changes so rebase can proceed on
   a clean tree.
3. **Fetch** — `git fetch` from origin.
4. **Rebase** — `git rebase origin/main` onto the latest upstream.
5. **Stash pop** — restores the stashed changes. If rebase fails
   (conflict), the stash is popped before reporting the conflict.

**Why stash instead of requiring a clean tree?** Agent sessions
frequently have uncommitted work in progress. Requiring a clean
tree would make sync unusable in the most common case.

**Why group by repo?** Worktrees linked to the same repository
share a single `.git` directory. Running concurrent git operations
against the same `.git` causes index lock conflicts. Sequential
processing within a repo group eliminates this.

### Stale index lock cleanup

Before stashing, Thurbox checks for stale `.git/index.lock` files
left behind by crashed git processes:

- **Linux**: reads the PID from the lock file and checks
  `/proc/{pid}` — removes the lock if the process is dead.
- **Fallback** (all platforms): removes locks older than 60 seconds
  based on file mtime.

If the first stash attempt fails with a lock-related error,
Thurbox retries up to 3 times with increasing delays (100 ms,
500 ms, 1 s) after cleaning stale locks.

### Results

Each worktree reports one of three outcomes:

- **Synced** — rebase succeeded, stash restored.
- **Conflict** — rebase failed due to merge conflicts. The conflict
  details are sent to the session's agent as a prompt asking it to
  resolve the rebase.
- **Error** — fetch or stash failed. The error message is shown in
  the status bar.

The status bar summarizes results: `"3 worktree(s) synced"` or
`"2 synced, 1 conflict(s)"`.

### Non-blocking execution

Sync runs on background threads via an `mpsc` channel. The main
event loop polls `try_recv()` each tick to collect results as they
complete. The TUI remains fully interactive during sync.

---

## Session Persistence

Sessions run inside a dedicated tmux server (`tmux -L thurbox`)
and survive thurbox crashes, restarts, and even multiple concurrent
thurbox instances.

### How it works

- Sessions spawn as tmux windows in the `thurbox` session. The
  tmux pane keeps running regardless of thurbox's lifecycle.
- On every session spawn, Thurbox assigns an `agent_session_id`
  (UUID v4) via the agent CLI's `--session-id` flag. This tells
  the agent to use a stable conversation ID from the start.
- On shutdown (`Ctrl+Q`), session metadata (including backend IDs)
  is written to the SQLite database at
  `$XDG_DATA_HOME/thurbox/thurbox.db`. Thurbox detaches from each
  session without killing it.
- On next startup, Thurbox discovers existing sessions from tmux,
  matches them to persisted metadata by `backend_id`, and adopts
  them — reconnecting to the live tmux panes with terminal content
  intact. Unmatched persisted sessions fall back to
  `--resume <session-id>` to create new tmux panes.
- External recovery is always possible via `tmux -L thurbox attach`.

### State storage

All session state is stored in the SQLite database (`thurbox.db`).
Tables include `sessions`, `worktrees`, `scheduled_commands`, and
`metadata`. The database uses WAL mode
for concurrent multi-instance access. Agent definitions are the
exception — they live in `~/.config/thurbox/agents.toml`.

### Worktree preservation

Worktrees are **not** removed on `Ctrl+Q` shutdown — they persist
on disk so the resumed session can continue working in the same
branch checkout. Worktree metadata (repo path, worktree path,
branch name) is saved in the database and reconstructed on restore.

### Explicit close vs quit

- **`Ctrl+Q` (Quit)**: Detaches from all sessions (tmux panes keep
  running), saves metadata. Sessions resume on next launch with
  terminal content preserved.
- **`Ctrl+D` (Delete)**: Soft-deletes the session — its tmux pane
  is killed and its worktree (if any) is removed. The database
  row is retained with `deleted_at` set so the deletion can be
  undone with `Ctrl+Z` (most recent) or restored from the
  `Ctrl+U` list.

### Multi-instance support

Multiple thurbox instances can view the same tmux sessions. Each
instance independently connects to tmux in control mode (`-C`).
Tmux broadcasts `%output` notifications to all connected clients —
there is no primary/secondary distinction.

---

## Parent Sessions (Lead/Worker)

Sessions carry an optional `parent_session_id` (nullable column on
`sessions`, schema v30) so orchestration scripts can model a lead
session that spawns workers: `thurbox-cli session create --parent
<uuid>` sets it, `session list`/`get` expose it, and `session list
--parent <uuid>` lists direct children. In the TUI, `Ctrl+F` fork
records the source session as the fork's parent.

### Why informational-only (no cascade)

The link is metadata, not a lifecycle contract. Deleting a parent
does **not** delete or orphan-block its children — workers routinely
outlive the lead that spawned them (the lead finishes orchestrating
while workers keep coding). A dangling parent id is harmless: the
child simply renders as a top-level session again. The parent is
validated once, at creation (it must be an existing active session),
and never re-validated.

### Why nesting stays inside repo groups

The session list's primary grouping is the repo set
(`compute_session_order`), and that stays authoritative: children
nest under their parent **within** a repo group (muted `└` prefix,
depth tracked in `SessionOrder::depths`), because a lead and its
workers usually share a repo. A child whose parent renders in a
different group keeps its natural position and gets a `↳` mark
instead — reordering across repo groups would break the "one header
per repo" invariant and make rows jump between groups. Group
bubbling is unchanged: an `Attention` child still pulls its whole
repo group to the top. Navigation (`Ctrl+J`/`Ctrl+K`) shares the
same ordering function, so it walks the tree exactly as rendered.
Parent cycles can't be produced by current writers (the parent must
exist before the child, and the link is immutable), but the ordering
is still defensive: cycle members render flat rather than vanish.

---

## Terminal Scrollback

### Scrollback buffer

The terminal uses vt100's built-in 1000-line scrollback buffer.
`Screen::scrollback()` returns the current offset (0 = at bottom),
and `Screen::set_scrollback(n)` moves the viewport. When the offset
is non-zero and new output arrives, vt100 auto-increments the
offset to keep the view pinned at the same history position. When
the offset is 0, new output naturally stays at the bottom.

### Scroll keybindings

`Shift+Up/Down` scrolls one line, `Shift+PageUp/PageDown` scrolls
half a page, and the mouse wheel scrolls three lines per tick.
Any other keypress while scrolled up snaps back to the bottom
before forwarding to the PTY. This matches the mental model of
"I'm reading history, and when I start typing I'm back in the
present."

**Why Shift, not Ctrl?**

Ctrl-prefixed keys are reserved for Thurbox global commands.
Shift+arrow and Shift+Page are the conventional scrollback
keybindings in most terminal emulators (GNOME Terminal, Kitty,
Alacritty) and do not conflict with the agent CLI or shell readline.

### Scrollbar widget

A ratatui `Scrollbar` overlays the right edge of the terminal
panel (inside the border). It only appears when there is scrollback
content. The thumb position is inverted from the offset (offset 0
= thumb at bottom, max offset = thumb at top) to match visual
expectations. When scrolled up, the block title shows a `[N↑]`
indicator and the PTY cursor is hidden to avoid visual noise in
historical output.

---

## Theme System

![Theme switcher](media/thurbox-theme.gif)

All UI colors are centralized in `src/ui/theme.rs` via a semantic
palette. Widget files reference named colors (accent, text, status,
border) rather than hard-coded `Color::*` values, so the whole UI
can be re-skinned by swapping the active palette.

Thurbox ships nine built-in presets — five dark (Default,
Catppuccin Mocha, Tokyo Night, Gruvbox Dark, Doom) and four light
(Catppuccin Latte, Tokyo Night Day, Gruvbox Light, Solarized
Light). Press `Ctrl+Y` (or `F4`, which avoids terminals that
intercept `Ctrl+Y` as DSUSP) to pick one. The choice is persisted
in SQLite under `metadata.active_theme` and survives restarts;
other Thurbox processes pick it up within one tick via
`PRAGMA data_version` polling.

### Why centralized?

- ~50 color references were scattered across 13+ widget files.
  Changing the accent color required editing every file.
- Semantic names (accent, status, border) make the intent clear at
  each call site.
- A single palette enables user-selectable themes without touching
  widget code.

### Color categories

| Category | Purpose |
|----------|---------|
| Accent | Focused borders, selected items, highlights |
| Status | Session status indicators (busy/waiting/idle/error) |
| Text | Three-level text hierarchy (primary/secondary/muted) |
| Borders | Panel border states (focused/unfocused) |
| Domain | Semantic colors for agent name, branch name |
| Hints | Keybinding and interactive hints |

---

## Focus Levels

Panels use a tri-state focus system (`Focused`, `Active`,
`Inactive`) for clear navigation feedback.

| Level | Border | Title | Meaning |
|-------|--------|-------|---------|
| `Focused` | Thick cyan | Bold cyan | Receiving input |
| `Active` | Plain cyan | Cyan text | Contextually relevant |
| `Inactive` | Plain gray | Gray text | Background |

---

## Status Messages

Status messages have a severity level and auto-dismiss after 5
seconds.

| Level | Badge | Text color | Use case |
|-------|-------|------------|----------|
| `Error` | Red `ERROR` | Red | Validation failures, operation errors |
| `Warning` | Yellow `WARN` | Yellow | Non-blocking issues |
| `Info` | Cyan `INFO` | Gray | Success feedback ("Session saved") |

Positive feedback is shown for: session start/restart/delete/
restore, worktree sync, and theme changes.

---

## Empty Terminal State

When the active session has no terminal content yet, the terminal
panel shows a centered hint box:

```text
┌───────────────────────────────┐
│ No active sessions            │
│                               │
│   Ctrl+N  New session         │
│   F1      Help                │
└───────────────────────────────┘
```

The session list is empty until the first session is created; the
active terminal can also briefly be empty during spawn.

---

## Info Panel Separators

Section boundaries in the info panel use styled `──────` separator
lines instead of blank lines, improving visual structure.

---

## Text Selection and Copy-Paste

Mouse drag selects text in the terminal panel. The selection is
confined to the active pane bounds.

- **Mouse drag**: Select text (anchor at press, cursor follows
  drag).
- **`Ctrl+C`** (with active selection): Copies selected text to
  the system clipboard via `arboard`. Trailing whitespace is
  trimmed per line.
- **`Ctrl+C`** (no selection): Forwarded to the terminal as SIGINT.
- **`Ctrl+V`**: Pastes from system clipboard into the active PTY.
- Any other keypress clears the selection.

Selection is highlighted in the terminal render buffer using
inverted colors. The clipboard handle is kept alive for the app
lifetime to avoid Linux-specific "dropped too quickly" issues.

---

## Shell Pane Toggle

`Ctrl+T` toggles between the agent session and a shell pane
(plain bash/zsh) for the active session. The shell runs in a
separate tmux pane alongside the agent pane.

- **Status bar**: Shows "Shell" label when viewing the shell pane.
- **Per-session state**: Each session tracks its own `TerminalView`
  (Agent or Shell) independently.
- Input is forwarded to whichever pane is currently active.

---

## Clickable URLs

URLs (`https://`, `http://`, `file://`) in terminal output are
detected via regex at click time and opened on `Ctrl+Click`.
Trailing punctuation (`.`, `,`, `;`, `:`, `)`, `]`) is stripped
from detected URLs. Character-based column offsets ensure correct
positioning with multibyte characters.

---

## Planned Features

Directional intent, not commitments. These may change as the
project evolves.

- **Multi-session orchestration**: Broadcast input to multiple
  agent sessions simultaneously.
- **Task delegation**: Split a task across multiple sessions with
  dependency tracking.
