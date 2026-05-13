# Feature Decisions

Design rationale for user-facing behavior.
For architectural choices, see [ARCHITECTURE.md](ARCHITECTURE.md).

---

## Session Sidebar

### Single session list

The left sidebar holds a single flat list of sessions — there is no
project grouping layer above it. Sessions are top-level, identified
by a UUID v4, and labeled with their name, role, branch (when in a
worktree), and cwd.

**Why no projects?**

- An earlier design grouped sessions under projects (one project →
  many sessions, with shared repos and roles). In practice users
  tended to create one session per task, so the project layer was
  pure overhead: an extra navigation level, an extra creation step,
  and an extra deletion guard.
- Removing the project layer (storage migration v16 dropped
  `projects`, `project_repos`, `project_roles`, `project_mcp_servers`,
  `project_vm_config`, `project_container_config`) collapses the
  model to "sessions own their own configuration". Roles, MCP
  servers, and skills become global presets attached at session
  creation time, not project attributes.

**Why a sidebar at all instead of a popup?**

- Sessions are persistent context, not transient selections. An
  always-visible list shows status (Running, Idle, Error), elapsed
  time, and branch at a glance — useful for monitoring multiple
  parallel Claude instances.
- The sidebar fits cleanly into the existing 3-tier responsive
  layout (`<80`, `>=80`, `>=120`); a popup would require its own
  open/close keybinding and dismissal logic.

### Fuzzy search

Pressing `/` while the session list is focused opens an inline
fuzzy filter that matches against the session's name, role, branch
name, and cwd. `Enter` confirms, `Esc` cancels.

**Why all four fields?** Users remember sessions by whichever
attribute is most distinctive — sometimes the branch name, often
the role ("the reviewer one"), occasionally the repo path. Indexing
all four makes the search hit on the first attempt without forcing
the user to remember which field to type into.

### Settings overlay

`Ctrl+E` opens the settings overlay with four tabs: **Roles**,
**MCP Servers**, **Skills**, and **Plugins**. Use `Tab` to cycle
between tabs. The first three are global presets shared across
sessions and selected at session creation. The Plugins tab lists
effective plugins (disk-discovered + registered) with their
version, source, enabled flag, and path; press `Space` to toggle a
plugin on or off. Install/uninstall and per-plugin configuration
are managed through the MCP tools — see `docs/PLUGINS.md`.

---

## Session Creation

`Ctrl+N` walks through a series of modals to configure a new
session. Each step has a sensible default and can be skipped when
not applicable.

1. **Repo picker** — fuzzy-searchable list of bookmarked repo
   paths. `Space` toggles selection, `w` marks the selected repo
   as a worktree base, `d` deletes the bookmark, and a path-input
   field with filesystem autocomplete adds new bookmarks. The
   first selected repo becomes the session's `cwd`; the rest are
   passed to Claude via `--add-dir` so it can read across all of
   them.
2. **Session mode** — Normal / Worktree / Container / VM. Skipped
   when at least one repo is marked as worktree (the choice is
   already implied).
3. **Base branch selector** — worktree mode only.
4. **Session name** — free text identifier shown in the sidebar.
5. **New branch name** — worktree mode only.
6. **Role selector** — only when 2+ global roles are defined; the
   single-role case auto-selects.
7. **MCP server picker** — pick which global MCP servers to attach
   to this session.
8. **Skill picker** — pick which global skills to attach.

**Why per-session repo selection?** Each session is its own context,
so it makes sense to pick repos at creation time rather than
inheriting from a parent grouping. Mixed sessions are supported:
some repos may be worktree-based (new branch created) while others
are added as-is.

**Why a bookmark list rather than a path picker every time?** Users
work on the same handful of repos repeatedly. Bookmarks make the
common case a 2-keystroke selection while still allowing arbitrary
paths via the input field. Bookmark deletion (`d`) keeps the list
from accumulating stale entries.

