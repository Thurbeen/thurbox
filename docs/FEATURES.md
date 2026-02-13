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
modifier. Ctrl-prefixed keys are intercepted by Thurbox
as global commands.

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
| `Ctrl+L` | Global | Cycle focus: Project → Session → Terminal |
| `Ctrl+I` | Global | Toggle info panel (width >= 120) |
| `j` / `Down` | Project list | Next project |
| `k` / `Up` | Project list | Previous project |
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
| All other keys | Focused terminal | Forwarded to PTY |

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

### Auto-cleanup

- Closing a worktree session (`Ctrl+X`) automatically removes
  the worktree via `git worktree remove --force`.
- Quitting Thurbox (`Ctrl+Q`) cleans up all active worktrees
  before shutdown.
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

## Planned Features

Directional intent, not commitments.
These may change as the project evolves.

- **Multi-session orchestration**: Run N Claude Code instances
  side-by-side, switch between them, broadcast input to all.
- **Session persistence**: Save/restore session layouts
  and PTY history across restarts.
- **Task delegation**: Split a task across multiple sessions
  with dependency tracking.
