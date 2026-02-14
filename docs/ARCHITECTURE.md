# Architecture Decisions

Each decision follows a mini-ADR format:
**Choice**, **Why**, **Rejected alternatives**.

---

## ADR-1: The Elm Architecture (TEA)

**Choice**: All state lives in a single `App` model.
Events become messages, `update()` applies them,
`view()` renders the result.

**Why**: TEA makes state transitions explicit and testable.
Every input has a traceable path from event to screen change.
There's no hidden state scattered across components, which matters
when multiple PTY sessions are producing concurrent output.

**Rejected**:

- *Component-based (each panel owns state)* — leads to
  synchronization bugs when sessions interact.
- *Ad-hoc event handlers* — untraceable control flow;
  hard to reason about as the app grows.

---

## ADR-2: PTY pipeline — portable-pty + vt100 + tui-term

**Choice**: `portable-pty` spawns the `claude` CLI,
`vt100::Parser` interprets escape sequences,
`tui_term::PseudoTerminal` renders the parsed screen into ratatui.

**Why**: Each crate handles exactly one concern. `portable-pty`
abstracts platform differences (Linux, macOS). `vt100` gives us a
full in-memory terminal state we can query without scraping.
`tui_term` bridges that state to ratatui widgets
with zero custom rendering code.

**Rejected**:

- *`nix::pty` directly* — Linux-only;
  would require a separate Windows/macOS backend.
- *`alacritty_terminal`* — full terminal emulator,
  far heavier than needed.
  We don't need font rendering or GPU acceleration.
- *Parsing raw ANSI ourselves* — error-prone,
  massive surface area, already solved by `vt100`.

---

## ADR-3: Async — tokio multi-threaded + spawn_blocking

**Choice**: The app runs on tokio's multi-threaded runtime.
PTY read loops run inside `spawn_blocking`
(blocking I/O in a threadpool), while PTY write and event handling
run in `tokio::spawn` (async).

**Why**: PTY reads are blocking by nature
(`read()` on a file descriptor). Putting them in `spawn_blocking`
prevents stalling the async executor. The writer side is naturally
async — it awaits messages from an mpsc channel
and writes when they arrive.

**Rejected**:

- *Single-threaded tokio* — PTY reads would block the entire
  runtime, freezing the UI.
- *`std::thread` for everything* — works but loses tokio's
  structured concurrency, select!, and channel ergonomics.

---

## ADR-4: Input translation — crossterm KeyCode to xterm ANSI

**Choice**: `input.rs` maps crossterm `KeyCode`/`KeyModifiers`
to raw xterm ANSI byte sequences before writing to the PTY.

**Why**: crossterm gives us structured key events.
PTYs expect raw bytes. The translation layer is explicit and
testable — each key has a known byte sequence, and edge cases
(arrow keys, function keys, modifier combos)
are handled in one place.

**Rejected**:

- *Raw passthrough (forward crossterm's raw bytes)* —
  crossterm's internal byte representation doesn't match xterm
  sequences. Modifier keys, in particular, would break.

---

## ADR-5: Responsive layout breakpoints

**Choice**: Three layout tiers based on terminal width:

- `<80 cols` — terminal panel only (full screen)
- `>=80 cols` — two panels (left panel + terminal)
- `>=120 cols` — three panels (left panel + terminal + info)

The left panel is a vertically split two-section panel
containing the project list (top 40%) and session list
(bottom 60%).

**Why**: 80 columns is the smallest usable terminal width. Below
that, showing a sidebar wastes too much space. At 120+, there's
room for supplementary info without shrinking the terminal panel
below readable width. Fixed breakpoints are predictable — the
layout never "jitters" near a threshold.

**Rejected**:

- *Fixed layout (always 3 panels)* — unusable on small terminals.
- *User-configurable breakpoints* — premature complexity.
  Can be added later if needed.

---

## ADR-6: File-based logging only

**Choice**: All tracing output goes to
`~/.local/share/thurbox/thurbox.log`.
Nothing writes to stdout or stderr.

**Why**: The TUI owns stdout entirely. Any stray `println!` or
log line to stdout would corrupt the terminal display. File-based
logging also makes it easy to `tail -f` the log in a second
terminal while developing.

**Rejected**:

- *Stderr logging* — crossterm's alternate screen captures stderr
  on some platforms, still risks display corruption.
- *In-app log panel* — useful eventually, but adds complexity
  before the core features are stable.

---

## ADR-7: Build profiles

| Profile | `opt-level` | LTO | Strip | Debug | Use case |
|---|---|---|---|---|---|
| `dev` | 0 | off | no | yes | Fast iteration |
| `test` | 1 | off | no | yes | Faster tests, still debuggable |
| `release` | 3 | full | yes | no | Distribution binary |
| `release-with-debug` | 3 | full | no | yes | Profiling / flamegraph |

**Why**: `test` at opt-level 1 catches optimization-dependent bugs
earlier while keeping compile times reasonable. The release profile
strips everything for a minimal binary. `release-with-debug` exists
specifically for `perf` / `flamegraph` workflows.

---

## ADR-8: Config file format — TOML

**Choice**: Project configuration uses TOML format, loaded from
`~/.config/thurbox/config.toml` (respects `$XDG_CONFIG_HOME`).

**Why**: TOML is human-readable, easy to hand-edit, and has
first-class Rust support via the `toml` crate (already a
transitive dependency). The XDG convention is standard on Linux
and avoids cluttering `$HOME` with dotfiles.

**Rejected**:

- *JSON* — verbose for config (requires quoting keys, no comments),
  though great for machine interchange.
- *YAML* — indentation-sensitive, surprising edge cases
  (the Norway problem: `NO` parses as boolean `false`).
  Not worth the risk for a config file.
- *CLI flags only* — doesn't scale to multiple projects.
  Users would need wrapper scripts or shell aliases.
- *Embedded in CLAUDE.md* — mixes project-specific AI guidance
  with application configuration; wrong separation of concerns.

---

## ADR-9: Two-section left panel

**Choice**: The left sidebar is a single panel split vertically
into two sections — project list (top) and session list (bottom) —
rather than two independent side-by-side panels.

**Why**: This reuses the existing 3-tier responsive layout
(< 80, >= 80, >= 120 cols) without adding a 4th breakpoint.
At 80 columns, showing two separate sidebar panels would leave
< 40 cols for the terminal — unusable. The vertically stacked
design mirrors the containment relationship (projects contain
sessions) and works at all supported widths.

**Rejected**:

- *Separate project and session panels* — requires >= 160 cols
  to show project + session + terminal simultaneously.
  Most terminals are 80-120 cols wide.
- *Modal/popup project selector* — hides project context while
  working, forces re-opening to switch. Projects are persistent
  context, not transient selections.
- *Tabs for projects* — horizontal tabs consume vertical space
  and don't scale well past 4-5 projects. A vertical list
  scrolls naturally.

---

## ADR-10: Default project — projects list is never empty

**Choice**: When no projects are configured, an ephemeral
"Default" project is created using the current working directory.
It is never persisted to disk. This guarantees
`projects.len() > 0` at all times.

**Why**: An always-non-empty project list eliminates
orphaned-session edge cases, removes empty-state UI branches,
and simplifies `active_project_sessions()` — no `Option` handling
needed. The default project coexists with user-added projects
and disappears on restart once user projects exist.

**Rejected**:

- *Replace default on first user project* — would reassign
  sessions created under the default, causing confusing
  ownership changes mid-session.
- *Persist default to disk* — pollutes the config file with
  auto-generated entries the user didn't create.

---

## ADR-11: Cross-instance live sync via file watching

**Choice**: Multiple Thurbox instances synchronize state by
watching the shared TOML config and state files
(`config.toml`, `state.toml`) for changes using the `notify`
crate. When one instance writes, others detect the change
within ~500ms and merge the new state.

**What syncs**:

- Project configs (add/remove/edit projects and roles)
- Session metadata (names, roles, worktree info)
- Role renames propagate to existing sessions

**What doesn't sync** (instance-local):

- PTY processes and terminal buffer contents
- UI state (focus, modals, scroll position)
- Running Claude CLI permissions (baked in at spawn time)

**Key mechanisms**:

- `src/sync/mod.rs` — `FileWatcher` wraps
  `notify::RecommendedWatcher`, watches parent directories,
  sends `SyncEvent` via tokio mpsc channel
- `AppMessage::Sync` variant — processed in the TEA update loop
- Self-write debouncing (200ms) — prevents reacting to our
  own file writes
- Atomic writes (write-to-temp-then-rename) — prevents readers
  from seeing partial content
- Multi-instance state format — each instance gets its own
  section in `state.toml` tagged by a UUID instance ID

**Why**:

- Zero coordination — no daemon, no lock protocol, no
  discovery. Each instance watches files it already uses.
- Crash-safe — if an instance dies, no stale locks or sockets
  remain. TOML files are the single source of truth.
- Minimal new dependencies — only `notify` is added.

**Rejected**:

- *Unix domain sockets / IPC* — requires a "first instance"
  coordinator or separate daemon. Adds discovery and lifecycle
  complexity.
- *Shared memory / mmap* — complex synchronization,
  crash-unsafe, not portable.
- *SQLite* — heavy dependency for two small config files.
