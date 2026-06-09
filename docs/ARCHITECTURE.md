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
(spawn, adopt, resize, kill, detach, discover). Each session runs
one coding-agent CLI inside the backend. The default backend is
local tmux (`tmux -L thurbox`); the same `TmuxBackend` also runs
over SSH for remote hosts (ADR-13).
`vt100::Parser` interprets escape sequences,
`tui_term::PseudoTerminal` renders the parsed screen into ratatui.

**Why**: The trait-based design keeps the session transport
behind a clean boundary so the app layer never touches tmux
directly. tmux provides truly persistent sessions
that survive thurbox crashes/restarts, multiple thurbox instances
share the same running sessions, and external recovery is
possible via `tmux -L thurbox attach`.

**Previous design**: `portable-pty` spawned the agent CLI
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

**Generalized off-the-hot-path pattern**: the same
`spawn_blocking` → `mpsc` → poll-in-`tick()` shape keeps every other
blocking side effect off the UI thread, so neither rendering nor
`Ctrl+N` ever freezes. Each operation owns an in-flight guard + result
receiver on `App`, kicks off the blocking work, and applies the result
when `tick()` polls `try_recv()`:

- **Worktree sync** (`Ctrl+S`) — `git rebase` per worktree
  (`worktree_sync_rx`, the original instance of the pattern).
