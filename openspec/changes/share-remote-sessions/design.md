## Context

See `proposal.md` → Why. The facts that shape the approach:

- A session is a `sessions` row (per-thurbox SQLite) bound to a tmux window
  `tb-<sanitized name>` by pane id, with a by-name fallback that is local-only
  today (`Terminals::pane_by_name`). `shared_session_to_json` already emits
  every fact of a row except `backend_id`; `session_json_with_state` adds
  `hook_state`. There is no CLI listing of *deleted* rows.
- The remote path today is laptop-driven end to end: `resolve_dirs` creates
  worktrees over ssh, `adapt_def_for_launch` rewrites the agent's hooks
  config for the host and ships it, `provision_agent_hooks_on_host` installs
  hooks into the host's agent config dirs, `spawn_window` launches over the
  transport, and status comes back through `@thurbox_state` on the pane. The
  whole psmux half of that is gated off (`psmux_hook_rewrite_supported`).
- Every session operation already ends in one of four `session_ops`
  pipelines — `spawn_session_headless`, `delete_session_headless`,
  `restart_session_headless`, `restore_session_headless` — used by the TUI,
  the CLI, automations and extension self-heal alike.
- Running a command on a host and reading one line back is a solved shape:
  `git::host_shell_c` (POSIX) / `host_powershell_c` (UTF-16LE base64 for a
  Windows sshd) and `reportable_stderr` for the error text; `copy_bytes_to_remote`
  ships a file. `agent::self_update` resolves a release artifact per target
  and verifies it against the release checksums, for the *running* platform.
- The heartbeat keeper (`arm_heartbeat`, a tmux window looping `automation
  tick`) is armed by the TUI at startup and by `automation create`; `tick`
  heals extensions, fires automations and polls remote hook states.
- Rule 5 (nothing on the loop), the module allowlist (`session_ops` may use
  `git`, `agent`, `storage`; `kernel` reaches `session_ops` by path), and the
  hooks contract (`fire_pre`/`fire_post` around each pipeline).

## Goals / Non-Goals

**Goals:**

- One authoritative record per session, on the host it runs on; observers
  are caches. No inference of state from tmux windows.
- Every existing caller delegates by construction — the four pipelines
  branch, callers do not.
- A host that has only tmux and git becomes shareable on first use, without
  the user installing anything, and stays exactly as it is when that cannot
  be done.
- Shrink the remote path: on a shared host, no remote worktree creation from
  afar, no hooks rewrite, no hooks provisioning, and the psmux carve-out
  closes.

**Non-Goals:**

- Sharing anything but sessions. Messages, tasks, automations, review
  comments, metrics and `display_order` stay per-thurbox (a laptop session
  cannot `message send` to a host session; that is a later change).
- Removing the laptop-driven remote path. It stays for hosts with no usable
  CLI, and it is what `share_sessions = false` selects.
- Live-reloading `hosts.toml`.
- The companion shell pane, which stays per-observer.

## Decisions

### D1 — The host's database is the record; a remote thurbox is a client

**Choice.** Every session on H is a row in H's database. A remote thurbox
mirrors H's rows and performs every write on H through H's `thurbox-cli`.

**Alternatives.**
- *Stamp each session's facts on its tmux window and reconcile from the
  server* (the first draft of this change). No host requirement, but the
  tmux server is a volatile store with no notion of deleted or restored, so
  it needed tombstones with a TTL, a claim protocol for relaunch, a
  lowest-pane-id rule, a pane-id heuristic for undo and a probe before
  relaunch. Every one of those is a consequence of the store; the host's
  database already has `deleted_at`, `force_deleted`, `restore` and
  `hook_state`. Rejected once "thurbox-cli on the host" stopped being an
  obstacle (D3).
- *Read and write the host's SQLite file from afar* (copy down / edit / copy
  back, or over sshfs). SQLite's locking needs the writer on the file's own
  filesystem; a copy-back loses whatever a host process wrote in between.
  Rejected outright.
- *`sqlite3` on the host*. Not more likely to be installed than thurbox, and
  it would mean the schema and every rule twice. Rejected.

