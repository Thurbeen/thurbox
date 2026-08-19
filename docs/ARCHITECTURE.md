# Architecture Decisions

Each decision follows a mini-ADR format:
**Choice**, **Why**, **Rejected alternatives**.

---

## ADR-1: The Elm Architecture (TEA)

> **Superseded by ADR-23.** This described v1's interface, which was retired when
> the plugin kernel took the `thurbox` binary name. The reasoning below is why the
> kernel keeps a single source of truth and one direction of data flow — reads are
> snapshots, writes are commands — rather than letting each pane own state. v1 is
> maintained on the `1.x` branch.

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

> **Superseded by ADR-23.** Breakpoints are no longer compiled in: `ui/layout.lua`
> decides the arrangement and can branch on width however it likes, and the kernel
> resolves rects before calling any plugin. The tiers below are what the shipped
> `layout.lua` still does by default, so they remain the behaviour a user sees.

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

**Choice**: All persistent state (sessions, worktrees,
automations) is stored in a single SQLite
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
`SQLITE_BUSY` error or an unbounded freeze) plus the WAL-friendly
performance pragmas `synchronous = NORMAL`, `cache_size`, `mmap_size`,
and `temp_store = MEMORY` (`storage::schema::initialize`; rationale in
`docs/PERFORMANCE.md` ADR-P6). The append-only
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

**Choice**: The default `SessionBackend` is `TmuxBackend`
parameterized over its `Local` transport (`TmuxTransport::Local`)
and registered as `local-tmux`, using a dedicated tmux server
(`tmux -L thurbox`) with session name `thurbox`. All I/O goes
through tmux control mode (`-C`). (The transport abstraction that
also enables remote SSH backends is ADR-13; here the choice is
simply that the out-of-the-box backend runs tmux locally.)

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
- `extended-keys-format csi-u` — the modern, unambiguous format some agents
  (e.g. `pi`) probe for at startup; thurbox injects keys via `send-keys` so this
  only sets the reported format, not the bytes agents receive. Best-effort: the
  option is tmux 3.3+ while thurbox's floor is 3.2, so a 3.2 host silently skips it
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
stdin, wrapped in a `ControlModeWriter` (implements `Write`). On a
**psmux** backend, which has no `-H`, the same writer encodes the byte
stream from the primitives psmux does support, and a **paste** leaves
control mode entirely for psmux's own `send-paste` — see "psmux divergences
from tmux" under ADR-13 below, and `control_mode::PsmuxPaste`.

**Command synchronization**: All commands that precede a
`send_command` (waited) call must themselves be waited. A
fire-and-forget (`send_command_nowait`) leaves an unclaimed
`%begin`/`%end` response in the stream that can steal the next
waiter. `send_command_nowait` is only safe when nothing follows
(e.g., `detach`) or when issued from the reader thread itself
(e.g., pause resume).

**Session restore**: On reconnect (`TmuxBackend::adopt`),
`capture-pane -e -p -J -S -<scrollback_lines>` seeds the fresh
vt100 parser with the pane's scrollback history **and** visible
screen (text + colors; `-J` rejoins wrapped lines so they re-wrap
at the new width). Without this seed the parser starts empty and
a session's pre-restart history cannot be scrolled in the UI —
the `%output` stream only carries bytes emitted after connect. A
forced resize then triggers SIGWINCH, causing the TUI application
to repaint its visible screen through the normal `%output` stream
— this delivers pixel-perfect rendering of the live region on top
of the seeded history. Seeding is best-effort: a failed capture
logs a warning and adoption proceeds with an empty seed.

**Rejected**:

- *`pipe-pane` + FIFO (previous)* — intermittent data loss from
  tmux bugs #641/#2989, required `mkfifo`/`stdbuf`/`cat` in the
  data path, no flow control, timing race on initial capture.
- *Screen/dtach* — less widely available, fewer features.

---

## ADR-13: Off-local sessions via an SSH / WSL tmux transport