### Profiles (preset bundles)

A **profile** is a named bundle of role names, MCP server names,
and skill names that get applied together at session spawn. It
exists so common session shapes (e.g. "orchestrator sessions
always use the `developer` role plus the `orchestrate` skill")
can be one-step to reproduce instead of re-ticking three separate
pickers.

Profiles are exposed through the MCP tools (`list_profiles`,
`get_profile`, `register_profile`, `unregister_profile`,
`set_profiles`) and through the `thurbox-cli session create
--profile <name>` flag. `create_session` over MCP accepts a
`"profile"` field on the same precedence rules.

**Multi-role merging.** A profile may list multiple roles. Their
`RolePermissions` are merged at spawn time: `allowed_tools` and
`disallowed_tools` are unioned, `append_system_prompt` is
concatenated in role order, `env` maps are merged with later-wins
precedence, and `permission_mode` is chosen as the most permissive
(ranked `plan` < `default` < `acceptEdits` < `bypassPermissions`).
Unknown mode strings rank lowest so they can't silently outrank a
known mode.

**Caller precedence.** An explicit `role`, `mcp_servers`, or
`skills` argument on the spawn call overrides the profile's
contribution for that single field. The displayed session role
becomes `profile:<name>` when a profile is applied, so the TUI
session list can distinguish preset-driven sessions.

**Seeded default.** On first startup Thurbox seeds one profile,
`orchestrator` (roles=`[developer]`, skills=`[orchestrate]`).
Deleting it is persistent — a `profiles_seeded` metadata flag
prevents re-seeding on subsequent startups.

**TUI.** When at least one profile is registered, the Ctrl+N
session creation chain inserts a profile picker right after the
session-name prompt. Row 0 is a synthetic "(No profile)" that
falls through to the normal role/MCP/skill chain; picking a
registered profile applies its roles (merged), MCP servers, and
skills, then jumps directly to the model picker. Users without
profiles see no UI change.

---

## Keybinding Design

### Philosophy: Ctrl = global, everything else = PTY

When the terminal panel is focused, **all keys are forwarded to the
PTY** except those with a `Ctrl` modifier (intercepted as global
commands) and `Shift+arrow/page` keys (intercepted for scrollback).

**Why Ctrl, not Alt?**

- Claude Code and shell programs heavily use Alt-key combinations.
  Intercepting Alt would break readline, vim, and Claude's own
  keybindings.
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
| `Ctrl+P` | Global | Schedule command for active session | **P**rogram |
| `Ctrl+T` | Global | Toggle shell pane alongside Claude session | **T**erminal |
| `Ctrl+H` | Global | Focus previous pane (cycle backward) | Vim: **h** = left |
| `Ctrl+J` | Global | Select next session | Vim: **j** = down |
| `Ctrl+K` | Global | Select previous session | Vim: **k** = up |
| `Ctrl+L` | Global | Focus next pane (cycle forward) | Vim: **l** = right |
| `Ctrl+D` | Session list | Delete selected session | Vim: **d** = delete |
| `Ctrl+E` | Global | Edit settings (roles, MCP servers, skills) | **E**dit |
| `Ctrl+O` | Global | Open active session's worktrees in editor | **O**pen |
| `Ctrl+R` | Global | Restart active session | **R**estart |
| `Ctrl+F` | Global | Fork active session | **F**ork |
| `Ctrl+S` | Global | Sync all worktree sessions with origin/main | **S**ync |
| `Ctrl+Z` | Global | Undo session delete | **Z** = undo |
| `Ctrl+U` | Global | Restore deleted sessions list | **U**ndelete |
| `F1` | Global | Toggle keybindings help | Universal help |
| `F2` | Global | Toggle info panel | Next to F1 |
| `Ctrl+Y` / `F4` | Global | Pick TUI theme | Color **Y**oke |
| `j` / `Down` | Lists | Next item | |
| `k` / `Up` | Lists | Previous item | |
| `/` | Session list | Open fuzzy search (name, role, branch, cwd) | Vim search |
| `Enter` | Search bar | Confirm and close search | |
| `Esc` | Search bar | Cancel search | |
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
- **Idle**: Claude CLI has exited cleanly (exit code 0). Session
  is still displayed but no longer accepts input.
- **Error**: PTY or Claude CLI exited with a non-zero code. Error
  details shown in status bar.
- **Shutdown**: Triggered by the user closing a session or quitting
  the app. Sends `SIGHUP` to the PTY child process, then waits for
  clean exit before dropping resources.

### Session Restart (`Ctrl+R`)

Restarts the active session's tmux pane while preserving the
conversation history. The session is killed and respawned with
`--resume` plus freshly-resolved role permissions from the
session's stored role.

**Why restart instead of close + new?**

- Closing destroys the Claude session ID. Restarting uses
  `--resume` so the conversation context is preserved.
- When a user edits role permissions via `Ctrl+E`, existing
  sessions keep running with stale permissions. `Ctrl+R` picks up
  the new permissions without losing context.
- The session's `SessionInfo` (ID, name, role, repos) stays intact
  — only the backend pane and I/O are replaced.

### Session context in system prompt

At session creation, Thurbox injects a small context block into
the Claude system prompt describing the session's name, role, and
working repos. This gives Claude immediate awareness of where it
is running without the user having to restate it.

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
stored in SQLite (`get_editor_command` / `set_editor_command` MCP
tools), defaulting to a sensible value on first run.

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

## Scheduled Commands

`Ctrl+P` opens a modal to schedule text that will be sent to the
active session after a configurable delay. This is useful for
queuing follow-up prompts, running maintenance commands, or pacing
multi-step workflows without manual intervention.

### Ctrl+P modal

The modal has two fields:

- **Command** — the text to send to the session's PTY.
- **Delay (minutes)** — a positive integer specifying how many
  minutes to wait before sending.

`Tab` switches between fields. `Enter` submits the scheduled
command, `Esc` cancels. The delay field only accepts digits.

### Dual-track execution

Scheduled commands use two independent execution paths for
reliability:

1. **Tmux external timer** — `tmux run-shell -b -d <seconds>`
   fires a shell script after the delay. The script checks the
   database for a cancellation flag before sending the command via
   `tmux send-keys`. This path is independent of the Thurbox
   process and survives crashes or restarts.
2. **App tick-loop safety net** — the TUI's tick loop polls the
   `scheduled_commands` table once per second for due commands.
   When found, it sends the command text via bracketed paste mode
   with a deferred Enter keystroke (~100 ms later).

Both paths mark the `executed_at` timestamp on completion.
Whichever fires first prevents the other from executing a second
time.

**Why dual-track?** The tmux timer guarantees execution even if
Thurbox crashes. The app tick loop guarantees execution even if
the tmux timer encounters an edge case. Together they provide a
reliability guarantee: once scheduled, the command will execute.

### Persistence

Scheduled commands are stored in the `scheduled_commands` SQLite
table with fields for `session_id`, `command_text`, `scheduled_at`,
`created_at`, `executed_at`, and `cancelled_at`. A partial index
on `scheduled_at` (where pending) optimizes due-command queries.

### Cancellation

The `cancelled_at` timestamp prevents execution. When set:

- The tmux timer's shell script checks the flag and skips sending.
- The app tick loop's query excludes cancelled commands.

Cancellation is atomic — it only succeeds if the command has not
already been executed or cancelled.

### MCP access

Four MCP tools provide programmatic access:

| Tool | Description |
|------|-------------|
| `schedule_command` | Schedule text to be sent to a session at a future time |
| `list_scheduled_commands` | List pending commands, optionally filtered by session |
| `get_scheduled_command` | Get a scheduled command by ID |
| `cancel_scheduled_command` | Cancel a pending scheduled command |

---

## Orchestrator Mode

The Admin session can act as a coordinator that spawns other
sessions, dispatches prompts to them, and reads back their
output — turning the built-in MCP client into a multi-agent
orchestrator.

### The three orchestrator primitives

| Tool | Purpose |
|------|---------|
| `create_session` | Spawn a new local-tmux session (optionally on a fresh git worktree) |
| `send_prompt` | Send text to a session's terminal immediately, followed by Enter |
| `capture_session_output` | Read the rendered contents of a session's pane |

All three are exposed through `thurbox-mcp` and pre-allowed
for the Admin session via `ADMIN_MCP_TOOLS`.

### Typical loop

1. `create_session(name, repo_path, role?, worktree_branch?,
   mcp_servers?, skills?)` — returns a UUID immediately; the
   TUI picks up the queued spawn on its next tick and boots
   the session.
2. Poll `get_session(id)` until the session exists and
   `status` transitions to `Idle`/`Waiting` (meaning Claude
   has finished its initial boot).
3. `send_prompt(id, "your task here")` — text is typed into
   the session's tmux pane; Enter is pressed after a short
   delay so the app has time to process the typed input.
4. Poll `get_session(id)` again; once the status returns to
   `Idle` the agent has finished responding.
5. `capture_session_output(id, lines?)` — returns the pane's
   rendered text. Default 200 lines of scrollback before the
   visible region; capped at 10 000.
6. React: call `send_prompt` again, `delete_session`, or
   spawn more workers.

### How spawning works

`create_session` writes a single row to the `session_commands`
table with a `spawn:<json>` payload and a pre-generated session
UUID. The TUI's existing DB-polled command queue (`process_session_commands`,
~10 ms cadence) picks it up, resolves role/MCP servers/skills
by name against the global config, optionally runs
`git::create_worktree` off the requested base branch, and then
calls `do_spawn_session` — the same code path as `Ctrl+N`.

