# Fleet as thurbox's engine

Status: accepted direction, not yet implemented. ADR-41 in
[ARCHITECTURE.md](ARCHITECTURE.md) records the decision; this file is the
design and the migration plan. It supersedes the split proposed in
Thurbeen/thurbox#1681 and reshapes the plan tracked in Thurbeen/fleet#401.

## Decision

1. **This repository becomes the one repository** for thurbox, Fleet and the
   multiplexer runtime. Fleet is imported here with its history. The stars,
   the issue tracker and the release channels (Homebrew, AUR, Chocolatey,
   winget) stay with `Thurbeen/thurbox`. Breaking Fleet's current install
   path is accepted: its use is small.
2. **thurbox requires Fleet.** Fleet is thurbox's orchestration core: every
   session is a Fleet Task. thurbox keeps no session state and no SQLite
   database.
3. **Fleet does not require thurbox.** Fleet reaches sessions only through a
   process protocol that any runtime can implement, down to a shell script.
4. **A runtime is the abstraction**, not a multiplexer. Fleet and thurbox
   know only "a runtime": a program that runs one keyed process somewhere.
   A multiplexer is one way to do that, a container or a VM another.
   `runtime-mux`, extracted from `src/backend`, is the first runtime and the
   one thurbox ships by default; it is not special.

## Components

```text
thurbox (interface) ──process: fleet watch / apply / send / delete / attach──▶ Fleet (engine)
   │                                                                             │
   │ starts the stream fleet attach names                                        │ process: runtime protocol
   ▼                                                                             ▼
 stream pane ◀──── terminal stream frames, whatever the runtime ──── a runtime: runtime-mux,
                                                                      runtime-container, runtime-vm, …
 coding agent ── fleet signal / fleet result ──▶ Fleet
```

| Component | Owns | Never owns |
| --- | --- | --- |
| Fleet (`crates/fleet-*`) | Tasks (one per session), the store, every decision, harness profiles (launch, resume, trust), agent hooks through `fleet signal`, workspaces and worktrees, the inbox, receipts, the runtime protocol and its conformance suite | Any runtime's code; any Rust dependency on a `runtime-*` crate or `thurbox` |
| A runtime (`crates/runtime-*`, or any program) | Processes by key on its places, its own place config (hosts, images, VM definitions), bounded `exec`, the attach command, whatever keeps a terminal alive there | Tasks, agents, journals, delivery, the interface |
| thurbox (`crates/thurbox`) | The TUI, the Lua kernel, rendering, stream panes, interface state in plain files (order, seen marks, bookmarks, review notes, settings, keybindings) | Sessions, SQLite, any runtime code, any Rust dependency on `fleet-*` or `runtime-*` |

Where a fact belongs:

- It must survive an interface restart, or a client other than this interface
  may need it: **Fleet**.
- It is how to reach a terminal or a host: **the runtime**.
- It is how this user looks at things: **thurbox**, in a file.

### Enforcement

One `architecture.toml` covers the whole workspace and `fleet-arch` checks it
on every build, the way it checks Fleet's crates today:

| Crate | May depend on | Must not depend on |
| --- | --- | --- |
| `fleet-*` | `fleet-*`, `term-stream`, crates.io | `runtime-*`, `thurbox` |
| each `runtime-*` | `term-stream`, crates.io | `fleet-*`, `thurbox`, every other `runtime-*` |
| `term-stream` | crates.io | everything else |
| `term-holder` | `term-stream`, crates.io | everything else |
| `thurbox` | `term-stream`, crates.io | `fleet-*`, `runtime-*`, `term-holder` |

Components talk only through processes; `term-stream` is a wire format, not
a component. A component can therefore later move
to a repository of its own with nothing to untangle.

## Contract 1: thurbox to Fleet

thurbox is a Fleet client and reads nothing of Fleet's store directly.

- `fleet watch --json`: a long-running NDJSON stream, one event per changed
  Task, resumable by the store's write counter (`--since`). It feeds the
  snapshot worker behind a `SnapshotSource` trait (today
  `src/kernel/snapshot/db.rs`).
- `fleet apply`, `delete`, `send`, `amend`: the interface's commands, behind
  an `Executor` trait (today `src/kernel/command/execute.rs`, which calls
  `session_ops` directly).
- `fleet attach <task> --json`: the command that opens the Task's terminal
  stream (contract 3).
