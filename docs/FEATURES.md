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
If the project has multiple repos, all repos are used
simultaneously: the first repo becomes the working directory
and the rest are passed via `--add-dir` so the Claude instance
has access to all project directories. No repo selector
is shown for multi-repo projects.
If no repos are configured, the session falls back to `$HOME`.
When switching projects, only that project's sessions
are shown in the session list.

### Project storage

Projects (name, repos, roles) are stored in the SQLite database
at `~/.local/share/thurbox/thurbox.db` (`$XDG_DATA_HOME` respected).
Projects are created and edited via the TUI (add-project modal
with `Ctrl+N`, edit with `Ctrl+E`).

If the database is empty on first launch, Thurbox creates an
ephemeral Default project using the current working directory.
The Default project is not persisted to the database.

On upgrade from a version that used `config.toml`, roles are
automatically migrated to the database on first launch. The
old `config.toml` is renamed to `config.toml.bak`.

### Edit project modal

`Ctrl+E` opens a pre-populated modal for editing the active
project's name, repositories, and roles. The modal mirrors the
add-project flow (Name → Path → RepoList) with an inline Roles
list that supports j/k navigation, add/edit/delete operations.

**Why not just delete and recreate?**

- Deleting a project kills all its sessions. Editing preserves
  them because the `ProjectId` stays stable across renames — it
  is not regenerated from the new name.
- The stable ID is persisted in the SQLite database so it
  survives application restarts. The `ProjectId` assigned at
  creation time never changes, even when the project is renamed.
- Users can fix typos or add repos without losing active work.

**Why a separate modal from add-project?**

- The edit modal needs pre-populated fields and a Roles section.
  Overloading the add modal with "mode" logic would complicate
  both the state machine and the key handlers.
- Separate modals keep each flow simple and independently testable.

#### Roles field behavior

- The Roles field shows an inline list of configured roles with
  j/k navigation, `a` to add, `e`/`Enter` to edit, `d` to delete.
- Editing or adding a role opens the role editor detail form as
  an overlay. `Esc` from the role editor returns to the Roles
  field in the edit-project modal.
- `Esc` from the Roles field saves all changes (name, repos,
  roles) and closes the modal.

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

All global keybindings use `Ctrl` and follow Vim conventions
where applicable: `h/j/k/l` for navigation, semantic letters
for actions (`C`=close, `D`=delete, `N`=new, `R`=restart, `Q`=quit).

| Key | Context | Action | Mnemonic |
|-----|---------|--------|----------|
| `Ctrl+Q` | Global | Quit Thurbox | **Q**uit |
| `Ctrl+N` | Project list | Add new project | **N**ew |
| `Ctrl+N` | Session list / Terminal | New session (mode selector, then optional branch selector) | **N**ew |
| `Ctrl+C` | Global | Close active session | **C**lose |
| `Ctrl+H` | Global | Focus project list | Vim: **h** = left |
| `Ctrl+J` | Global | Next session within active project | Vim: **j** = down |
| `Ctrl+K` | Global | Previous session within active project | Vim: **k** = up |
| `Ctrl+L` | Global | Cycle focus: Project → Session → Terminal | Vim: **l** = right |
| `Ctrl+D` | Session list | Close active session | Vim: **d** = delete |
| `Ctrl+D` | Project list | Delete selected project | Vim: **d** = delete |
| `Ctrl+E` | Global | Edit active project (name, repos, roles) | **E**dit |
| `Ctrl+R` | Global | Restart active session | **R**estart |
| `F1` | Global | Show help overlay | Universal help |
| `F2` | Global | Toggle info panel | Next to F1 |
| `j` / `Down` | Project list | Next project | |
| `k` / `Up` | Project list | Previous project | |
| `Enter` | Project list | Focus session list | |
| `j` / `Down` | Session list | Next session | |
| `k` / `Up` | Session list | Previous session | |
| `Enter` | Session list | Focus terminal | |
| `j` / `Down` | Repo selector | Next repo | |
| `k` / `Up` | Repo selector | Previous repo | |
| `Enter` | Repo selector | Select repo and spawn session | |
| `Esc` | Repo selector | Cancel selection | |
| `j` / `Down` | Session mode modal | Next mode | |
| `k` / `Up` | Session mode modal | Previous mode | |
| `Enter` | Session mode modal | Select mode | |
| `Esc` | Session mode modal | Cancel | |
| `j` / `Down` | Base branch selector | Next branch | |
| `k` / `Up` | Base branch selector | Previous branch | |
| `Enter` | Base branch selector | Select base and open name prompt | |
| `Esc` | Base branch selector | Cancel | |
| `Enter` | New branch prompt | Confirm name, create branch and worktree | |
| `Esc` | New branch prompt | Cancel | |
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

- **Running**: PTY is alive, read loop is active,
  output is streaming to the terminal widget.