Because the UUID is generated up front, the caller can start
polling immediately without waiting for the spawn to land.

### Scope

Orchestrator spawns are **local-tmux only**. VM and container
backends are out of scope — if you need a sandboxed orchestrator
worker, provision the VM/container through the TUI and drive
it with `send_prompt`/`capture_session_output`.

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
isolation. This is opt-in via the session mode selector ("Normal",
"Worktree", "Container", or "VM") or by marking a repo with `w`
in the repo picker.

### Flow

1. `Ctrl+N` triggers session creation and opens the repo picker.
2. Marking a repo with `w` in the picker, or choosing "Worktree"
   in the mode modal, routes through the worktree branch flow.
3. A base branch selector lists local branches from the selected
   repo.
4. Selecting a base branch opens a prompt for the new branch name.
5. Confirming creates a new git branch (from the selected base) in
   a worktree and spawns the session inside it.
6. Mixed sessions are supported: worktree-marked repos get a new
   branch while normal repos are added as-is via `--add-dir`.

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

**Why stash instead of requiring a clean tree?** Claude sessions
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
  details are sent to the session's Claude instance as a prompt
  asking it to resolve the rebase.
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
Tables include `sessions`, `worktrees`, `vms`, `containers`,
`scheduled_commands`, `roles`, `mcp_servers`, `skills`, and
`metadata`. The database uses WAL mode for concurrent multi-instance
access.

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
Alacritty) and do not conflict with Claude Code or shell readline.