- thurbox starts `fleet up` when no manager holds the lease.

An interactive session is a Task with no brief: it spawns and waits for its
user.

## Contract 2: Fleet to a runtime

A runtime is any program. Fleet runs it through `fleet-exec`, one bounded
process per call: `<program> <verb>`, a JSON request on stdin, and on stdout
`{"known": …}` or `{"unknown": {"why": …}}`. Unknown never concludes.

| Verb | Required | Answer |
| --- | --- | --- |
| `manifest` | yes | protocol version and the verbs supported |
| `start` | yes | adopts the one live process holding the key, else starts `argv` in `dir` with `env`, `dir` and `extra_dirs` visible to it at the same paths |
| `inspect` | yes | `None`, `Running{id}`, `Exited{id, code}` |
| `stop` | yes | a process already gone counts as stopped |
| `input` | no | types text, optionally submits |
| `capture` | no | screen text, cursor, foreground process |
| `exec` | no | a bounded command on the host (not inside the place), deadline and output cap; how Fleet makes workspaces off this machine |
| `attach` | no | `{stream: {argv}}` or `{tty: {argv}}`: how to open a terminal stream (contract 3) |

Every verb is idempotent by key. The runtime knows no harness, Task or
journal: pause is `stop`, resume and revive are `start` with the harness's
resume argv, idle comes from agent hooks, delivery is proven by the agent's
prompt hook.

Fleet config moves from one `[runtime]` to named `[runtimes.<name>]`, each a
`program`; a host names its runtime, so a Task's `spec.host` chooses it.

Agent hooks append to the file Fleet passes as `FLEET_SIGNAL_FILE` in `start`.
Fleet reads it directly on a local host and through `exec` on a remote one, so
a runtime needs nothing for signals.

`fleet runtime check -- <program>` is the conformance suite: two starts with
one key leave one process, a stop of an absent process succeeds, an error
answers Unknown and never None, a protocol mismatch answers Unknown. Every
runtime runs it in CI. `examples/runtimes/tmux.sh` is a reference runtime in
one script. A template repository for runtimes written elsewhere ships a
skeleton, the protocol version it targets and that CI.

### What a runtime is

A runtime answers one question: run this command, under this key, in this
directory, at this place, and tell me about it. Where and how is its own
business.

| Runtime | Place | What keeps the terminal | Today |
| --- | --- | --- | --- |
| `runtime-mux` | this machine, ssh hosts, WSL | tmux, psmux, rmux (ADR-31) | `src/backend` |
| `runtime-container` | docker or podman, local or remote | the container's pty, or a holder inside it | to write |
| `runtime-vm` | a VM or microVM | a holder inside the guest | to write |
| `runtime-process` | this machine, no terminal | none | the reference script's sibling |

A new multiplexer of the tmux family is a module in `runtime-mux`, not a
runtime. A new kind of place is a runtime. A runtime may reuse another inside
its place (a container running tmux to keep its screen), but no `runtime-*`
crate depends on another; shared code is a crates.io dependency.

Fleet must run a Task on the four required verbs alone, so a runtime without a
terminal is a full runtime:

- the brief reaches the agent at `start`, through the harness profile's argv
  or a file in the workspace, not only by typing;
- state reaches Fleet through `FLEET_SIGNAL_FILE`, not through the screen;
- the optional verbs add features and nothing more: without `input`,
  `fleet send` waits for the next start; without `capture`, no screen
  checks; without `attach`, no live view, and thurbox shows the Task without
  a terminal.

The default install configures `runtime-mux` and nothing else. With no
multiplexer on the machine, Fleet does not fall back to another runtime: the
first `start` answers the missing dependency by name, as `preflight` does
today, and other runtimes are opt-in in `[runtimes.<name>]`.

## Contract 3: the terminal stream

Attaching to a tmux window is the easy case: the server keeps the screen,
accepts several clients and arbitrates their sizes. A container or a VM has
none of that. `docker attach` replays nothing and shares one size between
every client; `docker exec -it` starts a new process instead of reaching the
agent; a VM offers a serial console or ssh. "Run this command in a pty" is
therefore not an interface: what a view gets would depend on the runtime.

What a view needs from a live session, whatever runs it:

1. the screen as it is now, on connect and after a dropped stretch;
2. the output that follows, in order, with size changes at their place in it;
3. a way to type, paste and resize;
4. to leave without stopping the process, and to come back;
5. other viewers at the same time, with one rule for who sizes the screen.