**Choice**: Run agent sessions on a remote host (over SSH) or in a
local WSL distro (via `wsl.exe`) by launching the same tmux
control-mode protocol behind a launch prefix. `LocalTmuxBackend` is
generalized into `TmuxBackend { transport, socket, session, name }`
where `transport: TmuxTransport` is `Local` (a bare
`Command::new("tmux")`), `Ssh { destination, ssh_opts, mux }`
(`ssh <dest> <mux> …`), or `Wsl { distro, mux }`
(`wsl.exe -d <distro> <mux> …`). `mux` is the host multiplexer binary
(`tmux` by default, or `psmux` for a Windows SSH host; a WSL distro
runs `tmux`). The transport's *only* job is to build the `Command`;
everything downstream — the control-mode reader/writer threads, pane
registration, `send-keys`/`%output` — is byte-for-byte identical
(`control_mode.rs` was already transport-agnostic). The SSH and WSL
arms share `TmuxTransport::prefixed`, since both join + shell-interpret
the trailing POSIX-quoted tokens identically; only the launcher prefix
differs.

Hosts are declared as data in `~/.config/thurbox/hosts.toml`
(`session::HostDef { kind: HostKind {Ssh, Wsl}, … }`/`HostRegistry`),
and WSL distros are additionally **auto-discovered** on Windows
(`agent::host_config::discover_wsl_hosts` via `wsl.exe -l -q`). The
combined set is loaded by `agent::host_config::load_all`, each
registered as a backend named `ssh:<host>` / `wsl:<distro>` via
`TmuxBackend::from_host`.

**Why WSL = "SSH without the ssh"**: `wsl.exe` runs `tmux`, `git`, the
agent, and the worktrees all *inside* the distro at native Linux paths,
so there's no Windows↔Linux path translation (`wslpath`) and the
worktree layout matches the SSH path exactly. Modeling WSL as a host
kind (rather than a per-session "run in WSL" flag wrapping a native
psmux pane) reuses the entire remote-host subsystem — picker,
persistence/restore, `git::*_on`, headless `--host` — for free.

**Why** (general): The local-vs-off-local difference is exactly one
line (how the tmux process is launched). The per-session control
commands travel over the stdin pipe, not the launcher argv, so only the
one-time `attach-session` launch crosses the boundary. SSH relies on
the system `ssh` binary + `~/.ssh/config` for auth/keys/multiplexing;
WSL needs no credentials at all.

**Key design decisions**:

- **Lazy registration**: off-local backends are registered but *not*
  connected at startup (`check_available`/`ensure_ready` deferred to
  first use), so a down host (or slow WSL discovery) never blocks the
  TUI. `App::select_backend` only resolves the backend from the
  registry; the blocking `ensure_backend_ready` runs on the spawn
  worker, never on the UI thread (ADR-P12).
- **Auto-discovery**: WSL distros appear with zero config; an explicit
  `kind = "wsl"` entry of the same name wins (for overrides like
  `worktrees_dir`). `discover_wsl_hosts` decodes `wsl.exe`'s UTF-16LE
  output and is a no-op off Windows / without `wsl.exe`.
- **Selection**: `SessionConfig.backend` (`ssh:<host>` / `wsl:<distro>`
  or `None`); `is_remote_backend` covers both. The TUI shows a host
  picker as the first new-session step (skipped when none configured/
  discovered); `thurbox-cli session create --host` is the headless
  equivalent.
- **Persistence/restore**: `backend_type` round-trips in SQLite;
  restore discovers windows **per backend** so off-local sessions
  re-adopt against their own host's tmux.
- **Off-local worktrees**: `git::*_on(host, …)` run git via
  `git::host_launcher` (`ssh …` or `wsl.exe …`). Worktree paths resolve
  under the host's `worktrees_dir` (or `$HOME/.local/share/thurbox/…`
  resolved + cached, keyed by backend name since a WSL host has no
  `destination`).

**Module placement**: `HostDef`/`HostRegistry`/`HostKind` live in
`session/` (the dependency sink) so both `agent` (builds the backend)
and `git` (runs git on the host) can depend on them without violating
the module-isolation rules.

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

### psmux divergences from tmux

The control-mode protocol is byte-identical over either transport, but the
**psmux** binary diverges from tmux in three places (all verified against psmux
3.3.6, each branched on `TmuxTransport::uses_psmux()`). `CLAUDE.md` keeps a
summary; this is the reference to read before touching that path.

