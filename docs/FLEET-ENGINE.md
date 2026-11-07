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
4. **The multiplexer runtime is its own component**, extracted from
   `src/backend`, with no knowledge of Fleet, Tasks, agents or the interface.

## Components

```text
thurbox (interface) ──process: fleet watch / apply / send / delete / attach──▶ Fleet (engine)
   │                                                                             │
   │ attach answer: argv, or a tmux pane to adopt                                │ process: runtime protocol
   ▼                                                                             ▼
 terminal (pty or tmux control mode) ◀──────────────────────────────── mux-runtime, or any runtime
                                                                      (tmux · psmux · rmux · ssh · WSL)
 coding agent ── fleet signal / fleet result ──▶ Fleet
```

| Component | Owns | Never owns |
| --- | --- | --- |
| Fleet (`crates/fleet-*`) | Tasks (one per session), the store, every decision, harness profiles (launch, resume, trust), agent hooks through `fleet signal`, workspaces and worktrees, the inbox, receipts, the runtime protocol and its conformance suite | Multiplexer code; any Rust dependency on `mux` or `thurbox` |
| Runtime (`crates/mux`, `crates/mux-runtime`) | Windows by key, host connections (`hosts.toml`), bounded `exec`, the attach address | Tasks, agents, journals, delivery, the interface |
| thurbox (`crates/thurbox`) | The TUI, the Lua kernel, rendering, the tmux control-mode client, interface state in plain files (order, seen marks, bookmarks, review notes, settings, keybindings) | Sessions, SQLite, any Rust dependency on `fleet-*` or `mux` |

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
| `fleet-*` | `fleet-*`, crates.io | `mux`, `mux-runtime`, `thurbox` |
| `mux`, `mux-runtime` | `mux`, crates.io | `fleet-*`, `thurbox` |
| `thurbox` | crates.io | `fleet-*`, `mux`, `mux-runtime` |

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
| `attach` | no | `{argv, tmux?: {socket, route, pane}}` |

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

### Attaching

thurbox needs no runtime library. With `attach`'s `argv` it opens a pty pane,
as Program panes already do, which works for any runtime (`ssh -t`,
`docker exec -it`, `tmux attach`). When the answer carries `tmux`, it adopts
the pane over its own control-mode client instead. Fleet carries no terminal
bytes.

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
2. **Extract `mux`.** Move `src/backend` into `crates/mux` with
   `config::hosts`, `agent::host_path` and `agent::preflight`; inject what it
   reads from `session::settings`. The tmux control-mode client stays in
   thurbox. Add the `architecture.toml` rule.
3. **Import Fleet** with its history under `crates/fleet-*`, its docs under
   `docs/fleet/`. Pin one toolchain (Fleet's); editions may differ per crate.
   One CI with path filters; component-scoped tags (`thurbox-v*`, `fleet-v*`,
   `mux-v*`) with cocogitto's monorepo mode. Transfer Fleet's open issues.
4. **Runtime protocol.** `mux-runtime` binary; Fleet's generic `exec` runtime
   kind, `[runtimes.<name>]`, `fleet runtime check`, the reference script.
   Fleet's P1 to P4 (workspaces, `fleet signal`, harness profiles, delivery by
   hook) land here if not before.
5. **Views.** `fleet watch`, `fleet attach`, interactive Tasks without a brief;
   thurbox's pty pane on an attach `argv` and adoption of a foreign pane.
6. **thurbox on Fleet.** `SnapshotSource` and `Executor` traits, then their
   Fleet implementations; thurbox starts `fleet up`.
7. **Migrate.** `thurbox-cli migrate fleet`: one Task per live session,
   adopting its window by stamp and its worktree by path; `agents.toml` into
   harness profiles. Automations move once the schedule kind exists.
8. **Delete.** `session_ops`, `storage`, the engine half of `thurbox-cli`, and
   Fleet's thurbox session adapter.
9. **Release.** thurbox's archive and packages carry `fleet` and
   `mux-runtime`; Fleet keeps a standalone archive. Fleet's old install URL
   and `FLEET_MAJOR` guard point here or are retired.

## Open

- A schedule kind in Fleet to replace automations.
- Fork and `restart --fresh`: a conversation field on the Task, or dropped.
- `hooks.toml` beyond `pre_launch`: harness settings or a Fleet plugin.
- Shared hosts (ADR-24): one Fleet store per operator is assumed.
- The tmux server Fleet's windows live on: the interface's own (`-L thurbox`)
  so control mode adopts them without a second connection, assumed.
- Windows (psmux) at the cutover, or after Linux and macOS.
- VEGA stays a separate starter repository; Fleetlens moves under
  `extensions/`.
