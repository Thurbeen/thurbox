# Feature Decisions

Design rationale for user-facing behavior.
For architectural choices, see [ARCHITECTURE.md](ARCHITECTURE.md).

---

## Project Panel

### Two-section left panel design

The left sidebar is split vertically into two sections:
projects on top (40%), sessions for the selected project
on bottom (60%). This replaces the previous session-only sidebar.

**Why a two-section panel, not separate panels?**

- Reuses the existing 3-breakpoint layout (< 80, >= 80, >= 120)
  without adding a 4th breakpoint for a 4-panel mode.
- Works at 80 columns — a separate project panel would
  require ~160 cols minimum to show both project and session lists.
- Maintains visual hierarchy: projects contain sessions,
  and the vertical stacking reflects that containment.

**Why not a modal/popup?**

- Projects are persistent context, not transient selections.
  A modal would hide the project list while working,
  forcing the user to re-open it to switch.
- The always-visible panel shows session counts per project
  at a glance — useful for monitoring multi-project workflows.

### Project ↔ session binding

Each session is bound to the project that was active when
it was created. Sessions spawn in the project's repo directory.
If the project has a single repo, the session uses it directly.
If the project has multiple repos, a selector modal lets the
user choose which repo to use as the working directory.
If no repos are configured, the session falls back to `$HOME`.
When switching projects, only that project's sessions
are shown in the session list.

### Config file format

Projects are loaded from `~/.config/thurbox/config.toml`
(`$XDG_CONFIG_HOME` respected):

```toml
[[projects]]
name = "my-app"
repos = [
    "/home/user/repositories/app-frontend",
    "/home/user/repositories/app-backend",
]

[[projects]]
name = "infra"
repos = ["/home/user/repositories/infra"]
```

If the file doesn't exist, Thurbox creates an ephemeral
Default project using the current working directory.
The Default project is never persisted to disk.

---

## Keybinding Design

### Philosophy: Ctrl = global, everything else = PTY

When the terminal panel is focused,
**all keys are forwarded to the PTY** except those with a `Ctrl`
modifier (intercepted as global commands) and `Shift+arrow/page`
keys (intercepted for scrollback navigation).

**Why Ctrl, not Alt?**

- Claude Code and shell programs heavily use Alt-key combinations.
  Intercepting Alt would break readline, vim,
  and Claude's own keybindings.
- Ctrl has well-established precedent for "meta" actions
  in terminal multiplexers
  (tmux uses `Ctrl+B`, screen uses `Ctrl+A`).
- Ctrl combos are easier to type one-handed, which matters
  for a tool you use alongside other terminals.

### Keybinding Table

| Key | Context | Action |
|-----|---------|--------|
| `Ctrl+Q` | Global | Quit Thurbox |
| `Ctrl+N` | Project list | Add new project |
| `Ctrl+N` | Session list / Terminal | New session (mode selector, then optional branch selector) |
| `Ctrl+X` | Global | Close active session |
| `Ctrl+J` | Global | Next session within active project |
| `Ctrl+K` | Global | Previous session within active project |
| `Ctrl+L` | Global | Cycle focus: Project → Session → Terminal |
| `Ctrl+I` | Global | Toggle info panel (width >= 120) |
| `Ctrl+S` | Global | Sync active session with remote (worktree sessions only) |
| `j` / `Down` | Project list | Next project |
| `k` / `Up` | Project list | Previous project |
| `r` | Project list | Open role editor |
| `Enter` | Project list | Focus session list |
| `j` / `Down` | Session list | Next session |
| `k` / `Up` | Session list | Previous session |
| `Enter` | Session list | Focus terminal |
| `?` | Project/session list | Show help overlay |
| `j` / `Down` | Repo selector | Next repo |
| `k` / `Up` | Repo selector | Previous repo |
| `Enter` | Repo selector | Select repo and spawn session |
| `Esc` | Repo selector | Cancel selection |
| `j` / `Down` | Session mode modal | Next mode |
| `k` / `Up` | Session mode modal | Previous mode |
| `Enter` | Session mode modal | Select mode |
| `Esc` | Session mode modal | Cancel |
| `j` / `Down` | Base branch selector | Next branch |
| `k` / `Up` | Base branch selector | Previous branch |
| `Enter` | Base branch selector | Select base and open name prompt |
| `Esc` | Base branch selector | Cancel |
| `Enter` | New branch prompt | Confirm name, create branch and worktree |
| `Esc` | New branch prompt | Cancel |
| `Shift+Up` | Focused terminal | Scroll up 1 line |
| `Shift+Down` | Focused terminal | Scroll down 1 line |
| `Shift+PageUp` | Focused terminal | Scroll up half page |
| `Shift+PageDown` | Focused terminal | Scroll down half page |
| Mouse wheel | Focused terminal | Scroll up/down 3 lines |
| All other keys | Focused terminal | Forwarded to PTY (snaps to bottom if scrolled) |