### Scrollbar widget

A ratatui `Scrollbar` overlays the right edge of the terminal
panel (inside the border). It only appears when there is scrollback
content. The thumb position is inverted from the offset (offset 0
= thumb at bottom, max offset = thumb at top) to match visual
expectations. When scrolled up, the block title shows a `[N↑]`
indicator and the PTY cursor is hidden to avoid visual noise in
historical output.

---

## Role System

Roles are **global** presets shared across all sessions. They are
managed via the settings overlay (`Ctrl+E` → Roles tab) or via the
MCP server.

A built-in "developer" role (`permission_mode: acceptEdits`, no
tool restrictions) is seeded as the default when no roles are
configured. When creating a session, a role selector appears if
2+ global roles are defined; otherwise the single role is used
automatically.

For programmatic role management via the MCP server, see
[MCP_ROLES.md](MCP_ROLES.md).

### Allow / Ask / Deny Semantics

Each role maps to Claude CLI flags:

| Concept | CLI Flag |
|---------|----------|
| Allow (auto-approve) | `--allowed-tools "Read Bash(git:*)"` |
| Deny (blocked) | `--disallowed-tools "Edit"` |
| Ask (prompt user) | *(default for unlisted tools)* |
| Permission mode | `--permission-mode plan` |

Bash scope patterns like `Bash(git:*)` and `Bash(cargo:*)` are
supported in both allowed and disallowed tool lists.

