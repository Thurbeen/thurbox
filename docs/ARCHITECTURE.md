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

## ADR-2: Session pipeline — SessionBackend + vt100 + tui-term

**Choice**: A `SessionBackend` trait abstracts session lifecycle
(spawn, adopt, resize, kill, detach, discover). The default
backend is `LocalTmuxBackend` (`tmux -L thurbox`).
`vt100::Parser` interprets escape sequences,
`tui_term::PseudoTerminal` renders the parsed screen into ratatui.

**Why**: The trait-based design allows plugging in different
transports (local tmux, SSH+tmux, Docker, cloud VM) without
changing the app layer. tmux provides truly persistent sessions
that survive thurbox crashes/restarts, multiple thurbox instances
share the same running sessions, and external recovery is
possible via `tmux -L thurbox attach`.

**Previous design**: `portable-pty` spawned the `claude` CLI
directly. Sessions died when thurbox exited, terminal content was
lost on restart, and multiple instances had no coordination.

**Rejected**:

- *`portable-pty` (previous)* — no session persistence,
  no multi-instance sharing, terminal content lost on restart.
- *`alacritty_terminal`* — full terminal emulator,
  far heavier than needed.
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

## ADR-11: Trait-based session backends

**Choice**: Session lifecycle is abstracted behind a
`SessionBackend` trait (`src/claude/backend.rs`). The `Session`
struct wraps the trait and manages reader/writer loops once,
regardless of which backend is active.

**Why**: Thurbox needs to support multiple deployment targets:
local tmux today, SSH+tmux, Docker, and cloud VMs in the future.
A trait boundary keeps the app layer completely backend-agnostic.
Adding a new backend is a matter of implementing `SessionBackend`
without touching `App`, `Session`, or any UI code.

**Trait methods**: `check_available`, `ensure_ready`, `spawn`,
`adopt`, `discover`, `resize`, `is_dead`, `kill`, `detach`.

**Key design decisions**:

- `spawn()` returns `(backend_id, output_reader, input_writer)`.
  The `Session` struct owns the reader/writer loops.
- `adopt()` reconnects to an existing session and returns initial
  screen content for parser seeding.
- `discover()` lists existing sessions for restore-on-startup.
- `detach()` stops streaming without killing the session.
- `kill()` permanently destroys the session.

**Rejected**:

- *Async trait methods* — added complexity for no benefit since
  all current backends use synchronous `Command::new("tmux")`.
  Can be added via `async-trait` if a future backend needs it.
- *Backend per session* — over-engineering; all sessions in a
  thurbox instance share the same backend.

---

## ADR-12: Local tmux as default backend

**Choice**: The first `SessionBackend` implementation is
`LocalTmuxBackend`, using a dedicated tmux server
(`tmux -L thurbox`) with session name `thurbox`. All I/O goes
through tmux control mode (`-C`).

**Why**: tmux provides session persistence (survives crashes),
multi-instance support (multiple thurbox processes can independently
interact with the same sessions), and external recovery
(`tmux -L thurbox attach`). It handles terminal capability queries
(DA1/DA2) natively via `extended-keys on`, eliminating the need for
thurbox to intercept and respond to these sequences.