---

## Session Lifecycle

```text
Create (UUID v4) → Running → Idle / Error
                      ↓
                  Shutdown (SIGHUP)
```

### States

- **Running**: PTY is alive, read loop is active,
  output is streaming to the terminal widget.
- **Idle**: Claude CLI has exited cleanly (exit code 0).
  Session is still displayed but no longer accepts input.
- **Error**: PTY or Claude CLI exited with a non-zero code.
  Error details shown in status bar.
- **Shutdown**: Triggered by the user closing a session or
  quitting the app. Sends `SIGHUP` to the PTY child process,
  then waits for clean exit before dropping resources.

### Why UUID v4?

Sessions need unique identifiers for the lifetime of the process.
UUIDs are collision-free without coordination, simple to generate,
and usable as map keys. Sequential IDs would work too, but UUIDs
prevent bugs where an old session ID accidentally refers to
a new session after recycling.

---

## Error Handling UX

### Rule: never crash, never modal

Errors are shown in the status bar footer as transient messages.
They do not block interaction, do not require dismissal,
and auto-clear after a timeout or on the next successful action.

**Why non-modal?**

- Modal error dialogs in a TUI are jarring — they steal focus
  from the terminal where the user is working.
- Most errors are recoverable (session failed to start,
  PTY read error). Showing them passively lets the user
  decide when to act.