### D2 — Delegation happens inside the four pipelines

`spawn_session_headless`, `delete_session_headless`,
`restart_session_headless` and `restore_session_headless` each begin with
`resolve_host`; when the host is shareable and `host_cli::usable(host)` says
so, the pipeline runs the caller's `fire_pre`, then
`host_cli::run(host, ["session", "create", …, "--json"])`, then applies the
host's answer to the local database, then `fire_post`. Otherwise it continues
into the existing code unchanged. Callers (`kernel::command::execute`,
`cli::sessions`, `cli::action`, `extensions::lifecycle`) do not change, which
is what makes the TUI's creation flow, `Ctrl+F`, `spawn` automations and
extension self-heal delegate for free.

The delegated `create` passes through `--name --repo-path --agent
--worktree-branch --base-branch --parent --add-repo --add-dir`. `--parent`
must be a session on H (the host validates against its own database, so a
laptop-local parent is refused — checked locally first, from the parent's
`backend_type`, so the refusal is immediate). The host mints the id; the
local row is written from the host's JSON (`id`, `name`, `agent`,
`agent_session_id`, `cwd`, `parent_session_id`) plus a follow-up `session get`
for worktrees and `base_branch`. Progress: a new `SpawnPhase::Host` reported
for the whole delegated call — the host's CLI is one blocking process and
cannot stream its own phases — added to the pinned phase sequence and the
`PHASE_LABEL` table.

`delete` delegates `--force` as given. Soft delete on the host leaves the
window running until H's own reaper or tick lets it go (headless soft delete
is the same today) — the laptop's reaper does **not** kill a mirrored row's
window; H owns it. `restart` and `restore` delegate whole; the local-only
refusal in `restore_session_headless` applies only to non-shareable hosts
now, because the host can recreate its own worktrees.

### D3 — Provisioning the host CLI

`host_cli::usable(host) -> Usable { path, socket, version } | Unusable(reason)`,
cached per backend name for the process (`OnceLock`-style map, like the
readied-backend set): probe `thurbox-cli version --json` on PATH, then at
`<host thurbox data dir>/bin/thurbox-cli`. A CLI of the same major is used; a
different major or none triggers provisioning:

1. `uname -sm` (POSIX) / `$env:PROCESSOR_ARCHITECTURE` (PowerShell) → target
   triple via `self_update::target_triple(os, arch)`, extended with
   `x86_64-pc-windows-msvc` (the zip artifact).
2. The **caller's own version** is the version fetched (not "latest"): a
   peer must speak this binary's JSON. `self_update` grows a
   `fetch_artifact(version, target) -> PathBuf` that downloads and verifies
   with the code `perform_update` uses; it refuses for a dev build
   (`0.0.0-dev`), except that a dev build whose own target equals the host's
   pushes its *own* `thurbox-cli` binary — that is what the Podman e2e
   exercises.
3. `copy_bytes_to_remote` the `thurbox-cli` binary to
   `<host thurbox data dir>/bin/thurbox-cli` (`chmod +x` on POSIX), then
   re-probe.

**The host advertises its own CLI.** A peer looks in `<data dir>/bin/` first,
and every thurbox — the TUI at boot, `thurbox-cli` on every call — keeps a
symlink there to its own `thurbox-cli` (`host_cli::advertise_running_cli`).
This is what makes a host running a **dev checkout** shareable: its
`target/debug/thurbox-cli` is on nobody's PATH, so without the pointer a peer
found only the release install (a different major) and had to provision —
which a dev peer can do only onto its own platform. Found in the first live
test: a macOS dev laptop against a Linux host running a dev checkout got the
legacy path on both sides.

The host thurbox data dir is the *release* layout on the host
(`$HOME/.local/share/thurbox`, `%LOCALAPPDATA%\thurbox`), which is where a
full install would look — so a later `install.sh` on H finds the database the
provisioned CLI created. A provisioned CLI is never used to replace an
installed one on PATH; it sits beside it, and the installed one wins as soon
as its major matches.