- **Per-tick metrics** — `refresh_system_metrics` (sysinfo + statusline
  file reads + the active pane's PID lookup) and `refresh_active_git_stats`
  (`git` diff/status shell-outs). The `sysinfo::System` is *moved into*
  the worker and returned with the result so CPU deltas persist across
  refreshes; a single in-flight guard prevents overlap.
- **Interactive spawn** — `git worktree add` (`spawn_worktree_session`)
  and `Session::spawn` (PTY/tmux window creation, 500 ms+) for the
  new-session wizard run on blocking tasks, with the follow-up
  (session adoption, task-prompt delivery) carried in a `Pending*`
  continuation applied on completion. Programmatic spawns
  (automations/tasks, restore) stay **synchronous** — they read the new
  session's id straight back, so they cannot defer it to a later tick.

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

The left panel is a single session list.

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

## ADR-8: State storage — SQLite

**Choice**: All persistent state (sessions, worktrees, VMs,
containers, automations) is stored in a single SQLite
database at `~/.local/share/thurbox/thurbox.db` (respects
`$XDG_DATA_HOME`). WAL mode enables concurrent multi-instance
access. Agent definitions are the one exception: they live in a
human-editable TOML file (see ADR-19), not the database.

*This supersedes the original TOML file-based approach
(`~/.config/thurbox/config.toml`), which was eliminated after
the SQLite migration.*

**Why**: SQLite provides atomic transactions, concurrent access
via WAL mode, and a single source of truth. Multi-instance sync
uses `PRAGMA data_version` polling (see ADR-7b). The TUI provides
all editing UI — there is no need for a human-editable config file.

Every connection sets a **5 s busy_timeout** (the DB is shared by
the TUI, `thurbox-cli`, and the automation heartbeat; writes are
short single-row upserts, so a bounded wait beats an immediate
`SQLITE_BUSY` error or an unbounded freeze). The append-only
**audit log is pruned to 90 days** on `Database::open` — entries
are debugging breadcrumbs, not compliance data, and unbounded
growth would bloat the database over months of use.

**Rejected**:

- *TOML config file (previous)* — race conditions when multiple
  instances write concurrently; split source of truth between
  config.toml and state files (sessions); no atomic multi-key
  updates. (Agent definitions are read-mostly and not subject to
  concurrent writes, so they remain in TOML — see ADR-19.)
- *JSON* — verbose for config, no atomic writes without
  temp-file-rename pattern.
- *CLI flags only* — doesn't scale to multiple sessions and
  long-lived configuration.
- *Embedded in CLAUDE.md* — mixes repo-specific AI guidance with
  application configuration; wrong separation of concerns.

---

## ADR-8b: Automations fire with or without the TUI

**Choice**: Automations fire from three places that all funnel
through one headless entry point, `thurbox-cli automation tick`:
the TUI tick loop, a detached **tmux heartbeat keeper** window
(`automation-heartbeat`, armed on TUI startup and on `automation
create`, looping `tick` every 60 s), and optional systemd/launchd
units (`packaging/`) for reboot-proof firing. Concurrency is made
safe by **claim-based firing** — `Database::claim_due_automation`
advances `next_run_at` with an atomic compare-and-swap, so exactly
one firer wins per due automation.

**Why**: The previous one-shot "scheduled command" fired even with
the TUI shut down by riding tmux's `run-shell` timers; the new
model must keep that durability for recurring + spawn automations.
A live keeper window both runs the heartbeat and keeps the tmux
server alive (a bare pending `run-shell` job does not), so even
spawn-only automations fire with no other sessions. Claim-first
ordering gives at-most-once semantics (a crash loses a run rather
than double-firing), the right default for agent prompts. tmux is
local-only; the send/spawn dispatch sits behind a seam so a future
remote/SSH `SessionBackend` (ADR-2) slots in without changing the
scheduler.

**Rejected**:

- *Per-automation `run-shell` timers (old style)* — precise to the
  second but require bookkeeping + re-arming N timers on startup; a
  single polling keeper is simpler and naturally handles
  create/edit/delete.
- *A bespoke long-running daemon* — duplicates what tmux (already
  required) and systemd/launchd provide; more moving parts.

---

## ADR-9: Flat session list (no project grouping)

**Choice**: The sidebar is a single flat list of sessions. There
is no "project" layer above sessions: each session picks its own
agent and repo selection at creation time.

**Why**: Earlier versions grouped sessions under projects (one
project → many sessions, with shared repos). In practice users
created one session per task, so the project layer was pure
overhead — an extra navigation level, an extra creation step, and
an extra deletion guard. Storage migration v16 dropped the
`projects`, `project_repos`, `project_vm_config`, and
`project_container_config` tables and removed `project_id` columns
from `sessions`, `vms`, and `containers`.

**Rejected**:

- *Two-section sidebar (projects on top, sessions on bottom)* —
  the previous design. Cost a navigation level and a creation
  step for no gain in the typical one-session-per-task workflow.
- *Modal/popup project selector* — hides context while working,
  forces re-opening to switch.
- *Tabs for projects* — horizontal tabs consume vertical space
  and don't scale well past 4-5 entries.

---

## ADR-11: Trait-based session backends

**Choice**: Session lifecycle is abstracted behind a
`SessionBackend` trait (`src/agent/backend.rs`). The `Session`
struct wraps the trait and manages reader/writer loops once,
regardless of which backend is active.

**Why**: Keeping session lifecycle behind a trait boundary leaves
the app layer completely backend-agnostic. The backends today are
local tmux and one SSH backend per configured host (both
`TmuxBackend` over a `TmuxTransport`; see ADR-13), and the seam means
the transport can evolve without touching `App`, `Session`, or any UI
code.

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
  the tmux backend uses synchronous `Command::new("tmux")`.
  Can be added via `async-trait` if a future backend needs it.

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

## ADR-13: Remote sessions via an SSH tmux transport

**Choice**: Run agent sessions on a remote host by launching the
same tmux control-mode protocol over SSH. `LocalTmuxBackend` is
generalized into `TmuxBackend { transport, socket, session, name }`
where `transport: TmuxTransport` is either `Local` (a bare
`Command::new("tmux")`) or `Ssh { destination, ssh_opts }`
(`ssh <dest> tmux …`). The transport's *only* job is to build the
`Command`; everything downstream — the control-mode reader/writer
threads, pane registration, `send-keys`/`%output` — is byte-for-byte
identical (`control_mode.rs` was already transport-agnostic).

Remote hosts are declared as data in `~/.config/thurbox/hosts.toml`
(`session::HostDef`/`HostRegistry`, loaded by
`agent::host_config::load_or_seed`), each registered as a backend
named `ssh:<host>` via `TmuxBackend::from_host`.

**Why**: The local-vs-remote difference is exactly one line (how the
tmux process is launched). The per-session control commands travel
over the stdin pipe, not ssh argv, so only the one-time
`attach-session` launch crosses the ssh boundary. Relying on the
system `ssh` binary + `~/.ssh/config` keeps auth/keys/multiplexing
out of thurbox (ControlMaster/ControlPersist + ServerAliveInterval
are recommended in the seeded `ssh_opts`).

**Key design decisions**:

- **Lazy registration**: SSH backends are registered but *not*
  connected at startup (`check_available`/`ensure_ready` deferred to
  first use via `App::backend_for`), so a down host never blocks the
  TUI.
- **Selection**: `SessionConfig.backend` (`ssh:<host>` or `None`).
  The TUI shows a host picker as the first new-session step (skipped
  when no hosts are configured); `thurbox-cli session create --host`
  is the headless equivalent.
- **Persistence/restore**: `backend_type` already round-trips in
  SQLite; restore was changed to discover windows **per backend** so
  remote sessions re-adopt against their own host's tmux.
- **Remote worktrees**: `git::*_on(host, …)` variants run
  `ssh <dest> git -C <repo> …`. Remote worktree paths resolve under
  the host's `worktrees_dir` (or `$HOME/.local/share/thurbox/…`
  resolved + cached over ssh).