### The protocol

A stream is a process. The view starts it and speaks over its stdin and
stdout; it ends when either side closes. Frames are length-prefixed, a type
byte then a payload: output and input are raw bytes, every other frame a JSON
object. The vocabulary is thurbox's own `PaneChunk` (`src/backend/contract.rs`),
which already carries every item in the output's order on one channel:

| Direction | Frame | Payload |
| --- | --- | --- |
| down | `hello` | protocol version, capabilities (`snapshot`, `scrollback`, `viewers`, `resize`) |
| down | `snapshot` | cols, rows, cursor, normal-screen history and rows, alternate rows, styled |
| down | `output` | bytes |
| down | `resized` | rows, cols |
| down | `sized_elsewhere` | whether another viewer sizes the screen |
| down | `resync` | a snapshot replacing the view's grid after dropped output |
| down | `state` | title and input modes, to parse but not count as output |
| down | `exit` | the process's code, or Unknown |
| up | `input` | bytes, sent as typed |
| up | `paste` | text, bracketed when the program asked for it |
| up | `resize` | rows, cols: a request the holder of the screen arbitrates |
| up | `detach` | leave; the process keeps running |

`snapshot` comes right after `hello` when the capability is there, so a view
never starts on an empty grid. Sizing follows ADR-27 everywhere: the viewer
being typed into sizes the screen, the others get `sized_elsewhere`.

`term-stream` is a crate with only the frame types and their codec: no
process, no runtime, no transport. thurbox, Fleet and every `runtime-*` may
depend on it, as they may on `serde`.

### The runtime's `attach`

`attach` answers how to open a stream, in one of two shapes:

- `{"stream": {"argv": […]}}`: the command speaks the protocol. This is the
  full answer.
- `{"tty": {"argv": […]}}`: a raw interactive command, for a runtime that has
  nothing better (`docker attach`, `virsh console`, `kubectl attach -it`).

`fleet attach` always hands a view a stream. It passes a `stream` answer
through, and wraps a `tty` answer in `fleet term-bridge -- <argv>`, which runs
the command in a local pty, keeps a `vt100` screen of it and speaks the
protocol. A bridge only knows what it has seen since it started, so its first
snapshot is blank until the program redraws; it asks for one by resizing.
That is the price of a runtime without a screen keeper, and `hello` says so.

### Who keeps the screen

| Runtime | Screen keeper | `attach` answers |
| --- | --- | --- |
| `runtime-mux` | tmux, psmux, rmux | `stream`: `runtime-mux view <key>`, control mode encoded as frames |
| `runtime-container`, `runtime-vm` | `term-holder` inside the place | `stream`: `docker exec -i <ctr> term-holder connect <key>`, or the same over ssh or vsock |
| any runtime, minimal | none | `tty`: bridged by Fleet, no history |

`term-holder` is a small static binary a runtime puts in its place and runs
the agent under: `term-holder serve <key> -- <argv>`. It owns the pty, keeps
the screen and its scrollback, accepts several clients on a local socket and
arbitrates their sizes. `connect` relays one client over stdin and stdout, so
any byte pipe reaches it: `docker exec -i` without `-t`, ssh, vsock,
`kubectl exec -i`. The same binary answers `capture` and `input`
(`term-holder capture <key>`, `term-holder send <key>`), so those verbs are one
`exec` each in a container or a VM. It plays the part tmux plays for
`runtime-mux`, for places with no multiplexer. A runtime may still run tmux in
its place instead.

### What changes in thurbox

Less than it looks: the grid, `vt100`, rendering, size arbitration and the
performance gates already consume `PaneChunk`s, today from control mode
through `WiredPane`. They stay. What changes is the source:

- **One pane kind for every session**: a stream pane. It runs the argv from
  `fleet attach`, decodes frames into the `PaneChunk`s `WiredPane` already
  takes, and encodes keys, pastes and resizes as frames. Program panes stay
  as they are.
- **Gone from thurbox**: the control-mode client, the transports, the
  multiplexer dialects, discovery and pane location, all now
  `runtime-mux view`'s.
- **Disconnects** become one case: the stream ends, the pane shows it is
  detached, and thurbox asks `fleet attach` again with backoff. Today that is
  a different path per transport.
