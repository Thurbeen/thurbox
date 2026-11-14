# Architecture Decisions

Each decision follows a mini-ADR format:
**Choice**, **Why**, **Rejected alternatives**.

---

## ADR-2: Session pipeline — SessionBackend + vt100 + tui-term

**Choice**: A `SessionBackend` trait abstracts session lifecycle
(spawn, adopt, resize, kill, detach, discover). Each session runs
one coding-agent CLI inside the backend. The default backend is
the platform's multiplexer run locally (`tmux -L thurbox`; psmux on native
Windows), and the same adapters run over SSH or WSL for a host
([runtime-mux](../runtime/runtime-mux.md#hosts-and-transports)).
`vt100::Parser` interprets escape sequences,
`tui_term::PseudoTerminal` renders the parsed screen into ratatui.
The parser uses vt100 0.18's nonzero dimensions; backend sizes still pass
through the existing two-cell floor. `vendor/tui-term` carries the published
0.3.4 source with its vt100 dependency aligned to 0.18 and its parser size
arguments converted. The library source, test fixtures, manifest and license are
vendored. Remove the patch when a published tui-term supports vt100 0.18.

**Why**: The trait-based design keeps the multiplexer behind a clean
boundary so no consumer touches tmux directly (ADR-11). tmux provides truly persistent sessions
that survive thurbox crashes/restarts, multiple thurbox instances
share the same running sessions, and external recovery is
possible via `tmux -L thurbox attach`.

**Rejected**:

- *`portable-pty`* — no session persistence,
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
when `tick()` polls `try_recv()`: the kernel's worker stores and the
coordinator's workers (ADR-23).

- **Metrics** — `kernel::metrics` samples `sysinfo` on a worker. The
  `sysinfo::System` is *moved into* the worker and returned with the sample
  so CPU deltas persist across refreshes; a single in-flight guard prevents
  overlap.
- **Session commands** — creating, sending to, renaming and deleting a
  session are calls of Fleet's command line (`kernel::command::fleet`),
  each run on a command worker, so a slow Fleet never stalls a frame.

**Rejected**:

- *Single-threaded tokio* — PTY reads would block the entire
  runtime, freezing the UI.
- *`std::thread` for everything* — works but loses tokio's
  structured concurrency, select!, and channel ergonomics.

---

## ADR-4: Input translation — crossterm KeyCode to xterm ANSI

**Choice**: `agent::input` maps crossterm `KeyCode`/`KeyModifiers`
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

### An image on the clipboard is the agent's paste, not ours

thurbox's clipboard transport carries text (`clipboard::copy`/`paste`, and the
OSC 52 leg by construction). An image therefore cannot be pasted here at all —
but the CLI in the pane can fetch one itself when it sees the paste chord, so
`Ctrl+V` is **handed to it** rather than swallowed. `kernel.paste` declines the
chord, the same `Some(false)` answer `kernel.copy` uses when there is no
selection, and the press falls through to the focused terminal.

Inside WSL the decision cannot be made locally: WSLg bridges the clipboard's
*text* only, so while Windows holds an image the X clipboard still hands out the
last text copied — a paste there inserts something stale rather than nothing,
which is worse. `clipboard::ImageProbe` asks `powershell.exe`
(`Clipboard::ContainsImage`) instead. That costs ~0.42 s measured, which is why
it is the seventh instance of the worker pattern rather than a call on the loop:
the press is claimed immediately and acted on when the answer lands. Off WSL
nothing is asked — the local clipboard is the one being copied into, and
arboard's answer is the whole truth.

Three bounds make that latency safe. The **target** is the session the press was
aimed at, taken at the press: 0.42 s is long enough to change panes, and a paste
landing in a pane nobody aimed it at corrupts what is being typed there. **One
question runs at a time** — key auto-repeat makes presses tens of times faster
than the answer, and a `powershell.exe` per repeat is a held key bringing the
machine down — with the presses that arrive while it is out kept (at most eight)
and asked about separately, because an answer may only classify presses that
predate it. And the child has a **five-second deadline**, after which it is
killed and the press handed to the agent unanswered: WSL interop can wedge
outright, and the question after this one waits on its answer. A probe that was
killed is never read as "no image" — that reading is exactly the stale paste
this path exists to stop.

**Rejected**: *a second chord for "give the paste to the agent"* — cheap and
exact, but it leaves the ordinary `Ctrl+V` after copying an image still pasting
stale text, which is the half of the bug that corrupts a prompt silently.
*Resolving the focus when the answer lands* — it reads as "paste where the
person is looking", but the intent belongs to the press, and a paste that
arrives in a pane it was not aimed at is the same corruption seen from the other
side. *Spending one answer on every press waiting behind it* — the clipboard can
change while the question is out, and a press classified by what preceded it is
exactly the stale paste this section is about.
*Detecting the image with arboard* — thurbox builds it with
`default-features = false`, the build without `get_image`, and the X clipboard
it would read does not carry the Windows image anyway.

### A paste on Windows arrives as keys, not as a paste

`Event::Paste` is unix-only. crossterm's Windows source reads console
INPUT_RECORDs, and `EnableBracketedPaste` there is documented as unsupported
(`execute_winapi` returns `Unsupported`), so a paste into the TUI arrives as a
stream of key presses — every line break an `Enter` that thurbox forwarded to
the agent, which submitted the prompt one line at a time.

`coordinator::paste` turns that stream back into a paste before anything is
dispatched. Its shape is dictated by what the console actually delivers,
measured against the `ssh` → ConPTY path with a key dumper rather than assumed:

- **The bracketed-paste markers are gone.** The ConPTY strips `ESC[200~` /
  `ESC[201~` before the app sees them; a paste is plain `Char`/`Enter` events
  with no framing at all. There is nothing to match on, so there is no marker
  machinery.
- **Editing keys arrive whole.** An arrow key, `Delete`, `Ctrl+Delete`, a
  function key each arrive as their own `KeyEvent` (`KeyCode::Left`, …), not as
  a run of `ESC` `[` `…` character events.

So the only thing separating a paste from typing is **timing**, and two small
rules follow:

1. **Grouping is by the inter-key gap.** A plain character joins a run when it
   lands within 10 ms of the previous one; a person's fastest two keys stay
   well above that and a pasted stream runs far under it. **Everything that is
   not a plain character passes straight through the instant it arrives** — an
   arrow, `Delete`, `Esc`, any `Ctrl`/`Alt` chord is never buffered, so it can
   never be swallowed.
2. **A run is a paste only if it carries an interior newline.** Delivering
   newlines to the agent one keystroke at a time is the sole thing this path
   exists to prevent — it is what submits a prompt mid-paste — so it is the only
   thing acted on. A newline-free run is emitted as the keys it was, because the
   clock times when a key is *read*, not when it was pressed: under load two
   keys a person typed 100 ms apart can be read together, and coalescing on
   length alone announced a phantom "pasted 2 characters" mid-type. A run that
   merely *ends* at a newline is a typed line submitted with `Enter`, so it too
   stays keys and still submits.

The result is that a multi-line paste reaches the agent as one bracketed paste
— which it renders as a paste (Claude Code collapses it to `[Pasted text #1 +N
lines]`) rather than as typing — while every editing key and every keystroke of
ordinary typing is untouched. The loop waits at most 10 ms for the next key
while a run is open and not at all otherwise, so an idle interface polls exactly
as before. The coalescer is inert where `Event::Paste` arrives on its own, and
its decisions are unit-tested on every platform against a driven clock.

This is the *inbound* half of a journey whose outbound half is runtime-mux's
`PsmuxPaste` ([psmux divergences](../runtime/runtime-mux.md#psmux-divergences-from-tmux)):
the reassembled paste is carried to a psmux pane in one piece by psmux's own
paste command.

---

## ADR-6: File-based logging only

**Choice**: All tracing output goes to `thurbox.log.<date>` in the data
directory (`~/.local/share/thurbox/`), one file per day, the newest 30 kept
(`coordinator::boot::log_appender`). Nothing writes to stdout or stderr.

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
| `dev` | 1 (dependencies 3) | off | no | yes | Fast iteration |
| `test` | 1 | off | no | yes | Faster tests, still debuggable |
| `release` | 3 | full | yes | no | Distribution binary |
| `release-with-debug` | 3 | full | no | yes | Profiling / flamegraph |

**Why**: `dev` is at opt-level 1, with dependencies at 3, because the interface
renders through an embedded Lua VM on every frame and `cargo run` is the
documented way to run it (`Cargo.toml` says why). `test` at opt-level 1
catches optimization-dependent bugs
earlier while keeping compile times reasonable. The release profile
strips everything for a minimal binary. `release-with-debug` exists
specifically for `perf` / `flamegraph` workflows.

---

## ADR-8b: Automations fire with or without the interface

**Choice**: Automations live in `automations.json` beside `settings.toml`
(`config::automations`). Two firers share the file: the interface ticks every
15 seconds while it runs (`kernel::automations`), and `thurbox-cli automation
tick` is the same tick for an OS timer (the units in `packaging/systemd` and
`packaging/launchd`). A fire is claimed in the file, under the file lock,
before it runs, and its outcome is written after. A run still marked running
past its deadline is recorded as timed out by the next tick. What a fire does
to a session goes through Fleet's command line.

**Why**: a schedule must keep firing with no interface open, and Fleet has no
schedule kind. Claim-first gives at-most-once semantics: a crash loses a run
rather than firing it twice, the right default for agent prompts.

**Rejected**:

- *A bespoke long-running daemon* — duplicates what an OS timer already
  provides; more moving parts.

---

## ADR-9: Flat session list (no project grouping)

**Choice**: There is no "project" entity above sessions: each session picks
its own agent and repository when it is created. The session list
(`ui/plugins/10_sessions.lua`) renders selectable host and repository fold
rows, with child sessions nested under their parent within each repository.
These are view rows derived from host config and each session's `cwd` and
parent (the Task's `gateway.fleet.dev/parent` label); they add no entity and
no creation step.

**Why**: users create one session per task, so a project layer is overhead —
an extra navigation level, an extra creation step, and an extra deletion
guard.

**Rejected**:

- *Modal/popup project selector* — hides context while working,
  forces re-opening to switch.
- *Tabs for projects* — horizontal tabs consume vertical space
  and don't scale well past 4-5 entries.

---

## ADR-11: Trait-based session backends

**Choice**: Session lifecycle is abstracted behind a
`SessionBackend` trait (`crates/runtime-mux/src/contract.rs`). The `Session`
struct wraps the trait and manages reader/writer loops once,
regardless of which backend is active.

**Why**: Every consumer — the runtime protocol (`runtime_mux::protocol`) and
thurbox's `kernel` — reaches a session through the trait and the registry that
holds one backend per route
(ADR-29), never through an adapter, which thurbox's restrictions in
`crates/architecture.toml` enforce. The adapters today are
`TmuxBackend`, `PsmuxBackend`, and `RmuxBackend`, peers over the shared tmux-protocol server
(ADR-31), each reached locally or on a host over a `TmuxTransport`
([hosts and transports](../runtime/runtime-mux.md#hosts-and-transports)).
A multiplexer that does not speak the tmux protocol is a new adapter behind the
same trait, not a branch in a consumer.
The tmux-compatible connection takes its optional flow-control setup command
from each adapter's `ControlPolicy`; a backend without that facility leaves it
absent. The shared lifecycle asks for outcomes through `SessionBackend` and
does not send control-mode commands.

**Roles.** `SessionBackend` is four role traits plus a name and a
`Capabilities` value (the methods themselves are `crates/runtime-mux/src/contract.rs`):

- `PaneStream` — a pane's output stream: `spawn`, `adopt`, history capture,
  snapshots and state seeding (title and mouse modes), `resize`, `claim_size`,
  `detach`;
- `WindowLifecycle` (ADR-29) — the windows on the server: `check_available`,
  `ensure_ready`, `discover`, `create_window`, `locate`, `rename_windows`,
  `stamp_window`, `window_panes`, `set_pane_retention`, `pane_state`,
  `pane_path`, `pane_pid(s)`, `pane_ids`, `is_dead`, `kill`, `shutdown`;
- `PaneIo` (ADR-30) — a pane by its id, as a keyboard and a screen:
  `send_text`, `send_key`, `capture`, `text_before_cursor`;
- `StatusChannel` — hook status and the heartbeat
  ([status channel](../runtime/runtime-mux.md#status-channel-and-heartbeat)):
  `hook_signal_command`, `record_hook_state`, `hook_states`,
  `take_hook_state_events`, `ensure_heartbeat`, `heartbeat_running`,
  `stop_heartbeat`.

`Capabilities` holds the flags consumers branch on, each answered by the
adapter for its own multiplexer: `needs_liveness_poll`, `supports_snapshots`
and `keeps_exited_windows`. One value rather than three methods with defaults
scattered through the trait, so what an adapter can do is read in one place and
a stub declares all of it (`Capabilities::NONE` is the backend that promises
nothing). It is plain `Copy` flags because `Terminals::sync` reads it every
frame. The hook command is not among them: it names the server's socket, which
a host can learn after the backend is built, so it is a `StatusChannel` method
built when asked. A consumer holds `dyn SessionBackend`; one that needs a
single role can take just that trait.

A `backend_id` crossing the trait is the multiplexer's **pane id** (for example,
tmux's `%N`), not a backend's name. The backend a session belongs to is its
route (ADR-28).

**Vocabulary.** Each word names one thing, and a name built from it says which:

| Word | Means | Type |
|---|---|---|
| backend | whatever implements the contract for one route | `dyn SessionBackend` |
| adapter | a concrete backend for one multiplexer | `TmuxBackend`, `PsmuxBackend` |
| multiplexer (mux) | the program that keeps panes alive | `session::Multiplexer` |
| platform | the OS of the machine a multiplexer runs on | `session::Platform` |
| host | a machine other than this one, as configured | `session::HostDef` |
| launcher | how a command reaches a host: ssh, WSL, or nothing | `shell::HostLauncher` |
| transport | a launcher plus the multiplexer binary run through it | `TmuxTransport` |
| route | a machine plus a multiplexer; the registry's key | `session::Route` |
| pane id | the multiplexer's handle for one pane (tmux: `%N`) | `backend_id` |

"tmux" in a name means the tmux protocol (`tmux_compat`, `TmuxTransport`,
`TmuxCompatible`) or the tmux adapter, never "any multiplexer". The published
`tmux_socket` of `version --json` predates the vocabulary and keeps its name,
because changing it would break every reader of the JSON
(`docs/thurbox/config.md` → Relocating an instance).

**Key design decisions**:

- `spawn()` returns `(backend_id, output_reader, input_writer)`.
  The `Session` struct owns the reader/writer loops.
- `adopt()` reconnects to an existing session and returns initial
  screen content for parser seeding.
- `discover()` lists the sessions already on a server.
- `detach()` stops streaming without killing the session.
- `kill()` permanently destroys the session.

**Rejected**:

- *Async trait methods* — added complexity for no benefit since
  the tmux backend uses synchronous `Command::new("tmux")`.
  Can be added via `async-trait` if a future backend needs it.

---

## ADR-12: Local tmux as default backend

**Choice**: The default `SessionBackend` is the platform's multiplexer
(`Multiplexer::platform_default`) over the local transport
(`TmuxTransport::local()`): `TmuxBackend` under `local:tmux`, or
`PsmuxBackend` under `local:psmux` on native Windows (ADR-31). Either runs a
dedicated server
(`tmux -L thurbox`) with session name `thurbox`. All I/O goes
through tmux control mode (`-C`). The same backend reaches a host through a
transport ([hosts and transports](../runtime/runtime-mux.md#hosts-and-transports)).

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
Output arrives as `%output` notifications, input is sent via
`send-keys -H` (hex-encoded).

Only bytes below `0x20` and `\` are octal-escaped in `%output`; the rest
arrive raw, and tmux cuts a pane's output into lines wherever its read
ended, often inside a multi-byte character. So the reader takes a
`%output` payload as bytes and never decodes the line as text first:
decoded one line at a time, each half of a split character becomes
U+FFFD, which vt100 drops, and a non-ASCII word loses a letter
(`tests/lazy_terminals.rs`).

**Configuration on init**:

- `status off` — no tmux status bar (thurbox renders its own)
- `default-terminal xterm-256color` — standard terminal type
- `history-limit 5000` — reasonable scrollback
- `mouse on` — on tmux, lets programs inside panes detect mouse support and
  request wheel reports. Thurbox forwards those reports through control mode;
  programs that leave capture off still use normal-screen scrollback
- `extended-keys on` — enhanced key reporting
- `extended-keys-format csi-u` — the modern, unambiguous format some agents
  (e.g. `pi`) probe for at startup; thurbox injects keys via `send-keys` so this
  only sets the reported format, not the bytes agents receive. Best-effort: the
  option is tmux 3.5+ while thurbox's floor is 3.2, so a 3.2–3.4 host silently
  skips it
- `window-size manual` — each window sizes independently of the smallest
  attached client. Said **per window, as it is born** (`birth_options`), never
  server-wide: tmux asks a window's size before the window exists, and a
  server-wide `manual` dereferences a NULL window there and takes the server
  down (measured, tmux 3.5a). Which of several attached thurbox instances
  sizes a window is ADR-27
- `pause-after 5` — flow control (auto-resumed by reader)

`remain-on-exit` is **not** set here: it is a window option whose right value
depends on what the window is for, so each window states its own at spawn
(`keeps_dead_pane`). An agent's window keeps its dead pane — its liveness is
read from a listing (`#{pane_dead}`), and the corpse holds the error it printed.
A companion shell's and a plugin's program's do not: those are read from their
pane's output stream, and tmux announces a pane's death only by closing its
window, so a kept window is an ending that is never announced.

`off` is also in `WINDOW_OPTS`, which is not a duplicate of the per-window
setting but the *birth* value: the user's `~/.tmux.conf` is read on thurbox's
socket too, and a global `remain-on-exit on` there would have every window born
keeping its corpse — including a program that dies in the round trip between
`new-window` and its own option being set. The one role that wants a corpse
asks for it; nothing inherits one. A window found again on restart is
normalised where it is looked up by name (`find_program_window`), which is one
round trip and knows the answer from the name it searched for — the generic
adopt path stays free of it, since that path also carries every agent pane on
every ssh host.

**Window naming**: `tb-<session-name>` prefix for discovery. The prefix is not
identity, because names are not unique. A caller that shares a server with
other windows resolves a session's window through the stamp `WindowIndex`
reads off the window itself rather than the name or a remembered pane id; see
ADR-25. The `runtime-mux` program is the one exception: nothing else makes
windows on its socket, so a key's escaped name is its identity
(`protocol::name_of`).

**Which socket**: `thurbox` (`thurbox-dev` for a dev build) for an instance
running out of the default data dir, and `thurbox-<digest of that dir>` for one
`THURBOX_DATA_DIR` has relocated (`runtime_mux::instance::socket_for`). The data
dir is the anchor because it is what makes an instance its own: a relocated
instance — a sandbox, a test — has no business creating windows on the
operator's server. A relocated *config* dir alone changes nothing.
`THURBOX_SOCKET` overrides both. `version --json` reports the name in force —
never assume it (docs/thurbox/config.md → Relocating an instance). The
`runtime-mux` program runs Fleet's sessions on a socket of its own,
`runtime-mux` (`RUNTIME_MUX_SOCKET` names another), never the interface's.

**Output streaming**: `%output` notifications from control mode,
demultiplexed by pane ID into per-pane broadcast channels. Multiple
instances can simultaneously register the same pane; output is
broadcast to all registered channels (`PaneSendersMap`, one `PaneSender` per
reader). Each channel feeds a `ControlModeReader`, the pane's `PaneOutput`
consumed by `Session::reader_loop`. This allows multiple instances
to independently parse and render terminal state in real-time.

**A pane's stream is chunks, not bytes**: `PaneOutput` is an iterator
of `PaneChunk`s in stream order — output, a snapshot, a resync, a size, a hidden
pane's state — so whatever is not bytes reaches the reader loop as a value at
exactly the point of the stream it belongs to. It replaced a `Read` that
carried those as `Interrupted` errors recovered by downcasting, a channel any
`Read` adapter that retries interruptions would have emptied silently. An
adopted pane's history seed is `Output` chunks of at most 4096 bytes ahead of the
live stream, so replaying a long scrollback never holds the parser for all of it.

**A full channel is caught up, not left corrupt**: the reader thread
never blocks, so output that finds a pane's channel full is dropped, and the
bytes after a gap cannot be parsed onto that grid — the gap may have cut an
escape sequence. A reader that lost a chunk is therefore *behind*: it is handed
no more output and the reader thread asks for one snapshot of the pane, spliced
into the stream where its answer arrives exactly as ADR-P27's restore is. It
reaches the reader as `PaneChunk::Resync`, which replaces even a resident grid,
and output flows again behind it. When the channel is still full at that point
the snapshot is parked for the reader to take once it has drained the channel —
nothing newer can be queued, since a reader behind is handed no output — and
anything handed to it meanwhile makes the parked one stale and asks again. A
snapshot that fails puts the reader back in step. A server that cannot snapshot
a pane (psmux) keeps dropping the chunk.

Pinned end to end in `tests/tui_e2e.rs` on the remote stand-in, comparing the
painted pane with `capture-pane -e` cell for cell, colour and attributes
included. A flood straight through a local
multiplexer never overflows: tmux prints slower than the reader drains. What
overflows a channel is a link that stalls with the host's output in flight and
then delivers it in one burst, so that scenario spools the downlink while it is
wedged and requires the overflow in the log; without the resync it ends on a
damaged grid. Its twin keeps the stand-in's shallow link, where tmux is pushed
back on and holds the flood mid-print until the link heals.

**Input**: `send-keys -H <hex>` through the connection's shared outbox,
wrapped in a `ControlModeWriter` (implements `Write`), which encodes
the bytes the way its adapter's `PaneInput` says. The **psmux** adapter, whose
multiplexer has no `-H`, encodes the byte stream from the primitives psmux does
support, and sends a **paste** out of control mode entirely through psmux's own
`send-paste` — see [psmux divergences from tmux](../runtime/runtime-mux.md#psmux-divergences-from-tmux)
and `PsmuxPaste` (`crates/runtime-mux/src/psmux.rs`).

**Command synchronization**: tmux answers every command with a
`%begin`/`%end` block, and the reader hands blocks to waiters strictly in
the order the lines were written. So **every** line written down the
connection takes a place in the waiter queue, whether or not anyone reads
its answer: a command nobody waits for keeps its place with the receiver
dropped (`ControlMode::send_command_detached`, and `write_unanswered` for
keystrokes and the reader thread's own resume of a paused pane). A line
written without one hands its answer to whichever command was queued next,
and every later answer arrives one command off until the queue runs dry: a
listing answered with a keystroke's empty block reads as a host holding no
windows while its link is fine.

**One writer per connection**: only the connection's writer thread
writes to its stdin. A sender takes its place in the waiter queue and hands the
line to that thread under one short lock (the outbox), so lines go out in queue
order and nothing that sends — the paint path's resize and snapshot requests,
the reader thread resuming a paused pane — waits on the pipe. Over a stalled ssh
link a write blocks; when it did so under a lock, every other sender queued
behind it, and the reader thread stopped reading output at the next `%pause`.
A write that fails ends the connection's input, and each waiter whose line had
not been written is handed that error: a broken pipe means the command never
went out, which is the one case `Server::ctrl_command_list` sends again. A
pane's own keystrokes reach its `ControlModeWriter` on a thread per pane
(`Session::spawn_writer`), not a tokio task, since a psmux paste still blocks
on a process of its own. That paste goes around the connection, so it first
waits until every line queued before it is written; otherwise it could reach the
pane ahead of keys typed earlier.

A command list (`a ; b ; c`, sent by `ControlMode::send_command_list`) answers
with one `%begin`/`%end` block per command it runs — each gets its own command
number, so nothing on the wire marks the blocks as belonging together (measured,
tmux 3.2 and 3.7c) — and an error drops the rest of the list. The waiter that
sent the list therefore records how many blocks it holds and keeps its queue
slot until that many have arrived or the first `%error` does, concatenating
their lines into one answer. The tmux adapter's headless
`create_window` relies on
this to fold `new-window` and its window options into one list without a later
command being answered by one of the list's own blocks.

**History on adopt**: On reconnect (`tmux_compat::Server::adopt`),
`capture-pane -e -p -J -S -<scrollback_lines>` seeds the fresh
vt100 parser with the pane's scrollback history **and** visible
screen (text + colors; `-J` rejoins wrapped lines so they re-wrap
at the new width). Without this seed the parser starts empty and
a session's history from before the connect cannot be scrolled in the UI —
the `%output` stream only carries bytes emitted after connect. A
forced resize then triggers SIGWINCH, causing the TUI application
to repaint its visible screen through the normal `%output` stream
— this delivers pixel-perfect rendering of the live region on top
of the seeded history. Seeding is best-effort: a failed capture
logs a warning and adoption proceeds with an empty seed.

The seed also carries the pane's **window title**, replayed ahead
of the history as an OSC 2 (`title_seed_bytes`). Agents use the
title as their activity line — Claude Code writes the task it is
on, and the session list renders it beside the name — and thurbox
reads it off the PTY, so an adopt that joins the stream mid-flight
showed nothing there until the agent next repainted it. tmux kept
the value (`#{pane_title}` *is* the last OSC the pane emitted), so
adopt reads it back and replays it through the same callback a
live title takes; nothing downstream learns a second way of being
told. A pane that never had one reads back as `#{host_short}` —
tmux's default, not an agent's line — and is suppressed. The query
is its own best-effort round trip rather than a chain onto the
capture: a mux that answers it differently (psmux is unverified
here) must lose the activity line, never the scrollback. The
attention notification (OSC 9/777) is deliberately **not**
restored — it is an event, not state.

The same query reads the pane's **mouse modes**
(`#{mouse_standard_flag}`, `#{mouse_button_flag}`, `#{mouse_all_flag}`,
`#{mouse_any_flag}`, `#{mouse_sgr_flag}`, `#{mouse_utf8_flag}`) and
replays them as DECSETs ahead of the title (`mouse_seed_bytes`).
Whether a wheel tick is forwarded to the app is read off thurbox's own
parser, and an app turns tracking on once, at startup — a repaint
redraws its cells, not its modes. Without the replay, a Codex adopted
by a later interface could not be scrolled at all: its alternate screen
keeps no scrollback for thurbox to scroll locally instead. A flag a
server does not know expands to nothing and reads as off; when only
`mouse_any_flag` is on, `?1000` is replayed, which is all the wheel needs.

**Rejected**:

- *`pipe-pane` + FIFO* — intermittent data loss from tmux bugs,
  `mkfifo`/`stdbuf`/`cat` in the data path, no flow control, a timing race
  on initial capture.
- *Screen/dtach* — less widely available, fewer features.

---

## ADR-15: Headless CLI as Separate Binary

**Choice**: Headless automation lives in a separate binary
(`thurbox-cli`). It exposes `automation`, `editor`, `config`, `version`,
`update`, `notify`, `perf`, `plugin`, `ui`, `schema` and `doctor` as
subcommands (`thurbox-cli --help`), printing JSON results. Sessions are not
among them: they are Fleet's, driven through Fleet's own command line
(ADR-41).

**Why**: A separate binary keeps scripting/automation out of the
TUI's event loop. The `cli` module never reaches a backend adapter
(`crates/architecture.toml`), so it can operate without a terminal UI.

**Rejected**:

- *Embedded in the TUI binary* — would force the TUI to multiplex
  a non-interactive command path alongside its crossterm event
  loop.
- *A long-running daemon* — adds operational complexity; the files the
  CLI shares with the interface already provide the coordination a
  one-shot CLI needs.

---

## ADR-23: The interface is a Lua plugin kernel

**Choice**: `thurbox` boots a Rust kernel that renders whatever Lua plugins it
finds under `ui/`. There is no built-in pane — the session list, the agent
terminal and the search strip are files a user can edit, move, turn off, delete or
replace.

**Why**: a pane built, styled, keybound and tested in Rust makes the binary the
bottleneck on the interface's evolution, and leaves a user who wants a different
pane no move short of a fork. Making panes data moves that cost to a file, and
the constraint that makes it safe is that a plugin is handed a snapshot and
returns a tree — it never gets the world.

Five rules carry it, each load-bearing:

1. **Four node kinds** — `text`, `box`, `input`, `surface`; everything else
   composes in Lua.
2. **Layout resolves before render**, so a plugin knows its own rect and can wrap,
   truncate and window. Sizes are declared *statically*, which breaks the
   circularity.
3. **Snapshot-read, command-write** — Lua never blocks, so no plugin, including
   one nobody has written, can stall the loop on Fleet, git or a dead host.
4. **Capabilities by absence** — an ungranted capability is not in the
   environment. Enforced statically by `thurbox.yml` as well as at runtime.
5. **Anything touching the world runs on a worker.**

**Floating focus stays plugin-defined.** The topmost open float can consume the
native focus cycle through the bounded `on_focus_cycle("next"|"previous")` hook.
The kernel routes the direction and preserves pane focus when consumed; Lua
owns target order, selection and wrapping. Declining retains the ordinary pane
cycle. This extends the plugin API without giving the kernel a card or row model;
quit, reload and recovery keys remain reserved. See `docs/thurbox/plugins.md`.

**One model, one loop.** `App` and its state are declared in `main.rs`; its
methods live in `src/coordinator/`, split by purpose (the loop and its workers,
commands, publish, draw, input, mouse, focus, interface, paste, boot, chrome).
The split is by file only: the invariants that matter (a frame settles, focus
never rests on a hidden pane, a command's result reaches the pane that asked)
hold across those groups, so `App` is not split into several models. The
coordinator has its own entry in thurbox's module rules in
`crates/architecture.toml`.

**Cost, accepted**: a frame is more expensive — every pane is a Lua call returning a
table that is converted and painted — so the loop settles aggressively and every
cached answer carries an age. Code review and the info panel are panes outside
the binary:
[`thurbox-code-review`](https://github.com/Thurbeen/thurbox-code-review) and
[`thurbox-info-panel`](https://github.com/Thurbeen/thurbox-info-panel), each its
own repository, installed by clone.

**Rejected**:

- *A config file describing panes* — expressive enough for arrangement, never for
  behaviour; every new interaction would have become a new key.
- *Splitting `App` into several models or loops* — every group reads the same
  selection and focus, so the coupling would move into the parameters.
- *An embedded scripting language with the host's capabilities* — the point of
  rule 4 is that a plugin someone else wrote is safe to load.

---

## ADR-25: A window's identity is a stamp on the window, not its name

**Choice**: every thurbox tmux window carries two window options written at
spawn — `@thurbox_session`, the id of the session that owns it, and
`@thurbox_role` (`agent` / `shell` / `program`). `discover`'s format string
reads both, `WindowIndex` (`backend::identity`) indexes a listing by
`(session id, role)`, and every lookup asks it rather than resolving
`tb-<name>`. The answer is three-valued: **at** a pane, **absent**, or
**unknown** — several windows answer to the name and at least one carries no
stamp. Nothing may read `unknown` as absence.

**Where it lives**: the rule is backend-neutral, so it is not the tmux
adapter's. The role vocabulary (`WindowRole`) and the listing a backend
answers with (`DiscoveredSession`) are contract values
(`backend::contract`); thurbox's window-naming convention (`tb-` / `tbs-` /
`tbp-` and `sanitize_window_name`) and the resolution rule (`WindowIndex`,
`Located`) are
`backend::identity`, which depends on the contract and nothing else. How a
stamp is *stored* is an adapter's business: tmux keeps it in the two window
options above, and a backend with no window options leaves every window
unstamped and resolvable by name. The prefixes and both option names are what
running windows carry, and changing either would orphan them. The rule cannot
live inside the tmux adapter: that would make the contract and the adapter
import each other, a cycle `arch-rules` fails on.

**Why**: a window's name is neither unique nor injective. Two sessions may be
given one, and `sanitize_window_name` maps everything outside `[A-Za-z0-9_-]`
to `_`, so `fleet 1` and `fleet_1` share `tb-fleet_1`. A remembered pane id is
no better: tmux reissues pane ids from `%0` whenever its server starts, so a
remembered `%N` can be a live namesake's pane afterwards, and checking that
the pane sits in a window of the right *name* confirms nothing when two
sessions share the name. A teardown keyed on either could kill a live
session's window. A window option is the same fact stored once, on the thing
it describes.

**Rejected**:

- *A pane-id tie-break inside an ambiguous name* — "the remembered `%1` is
  one of the two `tb-fleet` panes, so take it". This is exactly
  the unsound step the reissued-id bug walks through, and keeping it would
  have made the strict path a special case rather than the rule.
- *Renaming windows to something unique.* The window name is what a person
  reads in `tmux ls` and what a hand-run `tmux attach` reaches for; a uuid
  there is a worse tool for a fix that does not need it.

**Consequences**: a remembered pane id is a hint — it is what the interface
attaches to, not what a teardown targets, and no code resolves ownership
through it. `discover` lists `tbs-` and `tbp-` windows too: leaving them out
lets a name look unambiguous when it is not, and the *role*, not the listing,
is what keeps a plugin's program from ever resolving as somebody's agent.
Ambiguity refuses: a session whose window cannot be resolved is left alone
rather than given a second one. An unstamped window is adoptable only while it
is the only one with that name, and it is stamped on adoption. psmux
([divergences](../runtime/runtime-mux.md#psmux-divergences-from-tmux)) keeps `@` options in one server-global map and answers `#{@...}` with it for
every window, so the stamp is withheld at both ends there and a lone namesake
resolves to its listed pane. Local recovery can also select the sole live pane
among exited namesakes from a fresh listing. Multiple live namesakes stay
unknown locally; remote duplicates stay unknown even when a remembered pane
currently has the expected name, since a restarted server can reissue that id
to another namesake. psmux can print an `ERROR:` diagnostic on stdout while
exiting zero, so its listing replies reject those lines rather than treating
an error as an empty server. Captured terminal text is not parsed for
diagnostics; a program may legitimately print the same prefix. Because the
stamp answers for a *role*, a session owns a `tb-` and a `tbs-` window, and a
teardown takes both down through the stamp.

**One session, one window per role is enforced, not merely assumed.** The
three-valued answer above only works while that holds: two racing spawns can
each stamp a window with one id, and `unknown` is permanent because the
windows stay. A second window carrying a stamp is retired where the stamp is written.
`Server::retire_duplicate_windows` (`backend::tmux_compat::server`) runs after
every stamp (the headless `create_window`, whose stamp rides in
`new-window`'s own command list, `stamp_window`, an adopt, and every window
created on a host) and **the highest window id keeps the identity**. On a host
it lists and kills over the control connection where one is open, so the
repair costs no ssh handshake of its own, and one-shot otherwise. Not "the
window I just made": both racers run the sweep, so "mine wins" has
each retire the other's and can leave the session no window at all, while a
key tmux issues in order and never reissues makes every sweep reach the same
verdict. It also closes the gap between a window being created and being
stamped — the last stamp to land is followed by a listing that sees every
earlier one. Liveness is deliberately not the key: it changes between two
listings taken a moment apart, and a newest window whose pane already exited
is kept and stays on screen under `remain-on-exit`, which is how the operator
sees why it exited.

The same sweep runs from the tmux adapter's `locate` before it gives up on an
`unknown`, and
that is what reaches a server **already** carrying a pair. `WindowIndex`
itself is untouched: `stamped_match`
still refuses two, because the repair is a *write* and reading one of two as the
answer is the unsound step the pane-id tie-break was rejected for above. Both
run wherever the server keeps window options, a host's as well as this
machine's; psmux keeps `@` options in one server-global map, so the
stamp is withheld at both ends there and a sweep that believed a
listing would retire every window on the server.

---

## ADR-27: Several instances on one server — the one being typed into sizes a pane

**Choice**: a pane is the size of the rect **one** instance paints it into, and
every other instance shows that pane's screen as it is. The window names its
sizer in a window option, `@thurbox_sizer`, and a paint's resize
(`tmux_compat::Server::resize`) is honoured only for a window that is this instance's to
size: one nobody names, one it already names, or any window while it is the only
client attached. Input and focus are what hand the size over — a keystroke,
paste or forwarded click into a pane that is not at this instance's size claims
it outright (`claim_size`), which is tmux's own `window-size latest` with typing
as the activity, and so does the pane **gaining** the focus here
(`Terminals::focus`), whether by a focus key, a click or a session switch. Only
the gain counts: a pane that keeps the focus while another instance takes it
stays that instance's, or two instances focused on one pane would trade it every
frame. Every instance's vt100 grid
follows the pane's **real** size, read from `%layout-change` and delivered to
the pane's reader in the same channel as its output (`PaneChunk::Resized`,
ADR-12), so the size
changes between the last byte written for the old one and the first written for
the new. An instance whose rect differs from the grid paints the bottom rows of
a taller grid and blank margins around a smaller one, and says on its bottom row
that another thurbox is sizing the pane and that typing takes it. When the other
instance goes, the one left takes its own size back once, unprompted.

The decision is **tmux's**, in the command list that carries the resize, so it
costs no round trip and two instances cannot both win it: a `set-option -F`
settles the name (`#{?<may>,<me>,<current>}`), then each resize is an
`if-shell -F` on "the name is me". That shape is fixed on purpose. The control
connection pairs each waiter with a known number of `%begin` blocks, and an
`if-shell` answers with one more block per command it runs — four taken, one
declined for a two-command body (measured, tmux 3.7c). So each `if-shell` wraps
one command and has a one-command `else`: five blocks whichever way it goes.
An inner command that fails does **not** stop the list the way a failing
top-level one does, so the sizes are clamped to what `resize-window` accepts.

Who is sizing is read over a format subscription on
`#{?#{==:#{session_attached},1},,#{@thurbox_sizer}}` — the name while more
than one client is attached, nothing once one is alone. That is what clears the
hint and triggers the take-back when an instance quits or crashes: the
interface has no per-pane teardown at quit to release a name from, and a crashed instance could
not run one anyway, so the release is computed rather than sent.

**Why**: with two instances on one server — a lead over ssh and one locally, in
terminals of different sizes — each resized every pane to its own rect whenever
that rect changed, including a toast taking a row. The agent re-wrapped at
whichever painted last, and the other instance kept parsing its output into a
grid of its own, different size. Measured on a sandboxed server with two
instances of 100×30 and 160×45: twelve SIGWINCHes in twelve seconds of
alternating rect changes, the agent bouncing between 26×73 and 41×118. After:
six, all from the sizing instance's own rect changes — what a lone instance
would get — and none from the other.

**Rejected alternatives**:

- **Negotiate the minimum** (each instance publishes its viewport, all set the
  smallest). Deterministic, but every instance gets the smallest screen all the
  time, and a published viewport needs pruning when its instance dies without
  saying so — the same staleness problem, with a worse steady state.
- **A fixed size for shared sessions.** Simple, and the agent never re-wraps,
  but it changes a lone instance's behaviour and asks the operator to pick a
  number that is wrong for one of their terminals.
- **First attached owns it.** Never hands over: the instance you are actually
  working in stays letterboxed for as long as the other one lives.
- **Keep the grid at the rect and resize nothing.** The grid then parses output
  laid out for another width — the garbling this replaces.

**Consequences**: the instance not being typed into shows a cropped or
letterboxed view, and switching which one you type into re-wraps the agent once.
Two instances restarted together find a name left by an instance that is gone,
and neither is alone, so the pane stays at that size until one of them is typed
into. "Alone" counts every client attached to the session, so a plain
`tmux attach` on thurbox's socket makes a lone instance wait for input the same
way. A lone instance's grid takes a new size when tmux reports it (a round
trip later) rather than when it asked. That frame shows the old grid in the new
rect; the bytes that follow are laid out for the new size, which is when the
grid needs it. psmux has no `if-shell -F`, no format subscriptions and no
`%layout-change` to read, so on a Windows host the last instance to paint wins.

## ADR-28: A session's route is one typed value, read one way

**Choice**: a session's route is parsed by exactly one function,
`session::Route::parse`, into a **machine** (local, `ssh:<host>`,
`wsl:<distro>`) and a **multiplexer**; `Route::format` is its only writer.
Every route names its multiplexer (`local:<mux>`, `ssh:<host>:<mux>`,
`wsl:<distro>:<mux>`), and any other key is refused. The grammar is in
`docs/thurbox/config.md` → *Multiplexer choice and existing sessions*. A
**host key** (`ssh:<host>`, `wsl:<distro>`), which the host picker carries,
names a machine only: `HostRegistry::resolve` finds its host, and
`HostDef::default_route` completes it with the multiplexer the host's entry
names, tmux by default. One resolver finds a route's host,
`HostRegistry::host_of`, and returns it exactly as configured. The backend
registry is keyed by `Route` and looks up only the route it is asked for;
`backend::wiring` registers every adapter for this machine and for every host
(ADR-31), and the platform default and a host's preference decide only which
multiplexer a host key means and which backend is the default.

**Why**: a route read with prefix predicates, `split_once(':')` or string
equality at each site lets two lookups disagree. A route that leaves its
multiplexer unsaid changes meaning when a host's preference changes, so one
session could be attached through tmux and torn down through rmux. And the
multiplexer is not the platform: a route naming `psmux` must not make a Linux
host look like Windows.

**Rejected alternatives**:

- **Routes without a multiplexer.** The preference can change after the route
  was written, and the route then names another server.
- **Allow `:` in host names and disambiguate against the registry.** The same
  key would then mean different things depending on which hosts load. The
  config load refuses such an entry instead.

**Consequences**: per-backend state in the kernel is keyed by the route, and a
backend is named by the route it serves. Which multiplexer is available is
decided by registration, never by the OS. A route naming a multiplexer no
adapter implements is refused by name. Only what drives the multiplexer is
told the route's
multiplexer: the backend `wiring` registers for the route, built with the
host's own platform ([a host's platform](../runtime/runtime-mux.md#a-hosts-platform)).

---

## ADR-29: One backend registry per process; lifecycle goes through the route's backend

**Choice**: each composition root builds the `BackendRegistry` once and hands
it down: `coordinator::boot` for the interface (`backend::wiring::configured`,
to `Terminals::with_registry`), and the `runtime-mux` program's `main` for the
runtime protocol (`wiring::local_only`, to `protocol::serve`). Every verb asks
it for the backend its route names and acts through the trait: `create_window`
opens a stamped, detached window; `locate` places an owner's agent and shell
windows from one listing; `kill`, `pane_pid` and `stamp_window` work with or
without an attached connection; `rename_windows` follows a rename. A route no
backend is registered for is refused. Quit calls `shutdown_all`.

**Why**: a verb that builds a fresh adapter for itself names that adapter, and
a second backend could not own a session without editing it. A registry
rebuilt per call can differ from the one the process was wired with. And a
route the registry does not serve must not fall through to the local server.

**Rejected alternatives**:

- **A headless method family beside the trait** (`*_headless`, or `(id,
  name)`-keyed copies of pane verbs): two vocabularies for one window. The
  headless verbs take an `Owner` or a pane, like the attached ones.
- **A cached global registry**: invisible to tests, and hidden state.
- **A test-only switch in the binary that loads a probe backend**: a
  production escape hatch. An in-memory backend passes the contract suite
  `TmuxBackend` passes (`crates/runtime-mux/tests/support/backend_contract.rs`)
  instead.
- **Silent defaults for the lifecycle methods**: a default that does nothing
  lets a stub compile and misbehave. Each backend says what `stamp_window`,
  `window_panes`, `set_pane_retention` and `shutdown` do.

**Consequences**: RMUX and any future Herdr adapter own a session's lifecycle by
registering for its route in `wiring`; no consumer changes. The interface's
registry is built from the `hosts.toml` of its start, so a host added later is
served once the interface restarts. A host is served for every multiplexer an
adapter implements (ADR-28). Where
the interface holds a connection to a backend, a lifecycle kill goes through
it (reconnecting once on a dead link) rather than one-shot; either way it
kills the pane's whole window, so a window somebody split leaves nothing
running. Pane I/O is ADR-30.

## ADR-30: Pane I/O is located by the key, then addressed by pane

**Choice**: every verb that types into or reads a session's pane — the
runtime protocol's `input` and `capture`, and `inspect`, `stop` and `attach`
beside them — locates the key's agent window first (`protocol::locate`:
`locate(Owner)`, stamp first, a lone unstamped namesake second), and then
calls a pane-keyed verb on the contract: `send_text`, `capture`, `pane_state`,
`kill`. A key crosses the contract as `backend::Key`, thurbox's own closed set
of spellings; the adapter says it in its grammar.

**Why**: a pane verb that resolves a name on whichever server it happens to
reach types into a window of the same name that nobody stamped, and a
session's prompt reads as another session's input. Locating once, through the
backend the route names, then addressing the pane, leaves one addressing
vocabulary.

**Rejected alternatives**:

- **`(id, name)`-keyed copies of the pane verbs on the trait**: a second
  addressing vocabulary beside the pane one every attached caller already
  holds. Locating is one step, done once.
- **Falling back to a name when the listing is ambiguous**: an `Unknown`
  placement is an error for every verb that writes to a pane, and a `start`
  that cannot tell whether its earlier process runs answers unknown rather
  than launching a second one. The tmux adapter's own settling
  (retiring a duplicated stamp, psmux's unstamped windows resolved from a listing,
  ADR-25) happens inside `locate`, where it is that backend's to decide.
- **Default methods**: the six verbs are required; a stub refuses them.

**Consequences**: a pane verb on a route nothing serves is refused by name
(`no backend here serves …`). `pane_state` reads the foreground process's argv
with a local `ps` only for a local pane, and an answer that names another pane
than the one asked about — `display-message` answers for the current pane
against a target it cannot resolve — is no answer.
The single-pane PID lookup validates the returned pane or window identity too,
both headlessly and through control mode, so a missing target never borrows
the current pane's process ID. Real tmux and RMUX regressions cover live and
missing exact-name targets in both modes.

---

## ADR-31: tmux and psmux are peer adapters over one tmux-protocol server

**Choice**: `backend::tmux`, `backend::psmux`, and `backend::rmux` are peer adapters; none
names another. What all three speak — the tmux command grammar, control mode, the
session config, discovery, the headless spawn — is
`backend::tmux_compat::server::Server<M>`, generic over a `TmuxCompatible`
multiplexer `M`; each adapter is its multiplexer's answers to that trait: what
its server can do (`WINDOW_OPTIONS`, `WINDOW_EVENTS`, `SNAPSHOTS`, …), how it
quotes, how a window's command and environment reach it, how keystrokes and a
paste are typed (`PaneInput`), what its control-mode connection may expect
(`ControlPolicy`), what its own "nothing there" replies are
(`answered_absent`), and its version floor. The shared code asks what a server
can do and never which multiplexer it is: every command it builds is quoted
through `M::quote`, and its error text names `M::MULTIPLEXER`. tmux's own
answers are the trait defaults — POSIX quoting, environment flags, window
commands, `-H` keystrokes and bracketed paste, the hook command and listing,
the banner's version word and the absence replies — so RMUX overrides only its
words and psmux, with a different tokenizer and launch grammar, what differs. `ControlPolicy::flow_control_command`
provides an optional startup command to limit buffered output;
`PANE_MONITORING` gates
`refresh-client -A` when a server streams attached panes on its own, and
`COMMAND_LIST_SINGLE_REPLY` selects the number of reply blocks expected for
one semicolon-separated list. `backend::wiring` builds the
registry from one table, `(Multiplexer, AdapterFactory)`, where a factory is
`fn(&BackendSpec) -> Arc<dyn SessionBackend>` and a `BackendSpec` is the
route, the launcher, the platform and the host. Every adapter is registered
for this machine and for every host, whatever either's OS or preference; the
platform default (`Multiplexer::default_for`) and a host's preference decide
only which multiplexer a host key means and which backend is the default.

**Why**: a branch on the binary's *name*, or on the build OS, cannot tell a
local tmux on Windows or a local psmux elsewhere from the default, and no rule
can tell a psmux decision from a tmux one unless each is visible as a
reference. A third tmux-compatible multiplexer must be one module and one table
row.

**Rejected alternatives**:

- **Flags on one backend** (a `MuxDialect` of `const bool`s read by one
  `TmuxBackend`): keeps "psmux = tmux + an OS", and a quirk is still a branch
  in code the other multiplexer runs.
- **Duplicating the server in each adapter**: some three thousand lines that
  are the same protocol, which would drift.
- **Each adapter implementing `SessionBackend` by delegating to the shared
  server**: forty forwarding methods per adapter, saying nothing the trait
  does not; the multiplexer's own bodies are already the adapter's.
- **Registering a local adapter only on its "own" OS**: the gate this
  replaces. A binary that is not installed is reported, by name, when its
  backend is first asked to start (`preflight::Dependency::Multiplexer`).

**Consequences**: runtime-mux's restrictions in `crates/architecture.toml`
hold the shape by resolved references: no adapter reaches another, and the
helper reaches neither, test code included; each adapter's code names only its
own `Multiplexer` variant; and the factory names no multiplexer without an
adapter. `backend::wiring`'s selection matrix
registers probe adapters for all four multiplexers and checks each route
reaches its own from a POSIX and a Windows thurbox, locally, over ssh to a host
of either platform and in a WSL distro, built from the placement's platform
and launcher, with a launcher that adds nothing to the probe's command line.
Which registered multiplexers are native to a platform and installed there is
`Multiplexer::available_on`. Every backend applies its config before it spawns
or attaches.

RMUX extends this arrangement as a third adapter. Its control-mode policy
skips `pause-after`, pane monitoring, and format subscriptions, and treats a
command list as one reply, matching RMUX 0.10.0. Boundary markers split that
one reply into the three parts of a pane snapshot; detached resizes reserve one
reply too. RMUX polls pane liveness and remote hook options because its control
stream does not complete a killed pane's reader and refuses `refresh-client -B`.
Local creation, TUI attachment, and TUI-open pane deletion have been driven
live on Linux with RMUX 0.10.0; macOS, SSH, WSL, and native Windows remain
unverified. `Rmux::check_banner` rejects versions below 0.10.0; the server
reports a tmux compatibility version, so it cannot supply the RMUX floor.
The [0.10.0 release](https://github.com/Helvesec/rmux/releases/tag/v0.10.0)
changed the daemon wire protocol from 5 to 8 and rejects 0.9.x daemons.
See [config.md](config.md#multiplexer-requirements-and-rmux-setup) for
installation, selection, and the verified version table.

## ADR-33: A running interface owns its local control endpoint

**Choice**: each TUI has a random instance ID and a local socket or named pipe,
separate from the session backend. Discovery is scoped to the data profile and
probes each endpoint before offering it as a target. With several reachable
interfaces, a caller must select an ID. The transport worker bounds request
size, clients, queue depth and wait time, then hands typed requests to the
coordinator. Only the coordinator reads UI state or invokes Lua, and it replies
after applying or refusing the request.
An endpoint failure leaves the interface running with a startup notice; startup
notices appear in turn on their own timer so other status messages cannot hide
later warnings. An active error keeps its full display interval before the next
startup notice appears.
Discovery prunes records confirmed dead, keeping repeated scans bounded
by currently reachable interfaces and the records left since the last scan.

**Why**: several interfaces can display the same sessions while each owns its
own focus and search state, and a request has to say which screen it changes.
Routing through a backend would confuse a session's persistent terminal with
the ephemeral interface drawing it.

**What it serves**: `session.focus` checks the target's snapshot and selects the
session there; `search.open` sets a query through the loaded search plugin and
is idempotent. `ui state` reports focus, selection and query. Lua action handlers
may receive a read-only typed argument table as a second parameter; existing
one-parameter handlers continue to work. The runtime action catalog combines
kernel shortcuts with loaded plugin bindings and commands. The same descriptors
drive the palette, `ui actions`, and `schema`; local action requests validate
arguments and availability before invoking the owner. Addressed `ui input`
handles active modal and plugin text, keys and scroll without forwarding terminal
bytes. Destructive external actions receive one-use, instance-bound tickets;
the second request revalidates the target and records an owner-only audit
decision before dispatch. The receipt and action event report acceptance;
the worker's command status reports its eventual outcome. Menu choices pass
their row target as a typed argument to the owning
action. No visible terminal capture is part of this API.

## ADR-34: A tmux server is found again before it is replaced

**Choice**: a tmux server's socket lives under thurbox's data dir
(`TMUX_TMPDIR=<data dir>/tmux`, created `0700`) on this machine and on a host,
where the host's `sh` resolves the directory; the `-L` name (ADR-12) is
unchanged. A `TMUX_TMPDIR` already set wins. Before a backend reads a server as
absent — on its first command, when a listing comes back empty, when control
mode cannot reconnect, and before any `new-session` — it runs one script where
the server runs: a server answering in the new directory or in tmux's default
one is adopted; failing that, a live server bound to a missing socket path in
either is sent `SIGUSR1` (tmux's documented way to recreate a removed socket)
and adopted. A path where a socket file exists is never signalled. A server
that is there but out of reach — it did not answer `SIGUSR1`, or is bound to a
socket file that does not answer — is an error through discovery and bootstrap,
never an absence. Only when nothing is found is a server started, in the new
directory. A host's data dir too deep for a socket path falls back to tmux's
default, tested by the host's `sh` the same way for every command. The directory a
server was found in is kept for as long as it runs, so a server an earlier
build left in `/tmp` stays in use until it exits. Which process is bound to a
path is asked of the kernel (`/proc/net/unix` and `/proc/<pid>/fd`, else
`lsof`), not read off a command line.

**Why**: macOS sweeps old entries out of `/private/tmp`, and a unix socket's
access time is not bumped by `connect()`. One Mac's server lost its socket 3
days 18 hours after it started, while in daily use. `has-session` then failed,
`new-session` started a second server under the same name, and every session
was resumed into it. Four days later the first server still held 93 processes
and 3.1 GB of resident memory: 22 agents, 13 of them duplicates of sessions
running in the new server and 9 of sessions since deleted, since a deletion
reached only the server thurbox could see. Moving the socket keeps it from
being swept; finding the server first is what keeps any other loss from
becoming a second server. Neither half alone does both: a socket can still be
removed from anywhere, and recovery cannot help once a second server owns the
path — tmux unlinks the path before binding, so signalling the first would
orphan the second. For the same reason a server that did not answer is not
replaced: its `SIGUSR1` is still pending, and would take the path from a second
server the moment it resumed.

**Limits**: psmux has no socket directory and is untouched; RMUX is unmeasured
and keeps tmux's default. A socket moved to a directory thurbox does not look
in is still a server it cannot reach. The orphan left by an older build next to
a live server is reported in the log and documented (`docs/thurbox/config.md` → An
orphaned tmux server); killing it is the only remedy.

## ADR-38: The interface's diagnostic logs are bridged to OTLP, opt-in

**Choice**: with `export_diagnostic_logs` on (and `[telemetry] enabled`), the
interface adds an `opentelemetry-appender-tracing` layer to its `tracing`
subscriber, beside the file appender, sending through a batch log processor
to the same collector (`telemetry::diagnostic`). The layer has a filter of its
own: WARN and above, from thurbox's own targets only. The file log keeps its
`thurbox=debug` level. Off by default and
restart-only. `thurbox-cli` never builds the layer.

**Why**: the interface is the one long-running process, so a batch processor
has time to export, from a thread of its own, without holding up the loop.
A short-lived `thurbox-cli` command would pay a network round-trip per call,
and a timeout per call while the collector is down. WARN and above, because
the file's level is far too much to ship. Thurbox's own
targets only, because the exporter's HTTP stack logs through `tracing` too and
would otherwise feed every export into the next. The settings are loaded under
the file log alone before the global subscriber is installed: whether the
bridge exists depends on them, and a global subscriber cannot be replaced.

**Limits**: plain HTTP and `http/protobuf` only: no TLS stack is in the tree,
so an `https` collector is reached through a local Collector. Another protocol
builds no bridge and logs why to the file. The batch is flushed on a clean
exit; an exit by signal or panic loses what the processor still held, at most
about a second of records, which the file log has. A clean exit with the
collector unreachable waits out the request timeout once. Records carry what
the log lines carry — paths, session names, host names — which is why the
bridge is opt-in. A change to the switch, or to the endpoint, protocol or
headers while it is on, reads as restart-only.

## ADR-39: Errors keep their classification across operation boundaries

**Decision:** `SessionBackend` returns `BackendResult<T>` with `BackendError`
(`Absent`, `Unreachable`, `Refused`, `Busy`, `Io`). The transport and adapter
classify raw process status and protocol replies once. Reports may render an
error as text, but decisions match the type. A message containing "not found"
is never evidence of absence.

`thurbox-cli`'s `CommandError` carries its exit code: 1 general failure, 2
usage, 3 ambiguity. Its JSON adds `error_kind` beside `error`.

**Why:** a decision read off an error's wording breaks the day the wording
changes, and a sentence that happens to contain "not found" can authorize a
teardown it should not. A typed absence is evidence; a sentence is only a
diagnosis.

## ADR-40: The plugin API is declared once in Rust, and versioned

**Decision:** `kernel::lua_api` declares every field `LuaHost::publish` and
`LuaHost::enter` serve on `thurbox`, with its luals type and one line of
documentation. The published region of `ui/lib/thurbox.d.lua`, the `thurbox.*`
entries of `thurbox.yml` and `tests/fixtures/lua_std/reads.lua` are rendered
from it by `just lua-api`, between `BEGIN GENERATED`/`END GENERATED` markers.
`tests/lua_api.rs` fails when a file differs from the rendering, and when a
plugin walking a fully populated publish meets a key the schema does not
declare. `thurbox.api` publishes `lua_api::API`, an integer that moves only
when a field is removed or changes meaning; a plugin's `requires_api` above it
is a load error naming the file. The first field added under it is
`session.meta`, a session's `ui.*` labels, which a Fleet session leaves empty.

**Why:** the same shape was written by hand in four places, nothing compared
them, and a table published without its fields passed CI. Plugins saw
only the application version, while a field could be renamed in a minor
release and an edited bundled pane is kept forever, so the rename broke it with
no message. Generating the declarations from the publish side's own list makes
an undeclared field a test failure, and the contract number gives a plugin
something to state rather than a release to guess.

**Limits:** the schema covers what is published, not node props, `ctx`, the
event payloads or command verbs, which `thurbox.d.lua` still keeps by hand. A
load stays all-or-nothing, so a plugin needing a newer API fails the reload as
any broken pane does, and the inventory names it. A meta value is stripped of
the characters that would act on the terminal, reorder the row or hide text,
though not of everything unusual, and a key carrying one is not published; it
is untrusted text, which a pane renders as text.

## ADR-41: Fleet is thurbox's engine; sessions run on runtimes

The design is [the documentation overview](../README.md).

**Choice**: thurbox is an interface over Fleet. Every session is a Fleet
Task; thurbox keeps no session state and no database, and reaches Fleet
only through its CLI (`fleet watch`, `apply`, `send`, `delete`, `attach`). The
snapshot reads the sessions from a `SnapshotSource` (Fleet's Tasks through
`fleet watch`), and the command bus runs commands through an `Executor`
(Fleet's command line, [`[fleets]`](config.md#fleets--the-fleets-that-manage-sessions)).
Fleet reaches sessions only through a process protocol (`start`, `inspect`,
`stop`, and optionally `input`, `capture`, `exec`, `attach`) that any runtime
implements: a multiplexer, a container, a VM. The first is `runtime-mux`, with
no knowledge of Fleet, agents or the interface. The three live in this
repository, and `crates/architecture.toml` declares the dependencies between
them: Fleet and the runtime link nothing of each other or of thurbox, and
thurbox links `runtime-mux` to attach its panes until Fleet drives them.

**Why**: one owner of the session facts means one copy of them. A process
protocol keeps Fleet usable without thurbox and makes a new runtime one script.
One repository keeps a change across components in one change and one CI.

**Rejected alternatives**:

- *A runtime inside the interface's binary*: Fleet would still need thurbox.
- *One runtime per multiplexer*: tmux, psmux and rmux share one protocol
  server (ADR-31); splitting them duplicates it, and which one a host runs is
  a fact of the place, which belongs to the runtime.
- *A multiplexer runtime written inside Fleet*: rewrites psmux, WSL and
  ssh handling that `runtime-mux` already has.
- *Separate repositories*: equal isolation once contracts are process-only, but
  every change across components would span repositories. Still open later, since
  nothing links across.
- *Sessions of thurbox's own beside Fleet's*: a second engine.

## ADR-42: Hosts belong to runtimes

**Status**: in progress. The code lands in steps. The runtime protocol's
version 2 has landed: each request names a place, and `manifest` reports
places. Fleet's config declares places, and its `[hosts]` table is gone.
Until each other step lands, thurbox's `hosts.toml` works as documented
today.

**Choice**: a host is a place of a runtime.

- A host's id is `<runtime>/<place>`, such as `mux/devbox`. `local` is the
  short form of `<runtime>/local` on the default runtime: the one marked
  `default = true`, or the only runtime. Fleet refuses a config with several
  runtimes and no single default.
- Fleet's config declares each place inline, in
  `[runtimes.<name>.places.<place>]`. Fleet passes the place's table to the
  runtime in every request. Fleet does not read the table. The runtime
  trusts it, so it lives only in the operator's Fleet config.
- Fleet's `[hosts]` table goes. A runtime's `manifest` reports its places and
  whether each one is ready.
- thurbox reads hosts only from Fleet. thurbox stops reading `hosts.toml`.
  Its companion shell on a remote place comes from the runtime, through Fleet.
- Nothing installs silently. `fleet doctor` reports what each place is
  missing and the command that fixes it.

The protocol additions are in
[the runtime protocol](../runtime/README.md#places-protocol-version-2),
and the config shape is in
[Fleet's configuration](../fleet/configuration.md#runtimes-and-their-places).

**Why**: how to reach a place is a fact of the runtime (ADR-41). A host table
in Fleet and another in thurbox are two copies of one fact. One Fleet config
keeps all of a user's settings in one file. A place config passed with each
request lets a runtime keep no config file. A runtime for ephemeral VMs then
needs no new verb: its place is dynamic, and a key gets one VM.

**Rejected alternatives**:

- *A place file of the runtime's own* (`places.toml`): a second file to edit,
  and Fleet cannot validate or show it.
- *Bare place names, unique across runtimes*: two runtimes with a `local`
  place collide.
- *Keep `[hosts]` as aliases*: two names for one place.
- *One `[runtimes.<name>]` per host*: one program configured many times.
- *Install the multiplexer or harness on first use*: a silent change on a
  machine the user did not ask to change.