### Role List View

Shows all global roles. Supports add (`a`), edit (`e` / `Enter`),
and delete (`d`). Pressing `Esc` saves changes to the database and
closes the modal.

### Role Editor View

Edits a single role with five text fields:

- **Name** — role identifier (required, unique)
- **Description** — human-readable summary
- **Allowed Tools** — space-separated tool names (auto-approved)
- **Disallowed Tools** — space-separated tool names (blocked)
- **Environment Variables** — key=value pairs injected into
  sessions using this role

Permission mode defaults to `default` and can be overridden
per-role via the role editor.

`Tab` / `Shift+Tab` cycles between fields. `Enter` saves the role,
`Esc` discards changes.

---

## Skill Management

Claude Code skills come from **two sources**:

1. **Disk-source (predefined)** — any directory under
   `~/.local/share/thurbox/admin/skills/` that contains a
   `SKILL.md` is auto-discovered. Dropping a directory in is all
   it takes; no SQLite registration is needed. This mirrors how
   Containerfile templates work under `admin/containerfiles/`.
2. **Registered** — SQLite rows in the `skills` table that point
   at arbitrary absolute paths. Managed via the settings overlay
   (Ctrl+E → Skills tab), `thurbox-cli skill`, and the MCP
   `list_skills` / `set_skills` / `register_skill` /
   `unregister_skill` tools.

**Collision rule**: a registered entry with the same name as a
disk-source skill **shadows the disk-source entry**. Rationale:
disk-source skills ship as admin-managed defaults; registering
the same name is the documented way to override their path.
`thurbox-cli skill list` and the MCP `list_skills` tool both
return a `source` field (`"disk"` / `"registered"`) so operators
can tell which entries are predefined vs. user-configured. The
settings overlay shows only registered entries — disk-source
skills never appear as editable rows to avoid confusion about
"deleting" a directory the user can simply drop in again.

Both sources are presented in the skill picker at session spawn
time; the user selects skills by name, and Thurbox resolves each
name against the merged view before symlinking.

### Staging outside the worktree

Selected skills are staged into a per-session directory **outside
the session's working tree** and exposed to Claude Code via the
`CLAUDE_CONFIG_DIR` environment variable. Claude Code reads skills
from `$CLAUDE_CONFIG_DIR/skills/` automatically.

**Why outside the worktree?**

- Staging skills inside the repo would mean every session creates
  uncommitted (or worse, accidentally-committed) files in the
  user's working tree. Routing through `CLAUDE_CONFIG_DIR` keeps
  the worktree pristine.
- It also makes skills truly per-session rather than per-repo —
  two sessions on the same repo can run with different skill sets
  without colliding.

---

## Admin Session

A built-in Admin session provides conversational access to Thurbox
management via Claude Code with the `thurbox-mcp` MCP server
auto-configured. It is pinned at index 0 of the session list and
visually distinguished with a yellow `⚙` prefix.

### How it works

On startup, Thurbox creates:

1. An admin directory at `~/.local/share/thurbox/admin/` (or
   `thurbox-dev/admin/` for dev builds).
2. A `.mcp.json` file in that directory pointing to the
   `thurbox-mcp` binary. Claude Code auto-discovers this file.
3. An "Admin" session pinned at index 0 of the session list, with
   `cwd` set to the admin directory, all `thurbox-mcp` tools
   pre-allowed (auto-approved without user prompts), and a system
   prompt describing its management role.

The `.mcp.json` is rewritten on every startup to pick up binary
path changes after upgrades.

### Admin session restrictions

- Cannot be deleted (`Ctrl+D` shows an error message).
- Always present at index 0 — guarantees the session list is never
  empty, removing the need for a separate empty-state placeholder
  during startup.

**Why a pinned built-in instead of an auto-created normal session?**
A regular session could be deleted, leaving the user without the
conversational management entry point. Pinning the Admin session
makes the management interface a permanent first-class affordance
without requiring user setup.

### Binary resolution

The `thurbox-mcp` binary path is resolved by:

1. Checking for a sibling of `current_exe()` (works for both
   installed `~/.local/bin/` and dev `target/debug/` builds).
2. Falling back to bare `"thurbox-mcp"` for `$PATH` lookup.

---

## Theme System

All UI colors are centralized in `src/ui/theme.rs` via semantic
constants on a `Theme` struct. Widget files reference
`Theme::ACCENT`, `Theme::TEXT_PRIMARY`, etc. instead of hard-coded
`Color::*` values.

### Why centralized?

- ~50 color references were scattered across 13+ widget files.
  Changing the accent color required editing every file.
- Semantic names (`ACCENT`, `STATUS_BUSY`, `BORDER_FOCUSED`) make
  the intent clear at each call site.
- A single file enables future theming support (dark/light/custom)
  without touching widget code.

### Color categories

| Category | Constants | Purpose |
|----------|-----------|---------|
| Accent | `ACCENT` | Focused borders, selected items, highlights |
| Status | `STATUS_BUSY/WAITING/IDLE/ERROR` | Session status indicators |
| Text | `TEXT_PRIMARY/SECONDARY/MUTED` | Three-level text hierarchy |
| Borders | `BORDER_FOCUSED/UNFOCUSED` | Panel border states |
| Domain | `ROLE_NAME/ADMIN_BADGE/BRANCH_NAME` | Semantic domain colors |
| Hints | `KEYBIND_HINT/TOOL_ALLOWED/TOOL_DISALLOWED` | Interactive hints |

Composite style methods (`focused_title()`, `keybind()`, `cursor()`,
etc.) combine colors with modifiers for common patterns.

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
restore, role save, skill save.

---

## Unsaved Changes Guard

When pressing `Esc` in the role or MCP editor with modified fields,
a confirmation overlay asks "Discard changes? y/n". This prevents
accidental loss of edits.

Dirty detection uses snapshot comparison: field values are captured
when the editor opens and compared on `Esc`. If unchanged, the
editor closes immediately without prompting.

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

The Admin session guarantees the session list itself is never
empty, but the active terminal can briefly be empty during spawn.

---

## Info Panel Separators

Section boundaries in the info panel use styled `──────` separator
lines instead of blank lines, improving visual structure.

---

## Container Sessions

Sessions can run inside Docker or Podman containers for lightweight
OS-level isolation. This is opt-in: the session mode selector
modal offers "Container" alongside "Normal", "Worktree", and "VM".

### Container runtime

Thurbox auto-detects Docker or Podman, preferring Podman. The
runtime is detected once at startup via `detect_runtime()`.

### Containerfile templates

User-editable templates live in
`~/.local/share/thurbox/admin/containerfiles/`. Each template is a
folder containing a `Containerfile` and any support files (e.g.,
`init-firewall.sh`). The entire folder is used as the build
context.