**Sockets.** A release CLI on H uses the `thurbox` socket; the laptop must
attach to that server. `version --json` gains `tmux_socket` (and
`data_dir`), and `TmuxBackend::from_host` takes the socket from the usable
CLI when there is one, else `HostDef.socket`/the local default as today. A
dev laptop therefore attaches to H's release server — correct, since that is
where H's sessions are.

`Unusable(reason)` lands on `SessionInfo.sharing` (transient, like
`hook_wiring`), rendered as `Sharing: off (<reason>)` in the info panel; the
session still spawns through the laptop-driven path.

### D4 — The mirror

`session_ops::mirror::mirror_host(db, host, cli) -> MirrorReport { adopted,
updated, deleted, restored }` runs `session list --json` and `session list
--deleted --json` on H and reconciles the local rows whose `backend_type` is
H's backend name:

- a host row with no local row → `upsert_session` (same id, host facts,
  `backend_type = ssh:H`, `backend_id = ""` — resolved by window name on
  attach, D5), plus `set_hook_state` from the host's `hook_state` when it
  differs from the local raw value (the existing echo rule);
- a host row whose facts differ → `upsert_session` (never the hook columns,
  never `display_order`/`shell_backend_id`, which are the observer's);
- a local active row the host lists as deleted → `soft_delete_session`, plus
  `mark_session_force_deleted` when H says so; the reaper is told to forget
  the id (H killed or will kill the window);
- a local deleted row the host lists as active → `restore_session` (flags
  only; H already relaunched);
- a local row H knows nothing about at all (neither list) → left alone and
  reported: it is a session the laptop created before this change, on the
  laptop-driven path. It keeps working exactly as before; it is not adopted
  into H's database, and D9 says how it can be.

A pass that changes nothing writes nothing (`upsert_session` is skipped when
the facts are equal), so an idle mirror does not bump `data_version`.

Callers: a per-host mirror worker in `kernel::terminal` on `MIRROR_INTERVAL`
(10 s; one multiplexed ssh ≈ 100 ms) and immediately after any delegated
operation on that host; `automation tick`; `session sync`. The worker readies
nothing on the loop and queues the first pass after boot (ADR-P12).

### D5 — Attaching to a mirrored session

H's CLI emits each row's pane id (`backend_id` in `session list --json`), and
the observer attaches to the *same* tmux server, so a mirrored row normally
carries the exact pane. `Terminals::sync` already resolves a paneless row by
window name for local backends; the `!is_remote_backend` filter is dropped so
the by-name fallback serves a host that cannot report a pane (psmux). Names
are deliberately **not** made unique — `tests/create_e2e.rs` pins that two
sessions may share one, the pane id being what disambiguates — so the
fallback keeps its existing "ambiguous name resolves to nothing" rule, and
`session register` (which has only the name to go on) refuses a collision.
`missing_agents`
for a mirrored row does not launch: it issues `host_cli::run(host, ["session",
"restart", id, "--if-missing"])` on the attach worker, throttled by
`ATTACH_RETRY_INTERVAL`; `--if-missing` makes H relaunch only when the window
is absent, so N observers asking is one launch. A host whose CLI is unusable
falls back to today's local relaunch.

### D6 — Status

Hooks on a shared host call H's own `thurbox-cli session signal`, which writes
H's database — the mirror carries it at 10 s. To keep the sub-second channel
tmux hosts have today, `session signal` *also* sets `@thurbox_state` on its
own pane when `$TMUX`/`$TMUX_PANE` are set (socket path from `$TMUX`), which
the laptop's existing control-mode subscription receives and
`apply_hook_states` dedups against the raw `hook_state`. On psmux the poller
stays gated and the mirror is the channel — which is still strictly better
than the `Hooks: degraded` those hosts show now. `docs/AGENTS.md`'s status
table gets the psmux column updated.

### D7 — Hooks on both sides

The caller's `hooks.toml` fires locally around the delegated call
(`THURBOX_HOST` set, as for any remote session today); H's `hooks.toml` fires
on H inside H's CLI. A local pre-hook veto stops the delegation; H's veto
comes back as the CLI's error and becomes the caller's failure. Recorded as an
added requirement in `session-hooks`.

### D8 — Windows hosts

`host_cli::run` picks `host_shell_c` or `host_powershell_c` by
`HostDef::is_windows`, exactly as `git::host_probe` does; the CLI's arguments
are passed inside the encoded PowerShell script with PowerShell quoting, so
no `$…` expansion reaches sshd's shell. Provisioning downloads the zip and
`Expand-Archive`s it on the host. Because H's CLI launches the agent locally,
the agent's hooks are H's ordinary local ones (`thurbox-cli session signal`
works on native Windows today), so the psmux hooks-rewrite gate is simply not
consulted for a shared host. Probed in `windows-vm.sh test`.

### D9 — Sessions that predate this change

A remote session created by the old laptop-driven path has a row on the
laptop and none on H. It is left alone (D4) and keeps working. To bring it
under H's database: `thurbox-cli session sync --host H --adopt` runs
`thurbox-cli session register --json '<row>'` on H for each such row — a
small host-side command that inserts a row for a window that already exists
(no launch), validating that `tb-<name>` is present on H's server. Opt-in,
one-shot, documented. It is the only place a "row for a window I did not
spawn" exists, and it is explicit.

### D10 — What `share_sessions = false` means

The host is used precisely as today: no probe, no provisioning, no mirror, no
delegation. It is the escape hatch for a host whose thurbox the user does not
want touched, and the way to test the legacy path.

### D11 — A fork stays on the legacy path

A fork resumes the parent's conversation (`fork_session_id`) in the parent's
checkout (`inherit_worktrees`) — two facts the host's `session create` does
not take. Rather than grow the CLI a `--fork-from`/`--inherit-worktree` pair
in this change, a fork of a session on a shareable host is created from here
exactly as before, its `sharing` note says so, and `session sync --adopt`
registers it on the host afterwards (it lists as `unknown_local` until then).
Teaching the host CLI to fork is the natural follow-up.

## Risks / Trade-offs

- [Host CLI version drift] → The JSON is the contract; the probe requires
  the same major, and provisioning fetches the caller's exact version. A host
  with a newer major than the laptop is used through a provisioned side-by-
  side copy of the laptop's version, both writing H's database — schema
  migrations are forward-only, so the older CLI opens a newer database only
  if no migration is needed; when `SCHEMA_VERSION` differs the probe reports
  `Unusable(schema)` and the legacy path is used.
- [Status latency on the mirror] → 10 s worst case, tmux hosts keep the live
  pane channel (D6). Configurable interval is deliberately not offered.
- [Two observers resize one pane] → Existing multi-instance behaviour.
- [Provisioning downloads a binary onto a host] → Same artifact and the same
  checksum verification as `thurbox-cli update`; into thurbox's own directory,
  never PATH; logged; `share_sessions = false` prevents it entirely.
- [A delegated create is one opaque blocking call] → One phase named for the
  host; the host's error text is the caller's error text.
- [Laptop-created legacy sessions look unshared] → Reported by the mirror
  and adoptable with `--adopt` (D9).
- [The laptop reaper vs a soft delete on the host] → Only H reaps its rows;
  the laptop's reaper skips mirrored ids. A headless soft delete leaves the
  window until H's TUI or tick runs, exactly as a headless soft delete does
  locally today.

## Migration Plan

- No schema change on either side.
- Existing remote sessions keep working on the legacy path and are adoptable
  with `session sync --adopt`.
- Old laptop + new host, or the reverse: the probe's major check gates
  delegation; the legacy path is the fallback both ways.
- Rollback: downgrade; a provisioned `thurbox-cli` under
  `~/.local/share/thurbox/bin` is inert, and H's database is an ordinary
  thurbox database.

## Open Questions

- Should the mirror also carry `display_order` when the observer has never
  ordered a session itself? Deferred; observer-owned for now.
- Should `message send --to` resolve a session on another host and delegate
  the send? Natural follow-up once sessions are shared; out of scope here.