- **Capabilities** the interface reads from `hello`: no `scrollback` hides
  copy-mode history, no `viewers` warns before a second view, a bridged stream
  shows that its history starts now.
- **Cost**: one extra process and pipe between the multiplexer and the grid.
  Output frames are raw bytes, never base64; a snapshot is sent only on
  connect and resync. Step 5 measures it against PERFORMANCE.md's frame
  floors before anything is deleted.

A plain terminal uses the same stream: `fleet attach <task>` without `--json`
draws the snapshot, then passes output through and input back.

## Workspaces

A workspace is the files one session works in. Today two makers exist:
thurbox (`src/git/worktree.rs`, the symlink workspace of `src/workspace.rs`)
and Fleet (`[workspaces]`, otherwise the runtime). This design keeps one.

### Who and when

**Fleet makes every workspace; a runtime never does.** TaskController plans it
(Thurbeen/fleet#394, carried into the tracking issue):

```text
materialize ──▶ start ──▶ (stop / start … pause, resume, revive) ──▶ release ──▶ clean
   once            the workspace is untouched by every stop and start        once
```

`start` receives `dir` and `extra_dirs` ready, and makes nothing. A `{dir}`
workspace exists before the Task; nothing makes or cleans it.

### Where

The workspace is made on the Task's **host**: the machine the runtime runs its
places on, never inside an ephemeral place.

| Host | How Fleet runs the git commands |
| --- | --- |
| this machine | directly, through `fleet-exec` |
| any other | through the runtime's `exec`, which runs on the host (ssh host, WSL distro, the machine running docker, a persistent VM) |

So there is one workspace adapter, `git`, and two transports. A runtime needs
nothing for workspaces beyond `exec`, and one contract on `start`: **the
process sees `dir` and `extra_dirs` at the same paths.**

| Runtime | How the paths reach the process |
| --- | --- |
| `runtime-mux` | same filesystem |
| `runtime-container` | bind mounts at the same paths |
| `runtime-vm` | a shared folder (virtiofs), or the guest is the host and `exec` runs in it |

A container is recreated freely; its workspace survives on the host.

### Sources

A repository is named by identity (`url`) or, for one with no remote, by
`path` on the host. On the host, its source is:

1. a checkout mapped in config (`[hosts.<name>.sources]`, today
   `runtime.repositories`), used as it is;
2. otherwise a bare mirror Fleet keeps at `<root>/.sources/<digest>.git`,
   cloned once and fetched before each materialize, with the host's own
   credentials.

A repository with neither is `RepositoryUnmapped`, as today. thurbox's
new-session picker sends the identity of the directory the user picked: its
`origin` URL, else its path.

### Layout

```text
<root>/<key>/                 the session's directory for more than one repository
<root>/<key>/<name>/          one real worktree per repository
<root>/<key>/.fleet-workspace.json   written last; without it the workspace is partial
```

- One repository: the session runs in `<root>/<key>/<name>`.
- Several: it runs in `<root>/<key>`, where every worktree is a real
  subdirectory. thurbox's symlink workspace goes, and with it the symlink
  trust prompts it needed `--add-dir` for.
- A plain directory that is not a repository is an extra dir, given to the
  agent through the harness profile's `additional_dir_args`, never linked.
- The trust root is the primary repository's source, as today.

### Branches

Per repository the Task says `ref` (the base, default the remote's default
branch) and optionally `branch`:

| `branch` | Worktree |
| --- | --- |
| absent | a new branch `<prefix>/<task name>` from `ref` |
| a new name | a new branch of that name from `ref` |
| an existing branch | that branch checked out; one already checked out elsewhere is `NeedsAttention/BranchBusy` |

### Cleaning

`clean` removes the worktrees and the directory, never a branch: commits stay
in the source. A worktree with uncommitted changes is not removed;
the Task waits with `NeedsAttention/WorkspaceDirty` until a person commits,
discards or deletes with `--force`. Fleet today removes with `--force`; that
changes.

`spec.workspace.onDelete` is `clean` (the default) or `keep`. `keep` leaves the
files and forgets them. thurbox's soft delete is not a delete: it stops the
Task and sets `dispatch: false`, so restoring it is starting it again in the
same workspace.

### Reading a workspace from the interface

The diff and review views read files where the workspace is. thurbox never
reaches a host itself: it asks `fleet workspace exec <task> -- git …`, which
runs on the host through the same transport. On this machine that is a local
process.

## Where the SQLite tables go

| Table | Destination |
| --- | --- |
| `sessions`: identity, `backend_id`, `backend_type`, `key` | Task `status.session` |
| `sessions`: `launch_*`, `agent_args`, `agent_session_id` | Fleet harness profiles |
| `sessions`: `hook_state`, `hook_state_at` | `fleet signal`, then `status.sessionState` |
| `sessions`: `stopped_at`, `lost_at`, `teardown_owed`, `relaunch_held_at` | Task conditions, receipts, finalizers |
| `sessions`: `parent_session_id` | a parent label on the Task |
| `sessions`: `display_order`, `seen_at` | thurbox interface file |
| `worktrees` | Fleet's workspace port (see Workspaces) |
| `session_meta`, `session_events`, `session_relaunches`, `session_intents`, `session_loss_suspects`, `session_lifecycle_clock` | dropped: Fleet's receipts and Known/Unknown observation |
| `session_messages` | `status.inbox`, `fleet send` |
| `tasks` | Tasks with `dispatch: false` |
| `automations`, `automation_runs`, `automation_suspensions` | a Fleet schedule kind, to design |
| `event_cursors`, `snapshot_versions` | `fleet watch --since` |
| `audit_log`, `telemetry_export` | Fleet's store history and `fleet-telemetry` |
| `repo_bookmarks`, `review_comments`, `review_marks`, settings, keybindings | thurbox interface files |

`rusqlite` then leaves the dependency tree.

## Migration

Each step ships alone and starts with its end-to-end test.

1. **Workspace.** Turn this repository into a Cargo workspace, the current
   code as `crates/thurbox`. No behaviour change.
2. **Extract `runtime-mux`.** Move all of `src/backend`, control mode and
   the tmux/psmux/rmux dialects included, into `crates/runtime-mux` with
   `config::hosts`, `agent::host_path` and `agent::preflight`; inject what it
   reads from `session::settings`. Add the `architecture.toml` rule. thurbox
   keeps linking it only until step 5 gives it the `view` process.
3. **Import Fleet** with its history under `crates/fleet-*`, its docs under
   `docs/fleet/`. Pin one toolchain (Fleet's); editions may differ per crate.
   One CI with path filters; component-scoped tags (`thurbox-v*`, `fleet-v*`,
   `runtime-mux-v*`) with cocogitto's monorepo mode. Transfer Fleet's open issues.
4. **Runtime protocol.** `runtime-mux` binary; Fleet's generic `exec` runtime
   kind, `[runtimes.<name>]`, `fleet runtime check`, the reference script.
   Fleet's P1 to P4 (workspaces, `fleet signal`, harness profiles, delivery by
   hook) land here if not before.
5. **Views.** `fleet watch`, `fleet attach`, interactive Tasks without a brief;
   `term-stream`, `runtime-mux view`, `fleet term-bridge`; thurbox's stream
   pane beside the control-mode pane, behind a setting, until it measures as
   fast. `term-holder` and `runtime-container` follow.
6. **thurbox on Fleet.** `SnapshotSource` and `Executor` traits, then their
   Fleet implementations; thurbox starts `fleet up`.
7. **Migrate.** `thurbox-cli migrate fleet`: one Task per live session,
   adopting its window by stamp and its worktree by path; `agents.toml` into
   harness profiles. Automations move once the schedule kind exists.
8. **Delete.** `session_ops`, `storage`, the engine half of `thurbox-cli`, and
   Fleet's thurbox session adapter.
9. **Release.** thurbox's archive and packages carry `fleet` and
   `runtime-mux`; Fleet keeps a standalone archive. Fleet's old install URL
   and `FLEET_MAJOR` guard point here or are retired.

## Open

- A schedule kind in Fleet to replace automations.
- Fork and `restart --fresh`: a conversation field on the Task, or dropped.
- `hooks.toml` beyond `pre_launch`: harness settings or a Fleet plugin.
- Shared hosts (ADR-24): one Fleet store per operator is assumed.
- The tmux server `runtime-mux`'s windows live on: its own socket, since the
  interface no longer connects to it.
- Latency of the `view` bridge against today's in-process control mode, to
  measure in step 5 against PERFORMANCE.md's frame floors.
- Windows (psmux) at the cutover, or after Linux and macOS.
- VEGA stays a separate starter repository; Fleetlens moves under
  `extensions/`.
