---
name: thurbox-remote-hosts
description: Remote SSH and WSL sessions in thurbox: hosts.toml schema, the TmuxTransport abstraction, psmux/Windows-host divergences, remote worktrees, shared sessions (ADR-24) and host CLI delegation, agent-config path rewriting, remote hook status delivery via tmux pane options, and remote teardown. Use when working on remote/WSL/Windows hosts, ssh transport, psmux, host provisioning or remote session status.
---

# Thurbox remote SSH and WSL hosts

*Working reference extracted from `CLAUDE.md`, which indexes it. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## Remote SSH & WSL Sessions

Sessions can run on an **off-local host** while the TUI runs locally: a
**remote machine over SSH**, or a **local WSL distro** (`wsl.exe`). A WSL
distro is modeled as "SSH without the ssh" — the *only* difference is the
launch prefix (`wsl.exe -d <distro>` vs `ssh <dest>`); tmux, git, the agent,
and the worktrees all run **inside the distro** at native Linux paths, so
everything downstream of the launcher (control-mode protocol, POSIX quoting,
worktree layout) is identical to the SSH path — no `wslpath` translation. Hosts
are declared as data in `~/.config/thurbox/hosts.toml` (seeded commented-out;
fresh install = zero SSH hosts, behaves as before), **plus WSL distros are
auto-discovered on Windows** (`wsl.exe -l -q`) with no config. The seeded file
documents every field inline; the schema:

```toml
# An SSH host (the default kind):
[[hosts]]
name = "devbox"               # required — backend id "ssh:devbox"; what --host expects
destination = "me@devbox"     # required for ssh — target ("user@host" or ~/.ssh/config alias)
ssh_opts = ["-o", "ControlMaster=auto", "-o", "ControlPersist=10m", "-o", "ServerAliveInterval=15"]
                              # optional (default []) — extra ssh flags; no ~ expansion, use abs paths
socket = "thurbox"            # optional (default "thurbox") — host `tmux -L` socket
session = "thurbox"           # optional (default "thurbox") — host tmux session name
worktrees_dir = "/home/me/.local/share/thurbox/worktrees"
                              # optional — abs worktrees dir on the host
multiplexer = "tmux"          # optional (default "tmux") — set "psmux" for a Windows SSH host

# A WSL distro (only needed to OVERRIDE auto-discovery, e.g. a custom worktrees_dir):
[[hosts]]
name = "ubuntu"               # → backend "wsl:ubuntu"; what --host expects
kind = "wsl"                  # required to select the WSL transport
distro = "Ubuntu-22.04"       # optional (default = name) — the wsl.exe distro name
```

Only `name` (+ `destination` for ssh, `kind` for wsl) is required; every other
field's default is in the comments above and in `docs/CONFIG.md`.

How it works: `TmuxBackend` is transport-neutral
(`agent::transport::TmuxTransport`). The local backend launches
`<mux> -L thurbox …`; an SSH backend launches `ssh <dest> <mux> -L thurbox …`;
a **WSL backend launches `wsl.exe -d <distro> tmux -L thurbox …`**
(`TmuxTransport::Wsl`). `wsl.exe` forwards whitespace-free tokens to the
in-distro shell like `ssh` does, so the same POSIX quoting
(`shell::posix_quote`) and the byte-identical control-mode protocol
(`control_mode.rs`) apply — only the one-time process launch differs. (An arg
*containing whitespace* is preserved as one word, so multi-word `sh -c` scripts
go through `wsl.exe --exec` instead — see `shell::wsl_command` /
`git::host_shell_c`.) The local `DEFAULT_MUX` is **`tmux` on
Linux/macOS and `psmux` on Windows** — psmux is a native-Windows, drop-in tmux
clone (ConPTY, no WSL) speaking the **same control-mode wire protocol** and
pane-id (`%N`) / `-L` socket model, so the whole backend is parameterized by
binary name rather than forked (a remote SSH host can also pin
`multiplexer = "psmux"`); a WSL distro runs `tmux` inside the distro. The
control-mode protocol is byte-identical over either transport/binary, with
**psmux divergences** (verified against psmux 3.3.6, each branched on
`TmuxTransport::uses_psmux()`) — psmux lacks `send-keys -H`, does not join
`new-window` trailing tokens or honour its `-e`, and implements no control-mode
paste command. So thurbox re-encodes keystrokes from the primitives psmux does
support (`send_keys_commands`), folds env + command into **one token** of
PowerShell (`psmux_window_powershell`), and routes a bracketed paste out of band
through the one-shot CLI `psmux send-paste` (`control_mode::PsmuxPaste`). Each
workaround has non-obvious quoting/tokenizing constraints — **read the psmux
divergences subsection of ADR-13 in `docs/ARCHITECTURE.md` before touching this
path**; delivery is probed by `scripts/dev/e2e/windows-vm.sh test` (probes C, D).