**Module placement**: `HostDef`/`HostRegistry` live in `session/`
(the dependency sink) so both `agent` (builds the backend) and `git`
(runs git over SSH) can depend on them without violating the
module-isolation rules.

**Riskiest area**: SSH reconnect on a flapping link — `reconnect_control`
reopens the ssh connection; ControlMaster + keepalives mitigate
stalls. Worth the most manual testing.

**Rejected**:

- *A `TmuxTransport` trait with `Box<dyn>`* — an enum with two
  variants is simpler; promote to a trait only if a third transport
  (e.g. container exec) appears.
- *Embedded SSH library (russh, etc.)* — reimplements `~/.ssh/config`,
  agent forwarding, and multiplexing that the system `ssh` already
  provides.

---

## ADR-7b: Multi-Instance Sync — SQLite with PRAGMA data_version

**Choice**: Multiple thurbox instances synchronize all state
(sessions, worktrees, VMs, containers, automations)
via a shared SQLite database
(`~/.local/share/thurbox/thurbox.db`). Each instance polls
`PRAGMA data_version` to detect external changes. SQLite's WAL mode
handles concurrent access safely. Deletions use soft delete
(`deleted_at` column).

*This supersedes the original TOML file-based approach. The migration
to SQLite resolved race conditions where concurrent `save_state()` calls
could overwrite each other's writes.*

Session **I/O is NOT coordinated** via the database. Instead, each
instance independently connects to tmux and adopts all visible sessions.
Tmux natively handles concurrent clients: output is broadcast to all
connected clients, and input commands are serialized. This enables true
multi-instance collaboration without application-level locks or
ownership restrictions.

**Why**: This approach is:

- **Atomic**: SQLite transactions prevent torn writes and race conditions
- **Portable**: Works on Linux, macOS, any system with a filesystem
- **TEA-compatible**: External changes flow through the message pipeline
- **Graceful**: Single instance has zero polling overhead
- **Collaborative**: All instances can interact with the same sessions
  simultaneously (like tmux attach with multiple clients)
- **Single source of truth**: No split-brain between state files and DB

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

- **Not human-readable**: Unlike TOML, users cannot directly edit state.
  The TUI provides all editing UI (session creation, scheduling, theme
  selection). Agent definitions are the deliberate exception and remain
  hand-editable TOML (ADR-19).
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
- *TOML file-based sync (previous approach)* — race conditions when
  multiple instances write concurrently; no atomic multi-key updates;
  split source of truth between config.toml and state files
  (sessions) caused sync bugs.

---

## ADR-13: Headless CLI as Separate Binary

**Choice**: Headless automation lives in a separate binary
(`thurbox-cli`) that shares the same SQLite database as the TUI.
It exposes session, VM, container, scheduled-command, and
editor-command management as subcommands, printing JSON results.

**Why**: A separate binary keeps scripting/automation out of the
TUI's event loop. The TUI already polls `PRAGMA data_version`
every 250ms (ADR-7b), so changes made by `thurbox-cli` appear
automatically — no new synchronization mechanism is needed. The
`cli` module imports `storage`, `session`, `session_ops`, `sync`,
and `agent::tmux`, but never `app` or `ui`, so it can operate
without a terminal UI.