```text
~/.local/share/thurbox/admin/containerfiles/
  default/
    Containerfile
    init-firewall.sh
  python/
    Containerfile
    requirements.txt
```

A `default/` template (based on `debian:bookworm-slim`) is seeded
on first run. Users can add more folders for different environments
and select them via a picker in the TUI.

### Container defaults

| Parameter | Value |
|-----------|-------|
| CPUs | 2 |
| Memory | 2048 MB |
| Firewall | Enabled (nftables/iptables allowlist) |
| Containerfile | `default/` template |

### Firewall allowlist

When `firewall_enabled` is true, the container runs
`init-firewall.sh` which restricts egress to a configurable
allowlist of domains and CIDRs. The default allowlist includes
`api.anthropic.com`, `github.com`, `crates.io`, and other common
development endpoints.

### Container lifecycle

```text
Building → Starting → Ready → Stopping → Stopped
                        ↓
                      Failed
```

### Session restoration

Containers survive Thurbox restarts. On restart, Thurbox discovers
containers from the database, verifies they are still running, and
re-adopts their tmux sessions.

### Container state persistence

Container records are stored in the `containers` SQLite table with
a foreign key to `sessions(id)`.

### TUI template picker

When creating a container session, a Containerfile picker modal
lists all available templates. `j`/`k` navigate, `Enter` selects,
`Esc` cancels. If only one template exists, the picker is skipped
and the default template is used automatically.

### Default template contents

The seeded `default/` Containerfile builds on `debian:bookworm-slim`
and installs: `curl`, `ca-certificates`, `git`, `tmux`, `iptables`,
`ipset`, `jq`, `rsync`. It creates a dedicated `thurbox` user
(UID/GID 5000), installs Claude Code via the native installer,
copies the firewall script and allowlist into the image, and
configures sudoers for firewall script execution.

### Template management via MCP

Four MCP tools provide programmatic access to templates:

| Tool | Description |
|------|-------------|
| `list_containerfile_templates` | List template names and the files each contains |
| `get_containerfile_template` | Read a template's Containerfile content and list support files |
| `set_containerfile_template` | Create or update a template (Containerfile + optional support files) |
| `delete_containerfile_template` | Delete a template (refuses to delete "default") |

### Template name safety

Template names and support file names are validated to prevent
path traversal: they must be non-empty, at most 64 characters, and
cannot contain `/`, `\`, `..`, or start with `.`. The `default`
template is protected from deletion.

---

## VM Sessions

Sessions can run inside QEMU/KVM virtual machines for full OS-level
isolation. This is opt-in: the session mode selector modal offers
"VM" alongside "Normal", "Worktree", and "Container".

### VM specifications

| Parameter | Value |
|-----------|-------|
| Base image | Debian 13 (Trixie) genericcloud amd64 qcow2 |
| CPUs | 2 |
| RAM | 2048 MB |
| Disk | 10 GB qcow2 CoW overlay |
| Networking | User-mode (SLIRP), SSH on ports 22200+ |
| SSH user | `thurbox` (ed25519 ephemeral key) |
| Packages | tmux, git, rsync, curl, Claude CLI |
| Boot timeout | 120 seconds |

Base images are cached at `~/.local/share/thurbox/images/`. Per-VM
state (disk overlay, cloud-init ISO, SSH keys, PID file) lives
under `~/.local/share/thurbox/vms/<vm-uuid>/`.

### Host requirements

- `qemu-system-x86_64` with `/dev/kvm` support
- `genisoimage` or `mkisofs` (cloud-init ISO creation)
- `ssh-keygen`, `rsync`

### How it works

1. `Ctrl+N` triggers session creation.
2. Choosing "VM" in the session mode modal starts asynchronous
   provisioning:
   - Downloads the Debian 13 base image (once, cached)
   - Creates a qcow2 CoW overlay disk
   - Generates cloud-init ISO (SSH key, user setup, packages)
   - Launches QEMU with KVM acceleration (`-enable-kvm -cpu host`)
   - Polls SSH readiness every 500ms (up to 120s timeout)
3. Once the VM is ready, a tmux session is spawned inside the VM
   over SSH, and the Claude Code CLI starts with `--resume`.

### VM lifecycle

VMs are managed by `VmManager` with a state machine:

```text
Creating → Starting → Ready → Stopping → Stopped
                        ↓
                      Error