- **Idle**: Claude CLI has exited cleanly (exit code 0).
  Session is still displayed but no longer accepts input.
- **Error**: PTY or Claude CLI exited with a non-zero code.
  Error details shown in status bar.
- **Shutdown**: Triggered by the user closing a session or
  quitting the app. Sends `SIGHUP` to the PTY child process,
  then waits for clean exit before dropping resources.

### Session Restart (`Ctrl+R`)

Restarts the active session's tmux pane while preserving the
conversation history. The session is killed and respawned with
`--resume` plus freshly-resolved role permissions from the
active project's current configuration.

**Why restart instead of close + new?**

- Closing destroys the Claude session ID. Restarting uses
  `--resume` so the conversation context is preserved.
- When a user edits role permissions via `Ctrl+E`, existing
  sessions keep running with stale permissions. `Ctrl+R`
  picks up the new permissions without losing context.
- The session's `SessionInfo` (ID, name, project association)
  stays intact — only the backend pane and I/O are replaced.

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
2. If the project has 2+ repos, a session spawns immediately
   using all repos (first as cwd, rest via `--add-dir`).
   No repo selector or session mode modal is shown.
3. If the project has 1 repo, a session mode modal offers
   "Normal" (spawn in repo root) or "Worktree" (spawn in
   an isolated worktree).
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

- Closing a worktree session (`Ctrl+C`) automatically removes
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

Sessions run inside a dedicated tmux server (`tmux -L thurbox`)
and survive thurbox crashes, restarts, and even multiple
concurrent thurbox instances.

### How it works

- Sessions spawn as tmux windows in the `thurbox` session.
  The tmux pane keeps running regardless of thurbox's lifecycle.
- On every session spawn, Thurbox assigns a `claude_session_id`
  (UUID v4) via the Claude CLI's `--session-id` flag. This tells
  Claude to use a stable conversation ID from the start.
- On shutdown (`Ctrl+Q`), session metadata (including backend
  IDs) is written to the SQLite database at
  `$XDG_DATA_HOME/thurbox/thurbox.db`. Thurbox detaches
  from each session without killing it.
- On next startup, Thurbox discovers existing sessions from tmux,
  matches them to persisted metadata by `backend_id`, and adopts
  them — reconnecting to the live tmux panes with terminal content
  intact. Unmatched persisted sessions fall back to
  `--resume <session-id>` to create new tmux panes.
- External recovery is always possible via
  `tmux -L thurbox attach`.

### State storage

All session state is stored in the SQLite database
(`thurbox.db`). Tables include `sessions`, `worktrees`,
and `metadata` (for the session counter). The database uses
WAL mode for concurrent multi-instance access.

### Worktree preservation

Worktrees are **not** removed on `Ctrl+Q` shutdown — they
persist on disk so the resumed session can continue working
in the same branch checkout. Worktree metadata (repo path,
worktree path, branch name) is saved in the database and
reconstructed on restore.

### Explicit close vs quit

- **`Ctrl+Q` (Quit)**: Detaches from all sessions (tmux panes
  keep running), saves metadata. Sessions resume on next launch
  with terminal content preserved.
- **`Ctrl+C` (Close)**: Permanently kills the tmux pane.
  Its worktree (if any) is removed immediately.
  Closed sessions are not saved and will not be restored.

### Multi-instance support

Multiple thurbox instances can view the same tmux sessions.
The primary instance (first to connect) uses `pipe-pane` for
real-time output streaming. Input can be sent from any instance
via the pane TTY.

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

Roles can be managed from the TUI via the edit project modal
(`Ctrl+E`). The Roles field provides an inline list with
add/edit/delete capabilities; editing a role opens a detail form.

For programmatic role management via the MCP server, see
[MCP_ROLES.md](MCP_ROLES.md).

Projects start with no roles. Users add roles explicitly.
When no roles are defined, sessions spawn with default
(empty) permissions and no role selector is shown.

### Allow / Ask / Deny Semantics

Each role maps to Claude CLI flags:

| Concept | CLI Flag |
|---------|----------|
| Allow (auto-approve) | `--allowed-tools "Read Bash(git:*)"` |
| Deny (blocked) | `--disallowed-tools "Edit"` |
| Ask (prompt user) | *(default for unlisted tools)* |
| Permission mode | `--permission-mode plan` |

Bash scope patterns like `Bash(git:*)` and `Bash(cargo:*)`
are supported in both allowed and disallowed tool lists.

### Role List View

Shows all roles for the active project. Supports
add (`a`), edit (`e` / `Enter`), and delete (`d`).
Pressing `Esc` saves changes to the database and
closes the modal.

### Role Editor View

Edits a single role with four text fields:

- **Name** — role identifier (required, unique)
- **Description** — human-readable summary
- **Allowed Tools** — space-separated tool names
  (auto-approved)
- **Disallowed Tools** — space-separated tool names
  (blocked)