Control mode (`-C`) supports multiple concurrent client connections,
each receiving independent output streams. Each thurbox instance
establishes its own control mode connection, allowing all instances
to simultaneously monitor and interact with the same tmux sessions.
Output arrives as `%output` notifications (octal-encoded), input is
sent via `send-keys -H` (hex-encoded). This eliminates the previous
`pipe-pane` + FIFO approach which suffered from tmux data-loss
bugs (#641, #2989), required 3 external deps in the data path
(`mkfifo`, `stdbuf`, `cat`), and had no flow control.

**Configuration on init**:

- `remain-on-exit on` — keeps panes alive after process exit
- `status off` — no tmux status bar (thurbox renders its own)
- `default-terminal xterm-256color` — standard terminal type
- `history-limit 5000` — reasonable scrollback
- `extended-keys on` — enhanced key reporting
- `window-size manual` — windows size independently
- `pause-after 5` — flow control (auto-resumed by reader)

**Window naming**: `tb-<session-name>` prefix for discovery.

**Output streaming**: `%output` notifications from control mode,
demultiplexed by pane ID into per-pane broadcast channels. Multiple
instances can simultaneously register the same pane; output is
broadcast to all registered channels via `HashMap<String, Vec<SyncSender>>`.
Each channel feeds a `ControlModeReader` (implements `Read`) consumed
by the existing `Session::reader_loop`. This allows multiple instances
to independently parse and render terminal state in real-time.

**Input**: `send-keys -H <hex>` through the shared control mode
stdin, wrapped in a `ControlModeWriter` (implements `Write`).

**Command synchronization**: All commands that precede a
`send_command` (waited) call must themselves be waited. A
fire-and-forget (`send_command_nowait`) leaves an unclaimed
`%begin`/`%end` response in the stream that can steal the next
waiter. `send_command_nowait` is only safe when nothing follows
(e.g., `detach`) or when issued from the reader thread itself
(e.g., pause resume).

**Session restore**: On reconnect, `capture-pane -p -e -S -`
provides a quick initial approximation of the screen content
(text + colors, but not full escape sequences). A forced resize
then triggers SIGWINCH, causing the TUI application to repaint
its full screen through the normal `%output` stream — this
delivers pixel-perfect rendering with all original formatting.

**Rejected**:

- *`pipe-pane` + FIFO (previous)* — intermittent data loss from
  tmux bugs #641/#2989, required `mkfifo`/`stdbuf`/`cat` in the
  data path, no flow control, timing race on initial capture.
- *Screen/dtach* — less widely available, fewer features.

---

## ADR-7: Multi-Instance Sync — File-based polling with mtime

**Choice**: Multiple thurbox instances synchronize session metadata
via a shared TOML file (`~/.local/share/thurbox/shared_state.toml`).
Each instance periodically polls the file's modification time (mtime)
and reads its content if changed. Writes are atomic (temp file + rename).
Session counter is merged using `max()`, deletions use tombstones
with a 60-second TTL.

Session **I/O is NOT coordinated** via the shared file. Instead, each
instance independently connects to tmux and adopts all visible sessions.
Tmux natively handles concurrent clients: output is broadcast to all
connected clients, and input commands are serialized. This enables true
multi-instance collaboration without application-level locks or
ownership restrictions.

**Why**: This approach is:

- **Simple**: POSIX file I/O for metadata, tmux I/O built-in for sessions
- **Portable**: Works on Linux, macOS, any system with a filesystem
- **Observable**: Users can inspect/edit `shared_state.toml` directly
- **TEA-compatible**: External changes flow through the message pipeline
- **Graceful**: Single instance has zero polling overhead
- **Debuggable**: File format is human-readable TOML
- **Collaborative**: All instances can interact with the same sessions
  simultaneously (like tmux attach with multiple clients)

The polling latency (~250ms average) is acceptable for session
metadata sync — users rarely create/delete sessions faster than
this, and terminal I/O (with millisecond response) dominates perception.

**Multi-Instance I/O Model**: Rather than using an ownership model
to prevent duplicate I/O, each instance maintains its own control mode
connection to tmux. Tmux's architecture already supports this:

- Each control mode client receives independent output streams
- Output is duplicated by tmux to all connected clients
- Input commands (`send-keys`) are serialized by tmux
- No application-level coordination needed

This design choice (post-ADR) was made to enable true collaboration while
avoiding the complexity of application-level locks or message-passing for
I/O coordination.

**Trade-offs**:

- **~250ms latency** vs <1ms for inotify: acceptable for metadata
- **Eventual consistency**: brief windows where instances diverge;
  last-write-wins resolves conflicts
- **~4 stat() calls/sec** per instance: negligible disk overhead
- **Independent terminal state**: Each instance maintains its own
  `vt100::Parser`, so concurrent updates may briefly diverge. Instances
  converge quickly as output is replayed.
- **Concurrent input interleaving**: When multiple users type
  simultaneously, characters arrive in order at tmux but may display
  interleaved (same as `tmux attach` with multiple clients). This is
  **expected behavior** for multi-user terminal sessions.

**Rejected**:

- *Event-based sync (inotify/kqueue)* — platform-specific, requires
  different implementations for Linux/macOS/BSD, more complex error
  handling (file deletion, permission issues), adds monitoring
  overhead even for single-instance deployments.
- *gRPC/REST daemon* — requires deploying and managing a persistent
  service, adds operational complexity, increases failure surface area
  (daemon crashes, socket issues), incompatible with offline usage.
- *Git-based sync* — requires git repo for state, introduces gc/
  rebase issues, incompatible with non-repo environments.
- *SQLite shared DB* — adds database locking complexity, risk of
  corruption from concurrent access, not human-readable.