**Rejected**:

- *Embedded in the TUI binary* — would force the TUI to multiplex
  a non-interactive command path alongside its crossterm event
  loop.
- *A long-running daemon* — adds operational complexity; the
  shared SQLite DB plus tmux already provide the coordination a
  one-shot CLI needs.

---

## ADR-14: Centralized Theme Module

**Choice**: All UI colors are defined as associated constants on a
`Theme` struct in `src/ui/theme.rs`. Widget files import `Theme::*`
instead of using `Color::Cyan`, `Color::Gray`, etc. directly.

**Why**: ~50 hard-coded color values were scattered across 13+ widget
files. This made visual consistency difficult to maintain and made
any color scheme change require editing every file. Semantic names
(`ACCENT`, `STATUS_BUSY`, `TEXT_MUTED`) clarify intent at each call
site and enable future theming (dark/light/custom) with a single
module swap.

**Design**: `Theme` uses `const` associated items rather than a
global singleton or trait. This keeps it zero-cost (no runtime
dispatch, no initialization), works in const contexts, and is
trivially testable. Composite styles (e.g., `focused_title()`) are
`const fn` methods that combine colors with modifiers.

**Rejected**:

- *Global singleton / `lazy_static`* — runtime overhead, mutex
  contention in render path, unnecessary for static color values.
- *Trait-based theming* — over-engineering for the current need.
  Can be layered on top later if user-selectable themes are added.
- *CSS-like stylesheets* — no Rust TUI framework supports this
  natively; would require a custom parser and resolver.

---

## ADR-19: Declarative agent definitions

**Choice**: Each session runs exactly one coding-agent CLI chosen
at creation time; each agent runs with its own default config.
Agents are described as **data** in `~/.config/thurbox/agents.toml`
(sibling of any other config), seeded with built-ins (claude,
codex, gemini, opencode, aider, vibe) on first run via
`agent::agent_config::load_or_seed`. An `AgentDef` carries a
`command`, `args` (always passed — bake in flags like a model
here if you want), and argument-template groups (`resume_args`,
`fork_args`, `new_session_args`), plus a `resume_latest` flag. A
single `agent::GenericProvider` (an `AgentProvider`) launches any
defined agent by substituting `{id}` and appending each group only
when its driving value is present. Only `claude` can be addressed by
the thurbox-generated id (`--session-id {id}`); the other built-ins
can't pin or report a session id, so they set `resume_latest = true`
and use id-less, cwd-scoped flags (`codex resume --last`, `opencode
--continue`, …) that make the agent resolve "the last session in this
directory" itself. `resume_latest` only governs *when* the resume
group fires at restart (`session_ops::resume_trigger_for`): for these
agents restart always resumes; claude still defers to an on-disk
transcript check.

**Why**: Thurbox started as Claude-Code-specific, with a hard-coded
`ClaudeProvider` plus roles, skills, profiles, and an MCP/plugin
surface tied to one agent's permission model. Generalizing to "run
any coding agent" meant the launch contract had to be data, not
code: users add or tweak agents by editing TOML, with no recompile
and no per-session permission/prompt/tool configuration. The
`session::AgentDef` / `AgentRegistry` types are pure data (no
filesystem, no local imports) so they satisfy the `session/`
isolation rule; the TOML loading and the provider bridge live in
`agent`.

**Group precedence**: fork wins over resume, which wins over a
fresh `new_session` id; static `args` follow. A group with no
value is simply omitted — no "unresolved placeholder" heuristics.

**Config, not DB**: Agent definitions deliberately live in TOML
rather than SQLite (ADR-8). They are read-mostly, hand-editable,
and shared across instances by re-reading the file — there is no
concurrent-write hazard that would justify moving them into the
database.

**Rejected**:

- *Hard-coded providers per agent* — the previous `ClaudeProvider`
  approach; adding an agent meant a code change and release.
- *Per-session roles / permissions / prompts / tools* — removed
  with the pivot. They were Claude-specific and did not generalize
  across agents; a session now configures only its agent.
- *Agent definitions in SQLite* — overkill for read-mostly,
  user-authored config; TOML keeps them inspectable and diffable.