Permission mode defaults to `default` and can be
overridden per-role via the role editor.

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

### Keybindings (role editor detail form)

| Key | Action |
|-----|--------|
| `Tab` / `Shift+Tab` | Cycle fields |
| `Enter` | Save role (return to Roles field) |
| `Esc` | Discard changes (return to Roles field) |

---

## Admin Session

A global admin session provides conversational access to Thurbox
management via Claude Code with the `thurbox-mcp` MCP server
auto-configured.

### How it works

On startup, Thurbox creates:

1. An admin directory at `~/.local/share/thurbox/admin/`
   (or `thurbox-dev/admin/` for dev builds).
2. A `.mcp.json` file in that directory pointing to the
   `thurbox-mcp` binary. Claude Code auto-discovers this file.
3. An "Admin" pseudo-project pinned at index 0 in the project
   list, visually distinguished with a yellow `⚙` prefix.
4. A single admin session with `cwd` set to the admin directory.

The `.mcp.json` is rewritten on every startup to pick up binary
path changes after upgrades.

### Admin project restrictions

- Cannot be edited (`Ctrl+E` shows an error message).
- Cannot be deleted (`Ctrl+D` shows an error message).
- Cannot have additional sessions created (`Ctrl+N` is a no-op
  when sessions exist, or respawns if the session was closed).
- The admin session cannot be closed (`Ctrl+C` shows an error).

### Binary resolution

The `thurbox-mcp` binary path is resolved by:

1. Checking for a sibling of `current_exe()` (works for both
   installed `~/.local/bin/` and dev `target/debug/` builds).
2. Falling back to bare `"thurbox-mcp"` for `$PATH` lookup.

### Persistence

The admin project and session persist to the SQLite database
like all other projects and sessions. On restart, the tmux
pane is re-adopted; `ensure_admin_session` only ensures the
project exists at index 0 and `.mcp.json` is up-to-date.

---

## Theme System

All UI colors are centralized in `src/ui/theme.rs` via semantic
constants on a `Theme` struct. Widget files reference `Theme::ACCENT`,
`Theme::TEXT_PRIMARY`, etc. instead of hard-coded `Color::*` values.

### Why centralized?

- ~50 color references were scattered across 13+ widget files.
  Changing the accent color required editing every file.
- Semantic names (`ACCENT`, `STATUS_BUSY`, `BORDER_FOCUSED`)
  make the intent clear at each call site.
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

Panels use a tri-state focus system (`Focused`, `Active`, `Inactive`)
for clear navigation feedback.

| Level | Border | Title | Meaning |
|-------|--------|-------|---------|
| `Focused` | Thick cyan | Bold cyan | Receiving input |
| `Active` | Plain cyan | Cyan text | Contextually relevant |
| `Inactive` | Plain gray | Gray text | Background |

### Focus mapping

| `InputFocus` | Projects | Sessions | Terminal |
|---|---|---|---|
| `ProjectList` | Focused | Inactive | Inactive |
| `SessionList` | Active | Focused | Inactive |
| `Terminal` | Inactive | Active | Focused |

---

## Status Messages

Status messages replace the previous `error_message: Option<String>`.
Messages have a severity level and auto-dismiss after 5 seconds.

| Level | Badge | Text color | Use case |
|-------|-------|------------|----------|
| `Error` | Red `ERROR` | Red | Validation failures, operation errors |
| `Warning` | Yellow `WARN` | Yellow | Non-blocking issues |
| `Info` | Cyan `INFO` | Gray | Success feedback ("Project saved") |

Positive feedback is shown for: project create/edit/delete, role save,
session start/restart.

---

## Modal Breadcrumbs

Nested modals (up to 3 deep) show a breadcrumb trail at the top:

```text
 Edit "myproject" > Roles > "coder"
 Edit "myproject" > MCP Servers > "thurbox-mcp"
```

This provides navigation context without consuming extra screen space.

---

## Unsaved Changes Guard

When pressing `Esc` in the role or MCP editor with modified fields,
a confirmation overlay asks "Discard changes? y/n". This prevents
accidental loss of edits.

Dirty detection uses snapshot comparison: field values are captured
when the editor opens and compared on `Esc`. If unchanged, the editor
closes immediately without prompting.

---

## Empty Terminal State

When no sessions exist, the terminal panel shows a centered hint box:

```text
┌───────────────────────────────┐
│ No active sessions            │
│                               │
│   Ctrl+N  New session         │
│   F1      Help                │
└───────────────────────────────┘
```

---

## Info Panel Separators

Section boundaries in the info panel use styled `──────` separator
lines instead of blank lines, improving visual structure.

---

## Planned Features

Directional intent, not commitments.
These may change as the project evolves.

- **Multi-session orchestration**: Run N Claude Code instances
  side-by-side, switch between them, broadcast input to all.
- **Task delegation**: Split a task across multiple sessions
  with dependency tracking.