`multiplexer = "psmux"` also declares the **host is native Windows**
(`HostDef::is_windows` — the multiplexer is the proxy for the platform, since a
WSL distro runs `tmux` inside Linux), and a Windows host has no POSIX shell. So
each remote probe ships **two scripts emitting one line protocol** —
`git::host_probe` picks `sh -c` or `powershell -EncodedCommand`
(`host_powershell_c`; UTF-16LE base64, because ssh space-joins its args for a
default sshd shell that is commonly PowerShell and expands `$…` inside them) —
and `git::remote_home` resolves `%USERPROFILE%` rather than `$HOME`, which under
`cmd`/PowerShell prints the literal string and exits 0. Remote error *messages*
are cleaned in one place (`git::reportable_stderr`): OpenSSH ≥ 10's three-line
post-quantum advisory is dropped (it is informational, on stderr, and **first**,
so it used to be the whole reported error), and PowerShell's `#< CLIXML` stderr
envelope is decoded to the message inside it. See the two subsections after
"psmux divergences" in ADR-13.

Each host registers a backend named
`ssh:<name>` / `wsl:<name>` (`TmuxBackend::from_host`, registered lazily in
`main.rs` from `host_config::load_all_with_warnings`: discovery/down hosts must
not block startup, so `check_available`/`ensure_ready` are deferred to first use
— looking a backend up is a map read, and the blocking `ensure_ready` runs on the
attach worker in `kernel::terminal` (and on the spawn worker for a fresh
session), never on the loop, ADR-P12).

- **Data**: `session::HostDef` (with `kind: HostKind {Ssh, Wsl}`) /
  `HostRegistry` (pure data, in `session/` so both `agent` and `git` can use
  it); backend-name helpers `is_ssh_backend`/`is_wsl_backend`/
  `is_remote_backend`. **Loading**: `agent::host_config::load_all{,_with_warnings}`
  = configured hosts + `discover_wsl_hosts()` (deduped; a configured entry wins).
- **Selection**: `SessionConfig.backend` (`ssh:<host>` / `wsl:<distro>` or `None`
  = local). The TUI new-session flow shows a **host picker** first (skipped when
  none configured/discovered); the chosen host runs git worktree creation +
  branch listing on that host.
- **Worktrees**: `git::*_on(host, …)` variants run `git` via the host launcher
  (`git::host_launcher` → `ssh …` or `wsl.exe …`). Worktrees live under the
  host's `worktrees_dir` (or `$HOME/.local/share/thurbox/worktrees` resolved +
  cached per backend name — a WSL distro has no `destination`).
- **Persistence/restore**: `backend_type` round-trips in SQLite; restore
  discovers windows **per backend** so off-local sessions re-adopt against their
  own host. In v2 there is no separate restore pass: a session is adopted when a
  pane first asks to paint it, and readying its backend, discovering its window
  and attaching all happen on `kernel::terminal`'s attach worker — the sharpest
  teeth in the loop, since a down host runs out its ssh timeout. So an
  unreachable or slow host never blocks a frame, and nothing is readied that
  nothing is looking at (ADR-P7/ADR-P12, `docs/PERFORMANCE.md`).
