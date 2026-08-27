## Why

A session is a tmux window on some host plus a row in the SQLite database of
the thurbox that created it. When that host is reached over SSH or WSL, two
thurboxes already share its tmux server — a laptop spawning on `ssh:devbox`
and a thurbox running *on* devbox both use `tmux -L thurbox` there — but each
keeps its own database, so each sees only the sessions it made. Someone who
works on a machine both directly and remotely keeps two disjoint session lists
over one set of agents, and cannot pick up on one side what they started on
the other.

Today the laptop does everything *to* the host from afar — creates the
worktree over ssh, ships a rewritten hooks config, launches the agent, and
reads status back through a tmux pane option — because nothing of thurbox is
assumed to exist there. That is also why a host with its own thurbox cannot
see those sessions: they are recorded nowhere on it. The simpler shape is the
one every other remote tool uses: **the host owns its sessions' records, and
a remote thurbox asks the host.**

## What Changes

- **The host's database is the source of truth for the sessions on it.** A
  session that runs on host H is a row in H's database, whoever created it. A
  remote thurbox **mirrors** that database into local rows on `ssh:H` /
  `wsl:H` — the same id, the same facts, the host's hook status — and attaches
  to the windows by name as it does today. Deletions, restores and restarts
  performed on either side show up on the other as data, not as inferences
  from windows.
- **Remote operations are delegated to `thurbox-cli` on the host.** Creating,
  deleting, restarting and restoring a session on H run `thurbox-cli session
  create|delete|restart|restore` *on H*, which does the worktree, the hooks,
  the agent launch and the record natively, with H's own `agents.toml` and
  `hooks.toml`. Every caller — the TUI's creation flow and `Ctrl+F` fork,
  `thurbox-cli … --host`, `spawn` automations, extension self-heal — goes
  through the same pipelines and so delegates without knowing it.
- **A host without thurbox gets a CLI provisioned.** On first use of a
  shareable host, thurbox probes for `thurbox-cli`; when it is absent or a
  different major, it downloads the release artifact for the host's platform
  (the same download-and-verify the self-updater uses), pushes `thurbox-cli`
  to `~/.local/share/thurbox/bin/` on the host, and uses it from there. The
  first session then creates the host's database at the standard location, so
  a later full install of thurbox on that host finds every session already
  there. A host where no CLI can be provisioned (a dev build on a foreign
  platform, no network) keeps **exactly today's behaviour** — the remote
  worktree + hooks-rewrite path — and says so in the session's info.
- **The remote-hooks rewrite is no longer needed on a shared host.** The
  agent's hooks call the host's own `thurbox-cli session signal`, so status is
  written where the row is. This closes the `Hooks: degraded` carve-out for
  Windows (psmux) hosts: delegation goes through the PowerShell path the
  probes already use, and the host CLI launches the agent natively.
- **Sub-second status is kept where it existed.** `session signal` also sets
  the pane option a remote observer's control-mode subscription already
  reads, so a tmux host's status still arrives live; the mirror is the
  fallback and the only channel on psmux.
- **After a host reboot the host relaunches its own sessions**, as it does
  today for local ones; a remote thurbox that sees a mirrored row with no
  window *asks* the host to relaunch it (`restart --if-missing`, idempotent)
  rather than launching anything itself. No duplicate launches by
  construction.
- **`thurbox-cli session sync [--host <name>]`** runs one mirror pass
  explicitly; `session list --deleted` exposes the deleted list the mirror
  reads; `session restart --if-missing`.
- Per-host `share_sessions = false` in `hosts.toml` opts a host out of all of
  the above.

## Capabilities

### New Capabilities

- `shared-sessions`: the host's database owns the sessions on that host; a
  remote thurbox mirrors it and delegates create/delete/restart/restore to
  the host's CLI, provisioning that CLI when the host lacks it — with the
  status, reboot, undo/restore and Windows consequences.

### Modified Capabilities

- `session-creation`: creating a session on a shareable host is performed by
  the host's CLI with the host's configuration; the parent of a session must
  live on the same host.
- `session-hooks`: a delegated operation fires the caller's hooks locally, as
  today, **and** the host's own hooks on the host.

## Impact

- `session_ops/`: a `host_cli` module (probe, provision, run a `thurbox-cli`
  command on a host over ssh / `wsl.exe` / PowerShell and parse its JSON) and
  a `mirror` module (list + deleted list → local rows); `spawn`, `delete`,
  `restart` and `restore` branch to delegation for a shareable host; the
  existing remote path stays for hosts without a CLI.
- `agent/self_update.rs`: the artifact resolver learns the Windows zip and is
  reusable for a *foreign* target (host platform, not the running one).
- `agent/tmux.rs` + `cli/sessions.rs`: `session signal` sets the pane option;
  `list --deleted`, `restart --if-missing`, `sync`; `version --json` reports the
  tmux socket the host CLI uses, so the mirror attaches to the right server.
- `kernel/terminal`: a mirror worker per shareable host on `MIRROR_INTERVAL`
  plus an immediate pass after each delegated operation; by-name pane
  resolution extended to remote backends; `missing_agents` asks the host to
  relaunch instead of launching.
- `session/`: `HostDef.share_sessions`; `SessionInfo.sharing` (transient hint
  like `hook_wiring`).
- `storage`: no schema change. A mirrored session is an ordinary row.
- Docs: `docs/CONFIG.md`, `docs/FEATURES.md`, `docs/ARCHITECTURE.md` (a new
  ADR that also supersedes the remote-hooks bullets of ADR-13 for shared
  hosts), `docs/AGENTS.md` (psmux hook status), `CLAUDE.md`, seeded
  `hosts.toml`.
- Tests: unit tests beside `host_cli`/`mirror` with a fake runner;
  `tests/v2_shared_sessions.rs` over an in-memory database; the Podman e2e
  gains provisioning + both directions + reboot; `windows-vm.sh` gains a
  delegated-create probe.
- Unchanged: the database stays per-thurbox for everything that is not a
  session (messages, tasks, automations, review comments, metrics,
  `display_order`); hosts without a provisionable CLI behave as today.