```

### Session restoration

QEMU VMs survive Thurbox restarts (they are separate processes).
On restart, Thurbox:

1. Discovers sessions from all registered backends (local-tmux,
   devcontainer, qemu-vm).
2. For VM sessions, calls `VmManager::restore_vm()` to verify the
   QEMU process is still running via SSH probe.
3. Re-establishes the SSH control mode connection.
4. Adopts the tmux pane inside the VM with terminal content intact.
5. If the VM has died, falls through to re-provision a new VM with
   `--resume` to preserve conversation history.

### VM state persistence

VM records are stored in the `vms` SQLite table with a foreign key
to `sessions(id)`.

### Per-VM disk layout

Each VM's state lives in `~/.local/share/thurbox/vms/<vm-uuid>/`:

```text
<vm-uuid>/
  disk.qcow2       # CoW overlay (base image unchanged)
  cloud-init.iso   # nocloud format ISO
  ssh_key          # ephemeral ed25519 private key
  ssh_key.pub      # ephemeral ed25519 public key
  qemu.pid         # QEMU process PID file
```

The SSH ControlMaster socket is stored at
`/tmp/thurbox-ssh-<short-id>` (first 8 characters of the VM UUID)
to stay within the 108-byte Unix socket path limit.

### CoW overlay

Each VM gets a QCOW2 copy-on-write overlay that references the
shared base image as a backing file. The base image on disk is
never modified — all writes go to the per-VM overlay, which grows
on demand. This means creating a new VM is nearly instant (no
multi-gigabyte copy) and base image updates affect only new VMs.

### Cloud-init provisioning

VMs are provisioned via cloud-init in nocloud format:

- **User account**: `thurbox` with passwordless sudo and
  `/bin/bash` shell. SSH public key injected from the ephemeral
  keypair.
- **Packages**: `tmux`, `rsync`, `git`, `curl` installed via
  `package_update` + `packages`.
- **Claude CLI**: Installed via
  `curl -fsSL https://claude.ai/install.sh | bash` and symlinked
  to `/usr/local/bin/claude`.
- **Setup script**: An optional custom shell script from
  `VmConfig.setup_script` runs after package installation.

Thurbox waits for `cloud-init status --wait` to complete before
marking the VM as ready. If cloud-init reports errors (e.g., a
runcmd failure), provisioning continues as long as core packages
are installed.

### SSH port allocation

VM SSH ports start at 22200 and increment. On allocation, Thurbox
probes candidate ports to skip those already bound by orphaned
QEMU processes, scanning up to 100 ports. Host port forwarding is
configured via QEMU user-mode networking:
`-netdev user,id=net0,hostfwd=tcp::{port}-:22`.

### VM image management via MCP

Three MCP tools manage the base image cache:

| Tool | Description |
|------|-------------|
| `list_vm_images` | List downloaded images with file sizes |
| `download_vm_image` | Download an image from an HTTPS URL (rejects http/file) |
| `delete_vm_image` | Delete a cached image |

Image filenames are validated with the same path-traversal
protection used for container templates.

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

`Ctrl+T` toggles between the Claude Code session and a shell pane
(plain bash/zsh) for the active session. The shell runs in a
separate tmux pane alongside the Claude pane.

- **Status bar**: Shows "Shell" label when viewing the shell pane.
- **Per-session state**: Each session tracks its own `TerminalView`
  (Claude or Shell) independently.
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
  Claude Code instances simultaneously.
- **Task delegation**: Split a task across multiple sessions with
  dependency tracking.