- **`send-keys -H`** is not implemented (it injects the hex digits as literal
  text). `send_keys_commands` rebuilds the same PTY byte stream from the
  primitives psmux does support (`send-keys -l` literal runs +
  `Enter`/`Tab`/`Escape`/`BSpace`/`C-<letter>` key-names); tmux (incl. a WSL
  distro's tmux) keeps the byte-exact `-H` path. Literal runs go out as
  `-l -N 1 "…"` (double-quoted, `\"`/`\\` escaped): `-N` makes psmux's
  send-coalescing decoder — which re-quotes with a POSIX `'\''` escape its own
  parser can't read back (`it's` → `it\s`) — bail to the direct handler, which
  reads double-quote framing correctly (`flush_psmux_literal`/`psmux_quote`).
  Quoting alone isn't enough: psmux classifies arguments *after* tokenizing, so
  it drops any starting with `-` as an unknown flag (a typed hyphen never
  arrived, issue #920) and rewrites a `0xNN`-shaped argument into the character
  it names. `psmux_literal_args` re-emits such a leading character *as* a
  `0xNN` argument — psmux decodes it back and, in literal mode, joins arguments
  with no separator, reassembling the run exactly. Probed by
  `scripts/dev/e2e/windows-vm.sh test` (probe D).
- **`new-window` trailing tokens are not joined** (psmux keeps only the first
  and drops the rest — the agent launched with **no args**) and **`new-window
  -e` is ignored** (on the argv path too — no `THURBOX_SESSION` identity).
  `TmuxBackend::psmux_window_powershell` folds env + command into **one token**
  of PowerShell (`Set-Item Env:K 'v'; & 'claude' '--session-id' …` — psmux runs
  it via `powershell -NoLogo -Command`, whose Win32 command line strips
  unescaped double quotes, hence PowerShell single-quoting throughout;
  backslash is literal in psmux's parser, so `C:\` paths survive). Control-mode
  spawns (`psmux_window_command`) frame it in double quotes (psmux's tokenizer
  concatenates adjacent `'…'` segments but passes `'` through `"…"` tokens); the
  headless local `spawn_window` passes it as a single argv arg. The local socket
  honors the `THURBOX_SOCKET` env override (`local_socket()`) so test/sandbox
  tooling can scope an instance on Windows, where every `-L <name>` resolves
  machine-wide (no `TMUX_TMPDIR`).
- **A paste cannot be key-encoded at all** (the encoding above emits ESC as its
  own `Escape` key-name, so the agent saw a bare Escape instead of the
  `ESC[200~` marker and took each embedded CR as Enter — a pasted stack trace
  submitted line by line). It goes **out of band** through psmux's own paste
  command (`control_mode::PsmuxPaste`, issue #916): psmux's control-mode
  dispatcher implements no paste command (`paste-buffer`/`set-buffer`/
  `send-paste` are CLI/server-only), so a bracketed-paste payload
  (`bracketed_paste_text` unwraps one; anything else keeps the key encoding)
  goes to the one-shot CLI `psmux send-paste -t <pane> <base64>` — the same
  command psmux's client uses for Ctrl+Shift+V, so CRLF is normalized for
  ConPTY, markers are written contiguously and **only** when the pane's app
  enabled bracketed paste. A failure falls back to the key encoding (degraded
  beats dropped). Base64 because a raw newline in a psmux command argument is
  cut by the server's line-oriented read, truncating the payload *and*
  executing its tail as a command (psmux #560) — the same reason the headless
  prompt path (`paste_prompt_args`, feeding `send_prompt_now`/
  `deferred_prompt_script`) sends `send-paste` where tmux gets
  `send-keys -l <ESC[200~…>`. Probed by `windows-vm.sh test` (probe C).

### A Windows host speaks PowerShell, not `sh`

psmux is the *multiplexer*; the divergence above is about its wire protocol.
Independent of it, `multiplexer = "psmux"` also declares that the **host is
native Windows** (`HostDef::is_windows` — the multiplexer is the proxy for the
platform, since a WSL distro runs `tmux` inside Linux), and a Windows host has
no POSIX shell at all. Every remote probe was `sh -c <script>`, which there
fails with PowerShell's `CommandNotFoundException` — so the repo picker could
not list a directory, classify a committed path, or import a folder of repos on
a Windows host.

- **One dispatch point, two dialects.** `git::host_probe(host, posix,
  windows)` picks `host_shell_c` or `host_powershell_c`, and each pair of
  scripts emits the **same line protocol** (`!missing`, `g <name>`/`d <name>`,
  `git`/`dir`/`missing`, one name per line) so every parser stays
  transport-neutral. A probe cannot become POSIX-only by omission.
- **`-EncodedCommand`, not `-Command`.** The script crosses two shells that both
  rewrite it: ssh space-joins its trailing args, and the host's default sshd
  shell — commonly PowerShell itself — expands `$…` inside double quotes. A
  probe reading `$PSVersionTable` came back with the *outer* shell's expansion
  (`System.Collections.Hashtable`) substituted in. UTF-16LE base64 is
  `[A-Za-z0-9+/=]`, so neither `cmd` nor PowerShell finds anything to
  interpret. Paths inside the script are PowerShell single-quoted
  (`powershell_quote`: only `'` is special, so `\` and `$` in a Windows path
  are literal).
- **There is no `$HOME`.** `echo $HOME` under `cmd`/PowerShell prints the string
  `$HOME` and exits 0, so the bogus value was accepted and every `~`-relative
  path became a literal `$HOME/…`. `git::remote_home` routes a Windows host to
  `%USERPROFILE%`; that choice lives there and nowhere else, because the one
  other copy of it (`spawn::resolve_launch_home`) was the only caller getting it
  right.

### A remote error has to name the failure

Two layers of transport noise sat in front of every error message a command
reported, both removed by `git::reportable_stderr` — which every helper in the
module reports through, local git included, because a `clone`/`fetch` runs over
ssh via `GIT_SSH_COMMAND` and carries the same advisory:

- **OpenSSH's post-quantum advisory.** OpenSSH ≥ 10 prints a three-line `**
  WARNING: connection is not using a post-quantum key exchange algorithm.` block
  on **stderr** for every connection to a server on an older OpenSSH — Windows
  hosts very much included. It is informational and the command still runs, but
  it is *first* in the buffer, so reporting stderr verbatim made every remote
  failure read as a key-exchange problem and pushed the real cause below the
  fold. Suppressing it at the source is not an option: `LogLevel=ERROR` would
  equally hide `Permission denied`, and `WarnWeakCrypto=no` is fatal on the
  older clients that never warn anyway — so the `**` lines are filtered from the
  *reported* text. When they are all there was, the exit status is reported
  instead (`describe_exit`), never the advisory again.
- **PowerShell's CLIXML stderr.** `powershell.exe` does not write error records
  as text when its stderr is redirected (which it always is here) — it writes a
  `#< CLIXML` document whose messages sit in `<S S="Error">` nodes with CRLF
  encoded as `_x000D__x000A_`, so a Windows failure arrived as `#< CLIXML <Objs
  Version=…`. `decode_clixml` strips the envelope whole and inlines the error
  text; a document carrying only a `progress` record (PowerShell's "Preparing
  modules for first use") decodes to nothing, so the exit status is reported
  rather than the markup. Raw text interleaved with an envelope — what
  `[Console]::Error.WriteLine` and any native command produce — is kept.

---

---

## ADR-7b: Multi-Instance Sync — SQLite with PRAGMA data_version

**Choice**: Multiple thurbox instances synchronize all state
(sessions, worktrees, automations)
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

## ADR-15: Headless CLI as Separate Binary

**Choice**: Headless automation lives in a separate binary
(`thurbox-cli`) that shares the same SQLite database as the TUI.
It exposes `session`, `automation`, `task`, `message`, `editor`,
`config`, `extension`, `version`, `update`, and `notify` management
as subcommands, printing JSON results.

**Why**: A separate binary keeps scripting/automation out of the
TUI's event loop. The TUI already polls `PRAGMA data_version`
on every tick (~10 ms event-loop cadence) (ADR-7b), so changes
made by `thurbox-cli` appear
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
`Theme` struct in `kernel::theme` (v1 kept it in `src/ui/theme.rs`).
Plugins receive **roles** rather than colours
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
codex, antigravity, opencode, aider, copilot, vibe, pi, omp) on first run via
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

## ADR-20: Agent-agnostic extensions in `extensions/`

**Choice**: Opt-in workflows that *compose* thurbox (rather than
extend the binary) live in `extensions/<name>/` as data + shell:
a plain-markdown behavior spec, portable scripts built on
`thurbox-cli` + `jq`, and a curl-able, idempotent `install.sh` —
the same distribution model as `scripts/install.sh` and
`packaging/`. The first extension is **flow** (an experimental
focus-protecting triage agent; see FEATURES.md). Extensions reach
agents only through `agents.toml` **aliases** (e.g. `flow-worker`)
that the user maps to any CLI, and surface their spec through
context-file symlinks (`CLAUDE.md`/`AGENTS.md`/`GEMINI.md` → the
spec), so no vendor is named anywhere.

**Why**: ADR-19's pivot made thurbox agent-neutral; an opinionated
LLM workflow (prompts, triage rubrics, tick cadences) would undo
that if baked into core, and it iterates on a much faster cadence
than the binary (editing a markdown spec vs. cutting a release).
Keeping extensions as data over the public surface (`thurbox-cli`
plus `agents.toml`) also makes that surface's stability a tested,
load-bearing contract.

**Rejected**:

- *Vendor plugin formats* (e.g. a Claude Code plugin) — couples
  the workflow to one agent's ecosystem; the same agent brain must
  be runnable by codex, antigravity, opencode, vibe, ….
- *A `thurbox-cli flow init` subcommand with embedded assets* —
  puts one opinionated workflow inside the agent-neutral core and
  ties spec iteration to the release cycle.
- *A separate repository* — the extension scripts against
  `thurbox-cli`'s JSON surface and should version and CI alongside
  it.

## ADR-21: Declarative extension manifests + first-class lifecycle

**Choice**: Extend ADR-20 by teaching the core a single declarative
**manifest format** (`extension.toml`, `session::ExtensionDef`) and a
first-class lifecycle on the public surface:
`thurbox-cli extension install/uninstall/activate/deactivate/list/status`
(`session_ops::*`, `agent::extension_config`). The manifest has an
*install* half (`home`, `[[agents]]`, `[[files]]`, `[[symlinks]]`) and a
*runtime* half (`[[sessions]]`, `[[automations]]`). `install` resolves a
source (a bare name → the official repo pinned to the binary's release
tag; a path; or an `http(s)://` base — fetched via `curl`/`wget`), lays
down the payload, registers agents (append-only, comment-preserving),
writes the home-resolved manifest to the discovery dir, and activates.
Active extensions are recorded in SQLite `metadata` and **self-healed**
(missing sessions/automations recreated) at TUI startup and on every
`automation tick`. The core still knows the *format*, never a specific
extension; flow's `install.sh` becomes a thin shim over the CLI.

**Why**: ADR-20 left each extension to reimplement bootstrap in bespoke
shell, and gave no way to recover from a half-removed extension. Folding
the mechanics behind one data-driven command makes install reproducible
and uninstall symmetric, and self-heal makes an active extension robust
against accidental deletion — all while staying extension-neutral
(reusing `spawn_session_headless`, `db.create_automation`, `AgentDef`).
Pinning the fetch to the binary's release tag keeps a fetched extension
in sync with the binary that reads it.

**Rejected**:

- *Embedding extension assets in the binary* (the option ADR-20
  rejected) — still rejected; `install` fetches **data** at runtime, it
  does not bake assets in, so the agent-neutral core is preserved.
- *Adding an HTTP client dependency* — `curl`/`wget` shell-out matches
  the existing installer and keeps the dependency tree small.
- *Re-serializing `agents.toml` to add/remove agents* — would drop user
  comments/formatting; the installer edits text (append on install,
  block-removal by name on uninstall) instead.

### Install and lifecycle mechanics

The capabilities that reach outside the extension home, the installer's
resolution order, the `extension` CLI surface, versioning/staleness, and the
self-heal pass. `CLAUDE.md` keeps a summary and points here.

Three install-spec capabilities exist for reaching **outside** the extension
home (added for the built-in hooks extension): `[[external_files]]` places
a file into an agent's own config dir (absolute / `~` / `{home}` path,
guarded by `requires_dir` so it's skipped when that agent isn't installed);
`[[agent_patches]]` appends args to an **existing** agent in
agents.toml (`apply_agent_patches` via `toml_edit`, reversible — uninstall
removes exactly the injected subsequence); and `[[config_merges]]`
**reversibly deep-merges** shipped JSON into an agent's own *shared* config
file (`{path, source, requires_dir}`) — for agents whose hooks live in a
file that would be clobbered by `[[external_files]]` (antigravity's
`settings.json`). The merge (`agent::json_merge`) recurses objects, unions
arrays by deep-equality, and leaves a user's conflicting value untouched;
uninstall **prunes by marker** (every shipped hook command contains
`thurbox-cli session signal`), so removal stays correct even after the
payload's schema changes across an update — no orphans. Writes are skipped
when unchanged (it re-runs every startup + heartbeat tick). All three are
honoured by `session_ops::install_extension` / `session_ops::uninstall_extension`.

`thurbox-cli extension install <name|url|dir> [--home <dir>] [--force]`
(`session_ops::install_extension`) is the one-command installer: it
resolves the source (`agent::extension_config::resolve_source` — a bare
name → the official source `official_base()/<name>` over curl/wget,
**pinned to the binary's release tag** (`main` for dev builds) so a
fetched extension matches the binary; a path → a local dir), fetches + lays
down the payload files (`executable`/`if_absent`/`substitute` flags; paths
validated against traversal — no absolute/`..`), creates the symlinks, registers
the agents (`ensure_agents_registered` appends to agents.toml, preserving
existing entries), writes the home-resolved manifest to the discovery dir, and
activates. A `substitute` file the user edited (managed marker removed) is not
clobbered on reinstall unless `--force`. A **bare-name** install that can't fetch
its manifest becomes a discovery error
(`agent::extension_config::unknown_extension_help`: names `OFFICIAL_EXTENSIONS`,
offers a Levenshtein "did you mean?", points at `extension available`).
`uninstall <name> [--purge]` reverses install: tear down session + automation,
remove the extension's agents (`remove_agents_from_toml`, text-edit to preserve
comments), delete the manifest, `--purge` also the home dir. `reinstall <name>
[--purge]` (`session_ops::reinstall_extension`) is the clean-slate hammer —
uninstall + fresh `install --force` from the recorded source (rewriting even
user-edited seed/`substitute` files) — heavier than `update --force`, which only
refreshes payload files in place. Flow's
`install.sh` is a thin shim over `install`.

`thurbox-cli extension` (alias `ext`) — `install` / `uninstall <name>
[--purge]` / `reinstall <name> [--purge]` / `list` / `available [<query>]`
(alias `search`) / `update [<name>] [--all] [--force]` (no name ⇒ all) /
`activate <name>` / `deactivate <name> [--force] [--purge]` / `status [<name>]`
— wraps `session_ops::extensions`: `ensure_extension` idempotently (re)creates
any missing declared resource (reusing `spawn_session_headless` +
`db.create_automation`, matching by name so existing ones are reused);
`activate_extension` also records the name in the SQLite `metadata`
`active_extensions` JSON set; `deactivate_extension` tears the resources
down and clears the set. The CLI layer arms the tmux automation heartbeat
on activate so a `Send` automation actually fires headlessly. `available`
lists the official extensions (`OFFICIAL_EXTENSIONS`) for discovery — offline,
with an `installed` flag and ready-to-run `install_command` per entry. Every
mutating subcommand's JSON carries a human-readable `summary` line (and
`list`/`status` surface each extension's `description`).

**Versioning + update.** A manifest declares its own `version` and a
`min_thurbox_version` (soft compat gate — install/activate/heal *warn*,
never block, if the binary is older). The installer stamps two provenance
fields into the discovery-dir copy: `installed_with` (the thurbox version that
installed it) and `source` (the resolved install target). After a thurbox upgrade
the on-disk copy is older than the binary, so `ExtensionDef::is_stale` flags it
(`extension list`/`status`, plus a self-heal nudge). With `[features] auto_update`
on (the same flag that self-updates the binary), the self-heal pass —
`heal_one_extension`, run on TUI startup **and** the headless `automation tick` —
goes past the nudge and **refreshes the stale extension in place** (calls
`update_extension`); the `is_stale` gate is local/network-free, so a refresh
fetches at most once per extension per binary version. `update_extension` re-runs
`install_extension` from the recorded `source` — a bare name re-resolves against
the *new* binary's release tag — preserving user-edited files unless `--force`;
`update_all_extensions` does every installed one. Version helpers
(`compare_versions`, `is_dev_version`, `is_stale`, `compat_warning`) are pure
functions in `session::extension_def`; dev builds (`0.0.0-dev`) skip
staleness/compat since their version doesn't order against tags. No
version-snapshot store: rollback = pin a tagged install URL or downgrade the
binary + `update`.

**Self-heal**: `session_ops::heal_active_extensions` re-ensures every active
extension, called at **TUI startup** (`main.rs`, before session restore so healed
sessions are adopted normally) and at the top of the headless **`automation
tick`** (`cli/automations.rs`, so healing works with the TUI closed via the
heartbeat keeper). Consequence: while an extension is active, deleting its
session/automation is a no-op — they're recreated (a startup toast says so);
`extension deactivate` is the real off-switch. Headless healing requires
`[features] automations = true` (the heartbeat); with it off, healing happens only
at TUI startup. The flow installer delegates its bootstrap to `extension activate
flow` (with an inline fallback for older thurbox).

---

## ADR-22: `App` decomposition — coordinator + per-domain sub-modules

> **Superseded by ADR-23.** `src/app/` was deleted with v1. The pressure it
> answered was real and recurs: the kernel's `main.rs` is the coordinator now, and
> the same rule applies — cohesive clusters move out, the coordinator stays
> `EXEMPT` in `tests/architecture_rules.rs`, and governance is directory-level.

**Choice**: Keep the single `App` model (ADR-1, TEA) but split its
~11.7k-line `app/mod.rs` into per-domain sub-files under `src/app/`,
relocating cohesive `impl App` method clusters out of `mod.rs` while the
state they own lives in small per-cluster sub-structs. `app` stays one
**EXEMPT** module in `tests/architecture_rules.rs` (the coordinator that
imports every layer), and governance is directory-level, so the new
`app/*.rs` files introduce **no** new cross-layer edges and need no
allowlist entries — the split is entirely intra-`app`.

Two halves:

- *State* — already mostly done: `task_ui: TaskUiState`, `automation_ui:
  AutomationUiState`, `new_session: NewSessionWizardState`,
  `global_search: GlobalSearchState`, `worktree_sync: WorktreeSyncState`,
  `metrics`, `notification_state`. Two remain to extract: a new
  `PointerState` (text-selection / click-target / scrollbar / hover
  registries) and a `SpawnController` holding **only** the
  background-task machinery (`worktree_create`/`session_spawn` + their
  `pending_*`).
- *Behavior* — relocate the method clusters into domain files:
  `app/tasks.rs`, `app/automation.rs`, finish `app/search.rs`,
  `app/mouse.rs`, `app/worktree_sync.rs` + `app/git_stats.rs`, and
  `app/spawn.rs`. Methods stay `impl App` (they coordinate side effects);
  only pure state/logic lands on the sub-structs.

**The spine stays on `App`** (clusters borrow it, never own it): the
session vector + selection cursor (`sessions`, `active_index`), the
backend registry (`backends`), per-session render views
(`session_terminal_views`), the render-loop flags (`needs_redraw`,
`last_draw_at`, `last_output_gen`), the status/order caches
(`cached_hook_states`/`hook_states_version`, `cached_session_order`,
`last_active_session_id`, `spinner_frame`), and
`metrics`/`db`/`session_counter`/`terminal_rows`. The TEA methods
(`update`, `tick`, `view`, `handle_key`/`dispatch_action`, `new`,
`shutdown`), session restore/adopt, and all navigation/status/ordering
stay too — navigation *is* manipulation of the shared cursor. Two
cross-cluster handoff slots stay explicit and `pub(crate)`:
`pending_task_prompt` (tasks↔spawn) and `deferred_inputs`
(spawn/sync/paste).

**The spawn boundary**: `SpawnController` owns only its background tasks
and exposes `poll() -> SpawnEvent` (`WorktreesReady`/`Spawned`/`Failed`);
`App` applies the event via the existing `finalize_spawned_session`. The
controller never owns session *adoption* — that body touches `sessions`,
`active_index`, `focus`, `db`, `deferred_inputs`, `metrics`, and
`task_ui` in one place, and pushing it into a sub-struct would re-create
the god-object through a `&mut App` parameter.

**Order** (each its own PR, green throughout; `app/acceptance.rs` is the
safety net): (1) tasks → (2) automations → (3) search — the safe
relocations, state already extracted — then (4) mouse (first new
sub-struct), (5) sync, (6) spawn (machinery only; last and hardest).
Because all relocations carve from the same `mod.rs`/`key_handlers.rs`,
they are **sequenced**, not run in parallel, so each rebases onto the
prior cleanly.

**Why**: `mod.rs` is the repo's hottest merge-conflict file and
interleaves spawn/mouse/task/automation/sync/metrics, so no single flow
can be read without scrolling past four others. The split shrinks
`mod.rs` toward a coordinator + spine (~5–6k lines) with each domain's
invariants local, and *strengthens* the TEA spirit — side effects stay
concentrated at the coordinator, pure state/logic gets isolated — rather
than bending it. The state half is already underway, so most of the work
is mechanical relocation against existing tests: low risk, high
readability gain.

**Rejected**:

- *Splitting `App` into multiple models / TEA loops* — breaks ADR-1's
  single `update`/`view` and the `data_version`-driven redraw; the
  coupling is real (every cluster reads the selection cursor), so one
  model with a borrowed spine is correct.
- *Owning the spine in sub-controllers* (e.g. a `SessionController`
  owning `sessions`/`active_index`) — every other cluster borrows it, so
  this merely relocates the god-object and forces `&mut App`-style
  params everywhere.
- *Pushing side-effecting methods onto the sub-structs* — would drag
  `db`/`sessions`/`deferred_inputs` into each cluster and reintroduce the
  coupling; behavior stays `impl App`, only pure logic moves.
- *One big relocation PR* — unreviewable and merge-hostile; the value is
  in independently-reviewable, test-green increments.

---

## ADR-23: The interface is a Lua plugin kernel

**Choice**: `thurbox` boots a Rust kernel that renders whatever Lua plugins it
finds under `ui/`. There is no built-in pane — the session list, the agent
terminal and the search strip are files a user can edit, move, turn off, delete or
replace. v1's `src/app` (TEA) and `src/ui` (35 render modules) were deleted;
`session`, `agent`, `storage`, `git`, `session_ops` and `cli` are unchanged.

**Why**: every surface v1 grew had to be built, styled, keybound and tested in
Rust, so the interface was the bottleneck on its own evolution and a user who
wanted a different pane had no move available short of a fork. Making panes data
moves that cost to a file, and the constraint that makes it safe is that a plugin
is handed a snapshot and returns a tree — it never gets the world.

Five rules carry it, each load-bearing:

1. **Four node kinds** — `text`, `box`, `input`, `surface`; everything else
   composes in Lua. A prior attempt froze its catalog at six and reached sixteen,
   because it never built the userland layer.
2. **Layout resolves before render**, so a plugin knows its own rect and can wrap,
   truncate and window. Sizes are declared *statically*, which breaks the
   circularity.
3. **Snapshot-read, command-write** — Lua never blocks, so no plugin, including
   one nobody has written, can stall the loop on SQLite, git or a dead host.
4. **Capabilities by absence** — an ungranted capability is not in the
   environment. Enforced statically by `thurbox.yml` as well as at runtime.
5. **Anything touching the world runs on a worker.**

**The name is the constraint on how this shipped.** The updater in an installed
binary hard-fails on a known binary missing from a release archive and swallows the
error, so an archive that dropped the name `thurbox` would silently end auto-update
for every install already out there, unfixably. The kernel therefore inherited the
name rather than shipping beside it, and a profile with v1 history meets a one-time
gate (`kernel::consent`) before anything changes. See `docs/RELEASING.md`.

**Cost, accepted**: a frame is more expensive — every pane is a Lua call returning a
table that is converted and painted — so the loop settles aggressively and every
cached answer carries an age. And v1 surfaces are owed rather than ported: code
review, the file viewer and the info panel have no equivalent, and tasks,
automations, the restore list and the perf HUD are `thurbox-cli` only. Tracked in
`openspec/changes/v2-parity-gaps/`.

**Rejected**:

- *A config file describing panes* — expressive enough for arrangement, never for
  behaviour; every new interaction would have become a new key.
- *Shipping v2 beside v1 as a second binary* — auto-update never introduces a new
  binary, so it would have reached almost nobody, and two interfaces on one
  database doubles the surface every engine change has to satisfy.
- *An embedded scripting language with the host's capabilities* — the point of
  rule 4 is that a plugin someone else wrote is safe to load.
