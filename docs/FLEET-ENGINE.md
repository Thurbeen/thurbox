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
   │ runs the attach argv in a pty                                               │ process: runtime protocol
   ▼                                                                             ▼
 terminal pane (pty) ◀── one live terminal, whatever the runtime ──── a runtime: runtime-mux,
                                                                      runtime-container, runtime-vm, …
 coding agent ── fleet signal / fleet result ──▶ Fleet
```

| Component | Owns | Never owns |
| --- | --- | --- |
| Fleet (`crates/fleet-*`) | Tasks (one per session), the store, every decision, harness profiles (launch, resume, trust), agent hooks through `fleet signal`, workspaces and worktrees, the inbox, receipts, the runtime protocol and its conformance suite | Any runtime's code; any Rust dependency on a `runtime-*` crate or `thurbox` |
| A runtime (`crates/runtime-*`, or any program) | Processes by key on its places, its own place config (hosts, images, VM definitions), bounded `exec`, the attach command, whatever keeps a terminal alive there | Tasks, agents, journals, delivery, the interface |
| thurbox (`crates/thurbox`) | The TUI, the Lua kernel, rendering, pty panes, interface state in plain files (order, seen marks, bookmarks, review notes, settings, keybindings) | Sessions, SQLite, any runtime code, any Rust dependency on `fleet-*` or `runtime-*` |

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
| `fleet-*` | `fleet-*`, crates.io | `runtime-*`, `thurbox` |
| each `runtime-*` | crates.io | `fleet-*`, `thurbox`, every other `runtime-*` |
| `thurbox` | crates.io | `fleet-*`, `runtime-*` |

Components talk only through processes. A component can therefore later move
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
- `fleet attach <task> --json`: forwards the runtime's attach answer.
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
| `start` | yes | adopts the one live process holding the key, else starts `argv` in `dir` with `env` |
| `inspect` | yes | `None`, `Running{id}`, `Exited{id, code}` |
| `stop` | yes | a process already gone counts as stopped |
| `input` | no | types text, optionally submits |
| `capture` | no | screen text, cursor, foreground process |
| `exec` | no | a bounded command on the host, deadline and output cap |
| `attach` | no | `{argv}`: a command that is the live terminal of the process |

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

### Attaching

thurbox works without knowing any runtime. It runs `attach`'s `argv` in a
pty pane, as Program panes already do, and renders it with `vt100`. The same
code serves every runtime (`docker exec -it`, `ssh -t`, `runtime-mux view`).

Presenting one clean terminal is the runtime's job, not the interface's. For
`runtime-mux`, `attach` answers `runtime-mux view <key>`: a process that
bridges the window over the runtime's own control-mode client to its stdin and
stdout, with no nested status bar or prefix key, and forwards the pty's size.
Scrollback, resize and reconnection over ssh and WSL stay in the runtime.
Fleet carries no terminal bytes; the interface carries them from one process
it started, never from a multiplexer it knows.

## Where the SQLite tables go

| Table | Destination |
| --- | --- |
| `sessions`: identity, `backend_id`, `backend_type`, `key` | Task `status.session` |
| `sessions`: `launch_*`, `agent_args`, `agent_session_id` | Fleet harness profiles |
| `sessions`: `hook_state`, `hook_state_at` | `fleet signal`, then `status.sessionState` |
| `sessions`: `stopped_at`, `lost_at`, `teardown_owed`, `relaunch_held_at` | Task conditions, receipts, finalizers |
| `sessions`: `parent_session_id` | a parent label on the Task |
| `sessions`: `display_order`, `seen_at` | thurbox interface file |
| `worktrees` | Fleet's workspace port |
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
   `runtime-mux view`; thurbox's pty pane on an attach `argv`.
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