- Fatal errors (can't initialize terminal) are the only case
  where the app exits, and those happen before the TUI
  is even rendered.

---

## Responsive Layout

### Breakpoint Rationale

| Width | Layout | Why |
|-------|--------|-----|
| `<80` | Terminal only | Sidebar would leave <60 cols — too narrow |
| `>=80` | Left panel + terminal | 20-col sidebar (projects + sessions) + 60-col terminal min |
| `>=120` | Left panel + terminal + info | Terminal still gets ~70+ cols |

The left panel contains both the project list and session list
as a vertically split two-section panel. This reuses the existing
breakpoints without requiring a 4th tier.

### Why not user-configurable?

Configurable breakpoints add UI, storage, and edge-case complexity
for minimal gain. The fixed values cover standard terminal sizes
(80, 120, 160+). If a user resizes their terminal, the layout
adapts instantly. Custom breakpoints can be added later
if real demand emerges.

---

## Git Worktree Integration

Sessions can optionally run inside git worktrees for branch
isolation. This is opt-in: after pressing `Ctrl+N`, a session
mode selector modal asks "Normal" or "Worktree".

### Flow

1. `Ctrl+N` triggers session creation.
2. If the project has 2+ repos, a repo selector appears first.
3. A session mode modal offers "Normal" (spawn in repo root)
   or "Worktree" (spawn in an isolated worktree).
4. Choosing "Worktree" opens a base branch selector listing
   local branches from the selected repo.
5. Selecting a base branch opens a prompt for the new branch
   name. The user types the name for the new branch to create.
6. Confirming the name creates a new git branch (from the
   selected base) in a worktree and spawns the session inside it.
7. For projects with 0 repos, sessions spawn in `$HOME`
   with no mode modal (worktrees require a git repo).

### Worktree storage

Worktrees are created at
`<repo>/.git/thurbox-worktrees/<sanitized-branch>`,
where `/` in branch names is replaced by `-`.

### Cleanup behavior

- Closing a worktree session (`Ctrl+X`) automatically removes
  the worktree via `git worktree remove --force`.
- Quitting Thurbox (`Ctrl+Q`) preserves worktrees on disk
  so they can be resumed on next launch
  (see [Session Persistence](#session-persistence)).
- Cleanup errors are logged but do not block session close
  or app shutdown.

### UI indicators

- **Terminal title**: Worktree sessions show the branch in
  the title bar: `my-session [feature/foo] [Running]`.
- **Session list**: Branch name appears next to worktree
  sessions with a green `[branch]` badge.
- **Info panel**: Shows a "Worktree" section with branch name
  and worktree path when viewing a worktree session.

### Keybindings (session mode modal)

| Key | Action |
|-----|--------|
| `j` / `Down` | Next option |
| `k` / `Up` | Previous option |
| `Enter` | Select mode |
| `Esc` | Cancel |

### Keybindings (base branch selector)

| Key | Action |
|-----|--------|
| `j` / `Down` | Next branch |
| `k` / `Up` | Previous branch |
| `Enter` | Select base branch and open name prompt |
| `Esc` | Cancel |

### Keybindings (new branch name prompt)

| Key | Action |
|-----|--------|
| `Enter` | Confirm name, create branch and worktree |
| `Esc` | Cancel |

---

## Session Persistence

Closing Thurbox with `Ctrl+Q` saves all active session metadata
so they can be restored on the next launch.

### How it works

- On every session spawn, Thurbox assigns a `claude_session_id`
  (UUID v4) via the Claude CLI's `--session-id` flag. This tells
  Claude to use a stable conversation ID from the start.
- On shutdown (`Ctrl+Q`), all session metadata is written to
  `$XDG_DATA_HOME/thurbox/state.toml`
  (default: `~/.local/share/thurbox/state.toml`).
- On next startup, Thurbox detects the state file and resumes
  each session using `--resume <session-id>`, restoring the
  exact Claude conversations.
- The state file is cleared after successful restore to prevent
  stale state from accumulating.

### State file format

```toml
session_counter = 3

[[sessions]]
name = "Session 1"
claude_session_id = "abc-123-def-456"
cwd = "/home/user/repos/app"

[[sessions]]
name = "Session 2"
claude_session_id = "ghi-789-jkl-012"
cwd = "/home/user/repos/app/.git/thurbox-worktrees/feat-login"

[sessions.worktree]
repo_path = "/home/user/repos/app"
worktree_path = "/home/user/repos/app/.git/thurbox-worktrees/feat-login"
branch = "feat-login"
```

### Worktree preservation

Worktrees are **not** removed on `Ctrl+Q` shutdown — they
persist on disk so the resumed session can continue working
in the same branch checkout. Worktree metadata (repo path,
worktree path, branch name) is saved in the state file and
reconstructed on restore.

### Explicit close vs quit

- **`Ctrl+Q` (Quit)**: Saves all sessions, preserves worktrees.
  Sessions resume on next launch.
- **`Ctrl+X` (Close)**: Permanently closes the active session.
  Its worktree (if any) is removed immediately.
  Closed sessions are not saved and will not be restored.

---

## Terminal Scrollback

### Scrollback buffer

The terminal uses vt100's built-in 1000-line scrollback buffer.
`Screen::scrollback()` returns the current offset (0 = at bottom),
and `Screen::set_scrollback(n)` moves the viewport. When the
offset is non-zero and new output arrives, vt100 auto-increments
the offset to keep the view pinned at the same history position.
When the offset is 0, new output naturally stays at the bottom.

### Scroll keybindings

`Shift+Up/Down` scrolls one line, `Shift+PageUp/PageDown` scrolls
half a page, and the mouse wheel scrolls three lines per tick.
Any other keypress while scrolled up snaps back to
the bottom before forwarding to the PTY. This matches the mental
model of "I'm reading history, and when I start typing I'm back
in the present."

**Why Shift, not Ctrl?**

Ctrl-prefixed keys are reserved for Thurbox global commands.
Shift+arrow and Shift+Page are the conventional scrollback
keybindings in most terminal emulators (GNOME Terminal, Kitty,
Alacritty) and do not conflict with Claude Code or shell readline.

### Scrollbar widget

A ratatui `Scrollbar` overlays the right edge of the terminal
panel (inside the border). It only appears when there is scrollback
content. The thumb position is inverted from the offset
(offset 0 = thumb at bottom, max offset = thumb at top) to match
visual expectations. When scrolled up, the block title shows a
`[N↑]` indicator and the PTY cursor is hidden to avoid visual
noise in historical output.

---

## Role Editor

Roles can be managed from the TUI via a two-view modal
accessed with `r` when the project list is focused.

Projects start with no roles. Users add roles explicitly.
When no roles are defined, sessions spawn with default
(empty) permissions and no role selector is shown.

### Allow / Ask / Deny Semantics

Each role maps to Claude CLI flags:

| Concept | CLI Flag | In TOML |
|---------|----------|---------|
| Allow (auto-approve) | `--allowed-tools "Read Bash(git:*)"` | `allowed_tools = ["Read", "Bash(git:*)"]` |
| Deny (blocked) | `--disallowed-tools "Edit"` | `disallowed_tools = ["Edit"]` |
| Ask (prompt user) | *(default for unlisted tools)* | *(omitted from both lists)* |
| Permission mode | `--permission-mode plan` | `permission_mode = "plan"` |

Bash scope patterns like `Bash(git:*)` and `Bash(cargo:*)`
are supported in both allowed and disallowed tool lists.

### Role List View

Shows all roles for the active project. Supports
add (`a`), edit (`e` / `Enter`), and delete (`d`).
Pressing `Esc` saves changes to the config file and
closes the modal.

### Role Editor View

Edits a single role with four text fields:

- **Name** — role identifier (required, unique)
- **Description** — human-readable summary
- **Allowed Tools** — space-separated tool names
  (auto-approved)
- **Disallowed Tools** — space-separated tool names
  (blocked)

Permission mode defaults to `dontAsk` and can be
overridden per-role via the config file
(`permission_mode = "plan"` in the role's TOML block).

`Tab` / `Shift+Tab` cycles between fields.
`Enter` saves the role, `Esc` discards changes.

### Keybindings (role list)

| Key | Action |
|-----|--------|
| `j` / `Down` | Next role |
| `k` / `Up` | Previous role |
| `a` | Add new role |
| `e` / `Enter` | Edit selected role |
| `d` | Delete selected role |
| `Esc` | Save and close |

### Keybindings (role editor)

| Key | Action |
|-----|--------|
| `Tab` / `Shift+Tab` | Cycle fields |
| `Enter` | Save role |
| `Esc` | Discard changes |

---

## Git Auto-Sync

Worktree-backed sessions automatically sync their branch state
with the remote, so users always see whether they are ahead,
behind, or diverged without manually running `git fetch`.

### How it works

1. Every ~30 seconds, Thurbox runs `git fetch origin` followed
   by `git rev-list --count` for each worktree session.
2. The result is displayed as a `SyncStatus` in the UI:
   - **Up to date** (`⟳`): Local matches remote.
   - **Behind N** (`⟲`): Remote has commits not yet pulled.
   - **Ahead N** (`⟶`): Local has unpushed commits.
   - **Diverged** (`⇄`): Both sides have unique commits.
   - **Syncing** (`⊙`): Fetch is in progress.
   - **Error** (`✗`): Fetch or status check failed.
3. Sync operations run in background `spawn_blocking` tasks
   to avoid blocking the TUI event loop.

### Manual sync

Press `Ctrl+S` (any focus) to immediately sync the active
session. This is useful after pushing or pulling outside
Thurbox. The status briefly shows "Syncing..." while
the fetch runs.

### UI indicators

- **Terminal title**: Shows sync icon after the branch name:
  `Session 1 [feature/foo ⟳] [Running]`.
- **Session list**: Sync icon appears after the branch badge.
- **Info panel**: Full sync status line with elapsed time
  since last sync.

### Keybindings

| Key | Context | Action |
|-----|---------|--------|
| `Ctrl+S` | Any focus | Sync active session now |

### Design decisions

**Why fetch-only, not auto-pull?**

Auto-pull modifies the working tree, which could disrupt
a running Claude session mid-task. Showing status is safe;
acting on it should be explicit.

**Why 30-second interval?**

Balances freshness against network traffic. Most remote
changes are not urgent — a 30-second delay is acceptable
for status-only information. Manual `Ctrl+S` covers
the "I just pushed" case.

**Why per-session, not global?**

Different sessions may use different repos or branches.
Per-session tracking avoids showing stale status
for unrelated worktrees.

---

## Planned Features

Directional intent, not commitments.
These may change as the project evolves.

- **Multi-session orchestration**: Run N Claude Code instances
  side-by-side, switch between them, broadcast input to all.
- **Task delegation**: Split a task across multiple sessions
  with dependency tracking.