- **Headless**: `thurbox-cli session create --host <name>` spawns on the host
  (an SSH name or an auto-discovered WSL distro name).
- **Shared sessions (ADR-24).** A shareable host (`share_sessions = true`, the
  default) owns the record of the sessions on it: its **own thurbox database**.
  A remote thurbox *mirrors* that database into local rows on `ssh:<name>`
  (`session_ops::mirror` — same id, the host's facts and hook status; every
  10 s from a worker in `kernel::terminal` — 60 s after a pass that could not
  run, since a `Yes` verdict is cached for the process lifetime and a host that
  has since gone down would otherwise run its ssh out to the connect timeout six
  times a minute — right after anything it delegated, and from `automation
  tick`), and performs create/delete/restart/restore by
  running `thurbox-cli session …` **on the host** (`session_ops::host_cli`,
  branched inside the four pipelines so every caller delegates). A host with
  no CLI is **provisioned** one under `~/.local/share/thurbox/bin/` (the
  release archive of this version, checksum-verified; a dev build ships its
  own sibling binary when the platform matches) — but a thurbox running on
  the host **advertises its own CLI** there first (`host_cli::
  advertise_running_cli`, a symlink refreshed at TUI boot and on every CLI
  call), which is how a host running a dev checkout is shareable without any
  provisioning. `version --json` reports the
  host CLI's `tmux_socket`, which the backend adopts (`agent::tmux::
  learn_host_socket`) so a dev laptop attaches to a release host's server.
  Everything below this bullet — the hooks rewrite, remote provisioning, the
  pane-option status channel — is the **legacy path** for a host that cannot
  be delegated to (no artifact, no network, schema mismatch, `share_sessions =
  false`); `session create` then reports `sharing` and `session sync --adopt`
  registers such rows on the host later. `session signal` also sets the pane
  option, so tmux hosts keep sub-second status. Relaunch after a reboot is the
  host's (`session restart --if-missing`). A fork stays on the legacy path.
  Docs: `docs/FEATURES.md` → Shared sessions.
- **Agent config on the host**: agent args referencing thurbox-managed config
  by *local* path (the hooks extension's `--settings <config>/hooks/
  claude.json`) would kill the remote agent on launch ("Settings file not
  found"). `session_ops::spawn::adapt_def_for_launch` (shared by headless
  spawn and the TUI, run on the spawn worker — never the UI thread) rewrites
  them per host: on a POSIX remote the home-anchored path is **translated to
  the remote home**, the file copied there, and the arg substituted. A
  **Windows-local** config root (`C:\…` — the Windows TUI driving a WSL distro)
  has no absolute counterpart to mirror, so it lands under the remote
  `$HOME/.config/<root-name>` (final component = dev/release isolation), with
  `\` honoured as a separator **only** for such a root (it is a legal POSIX
  filename char) since the injected arg mixes them (`C:\…\hooks/claude.json`).
  On a psmux host (while `psmux_hook_rewrite_supported` stays off) or a failed
  home lookup/copy the **flag+path pair is stripped** so the agent launches
  clean — surfaced as a `Hooks: degraded` row in the info panel
  (`SessionInfo.hook_wiring`). Literal signal commands carried directly in
  args (aider's `--notifications-command`) are rewritten too.
  The local-location env hints
  (`THURBOX_METRICS_DIR`/`THURBOX_CONFIG_DIR`/`THURBOX_DATA_DIR`, and
  `THURBOX_SOCKET` — the host's sessions are on the host's own server) are
  likewise skipped for remote spawns (`inject_thurbox_env`); only the opaque
  identity vars travel.
- **Remote session status** (hooks-driven, like local, **all agents**):
  `thurbox-cli session signal` can't work from a host (no CLI there; it would
  write the host's own DB), so hook commands are **rewritten**
  (`builtin_hooks::rewrite_hook_signals_for_target`) to set a tmux **pane user
  option** instead — `tmux set-option -p @thurbox_state <s>` needs no socket,
  pane id, or identity inside a pane (the psmux form bakes in
  `-L <socket>`). Delivery per agent: claude's hooks file travels via its
  `--settings` arg; agents wired through their **own config dir** (codex,
  antigravity, opencode, vibe, copilot) are provisioned at spawn time by
  `session_ops::remote_hooks::provision_agent_hooks_on_host` — the rewritten
  payload shipped into the host's agent config dir with the local installer's
  safety rules (`requires_dir` probe over ssh, prune-then-merge for shared JSON,
  managed-marker guard for standalone files, compare-before-write; cached per
  `(backend, agent)`, best-effort, never fails the spawn; remote **cleanup** is a
  documented leave-behind). The local TUI's persistent control-mode connection
  subscribes once per connection (`refresh-client -B
  'thurbox-status:%*:#{@thurbox_state}'`, armed in `ControlMode::start` so
  reconnects re-arm; tmux ≥ 3.2 = the existing floor) and receives
  `%subscription-changed` pushes (≤1/s); a **remote psmux** connection instead
  runs a 1 s **poller thread** (`list-panes -F` diffed by
  `control_mode::diff_polled_hook_states`) feeding the same queue — armed only
  behind the psmux gate below (a poll is an active per-second command, unlike the
  passive subscription, and a *local* psmux session signals via `thurbox-cli`).
  Both channels drain each tick via `App::drain_remote_hook_events` into the same
  `set_hook_state` columns local signals use — so Done→seen acknowledgment, OS
  notifications, and the stuck-`working` fallback are shared. Events are matched
  by **backend name + pane id** (pane ids collide across hosts), allow-listed
  (remote-controlled text), and deduped against the cache (a reconnect re-report
  must not resurrect an acknowledged `done`). Those live channels die with the
  TUI, so the headless **`automation tick`** (the 60 s heartbeat keeper) also
  polls each host with live remote sessions in the DB
  (`session_ops::remote_hooks::poll_remote_hook_states` — one-shot `list-panes
  -F`, allow-listed, diffed against the stored `hook_state`) and writes changes
  into the same columns, so remote status keeps flowing with the TUI closed at
  tick cadence. Remaining carve-out: the **whole psmux/Windows-host path** — hook
  provisioning, rewrite shipping, and the status poller — is gated off on one
  switch (`session::psmux_hook_rewrite_supported`) until the psmux behaviors are
  proven by `scripts/dev/e2e/windows-vm.sh test`'s probes; such sessions show a
  `Hooks: degraded` hint instead of silently idling.
- **Remote teardown** (WSL inherits the SSH path): `session delete --force`
  teardown is **backend-aware** — `teardown_runtime_resources` resolves the
  session's `HostDef` from its `backend_type` and, for a remote session, kills
  the pane via `kill_pane_remote(host, backend_id)` and removes each worktree
  via `git::remove_worktree_on(Some(host), …)` (local sessions keep the
  `kill_window`/`remove_worktree` + Windows pane-reap path). Best-effort: an
  unreachable host or a missing `hosts.toml` entry is recorded in
  `ForceDeleteReport.remote_teardown_error` (surfaced in the CLI JSON) and the
  row is still soft-/force-deleted. Like local force-delete it removes the
  worktree *directory* only, leaving the branch. `wsl.exe`'s exact arg-passing
  isn't verified in CI (no WSL runner); the construction is unit-tested
  (`transport::tests::wsl_*`, `git_command_wsl_*`).
- **Local e2e**: `scripts/dev/e2e/linux-container.sh up` spins a throwaway Podman
  container (sshd + tmux + git) and `… test` asserts a session lands on the
  `ssh:podman` backend (state under `target/`, never touches your real
  `~/.ssh`/`~/.config`).

