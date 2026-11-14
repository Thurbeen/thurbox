# runtime-mux: hosts, psmux and status

`crates/runtime-mux` runs sessions on a tmux-protocol multiplexer: tmux, psmux
or RMUX. thurbox's [architecture decisions](../thurbox/architecture.md) cover
the backend contract (ADR-11), the local server (ADR-12) and the peer adapters
(ADR-31). This page covers how a session reaches another machine, where psmux
differs from tmux, and the status channel of the contract.

## Hosts and transports

A host is another machine (`ssh:<host>`) or a WSL distro on this one
(`wsl:<distro>`). The multiplexer protocol is the same on every host. Only the
launch prefix differs.

- `tmux_compat::server::Server<M>` holds a `TmuxTransport`. A transport is an
  optional **launcher** (`shell::HostLauncher`: `Ssh { destination, ssh_opts }`
  or `Wsl { distro }`) and the multiplexer binary it runs.
- `HostLauncher::for_host` is the one conversion from a host to a launcher.
  The launcher carries any command line unchanged (`shell::launch`). The
  transport adds the multiplexer's `-L`.
- Everything after the launch is the same on every host: control mode, pane
  registration, `send-keys` and `%output`. The per-session commands travel over
  the control connection's stdin, so only the attach crosses the launcher.
- `ssh` uses the system binary and `~/.ssh/config` for keys, agents and
  multiplexing. WSL needs no credentials.
- Hosts are declared in `hosts.toml` (`HostDef`, `HostRegistry`). WSL distros
  are also discovered (`hosts::discover_wsl_hosts`, from `wsl.exe -l -q`). An
  explicit entry of the same name wins. The distro in `$WSL_DISTRO_NAME` is
  this machine (`HostDef::is_wsl_loopback`) and is never a host.
- Each host gets one backend per adapter, keyed by its route
  (`ssh:<host>:<mux>`, `wsl:<distro>:<mux>`, `wiring`).
- A host backend is registered, not connected. It connects on first use, on a
  worker, so a down host never blocks a caller.
- `wsl.exe` never shares the caller's terminal (`shell::wsl_exe`:
  `CREATE_NO_WINDOW` on Windows, `setsid` on Unix). A `wsl.exe` child reads its
  parent console's keyboard even with every std stream redirected.

Rejected: a transport trait (a launcher plus a binary name is enough until a
launcher is not a command prefix), and an embedded SSH library (it would
reimplement `~/.ssh/config`, agent forwarding and multiplexing).

## A host's platform

A session's place, the host's OS, the launcher and the multiplexer are four
independent choices. The route carries the place and the multiplexer. The OS is
`HostDef::platform` (`Posix` or `Windows`); this machine's is
`shell::local_platform`.

- `hosts.toml`'s `platform = "posix" | "windows"` declares it. Without it, a
  host whose `multiplexer` is `psmux` is Windows and any other is POSIX. A WSL
  distro is always POSIX.
- Each decision reads the dimension it is about. The pane's shell, the
  `default-command` pin and the `/bin/sh -lc` wrap follow the host's platform,
  never the build OS or the multiplexer's name.
- Every adapter is registered on every machine and every host. Which ones are
  installed is `Multiplexer::available_on`.
- A restriction in `crates/architecture.toml` keeps
  the route and the contract free of launchers, host entries, adapters and
  `cfg(windows)`. Tests simulate the other OS through `shell::simulate_local`.

## A Windows host speaks PowerShell

A native Windows host has no POSIX shell, and its ssh login shell is `cmd.exe`
or PowerShell.

- `shell::HostShell`, built once from the host's platform, decides how a word
  is quoted, how a script runs and how a program is launched. It is a host
  property: a psmux and a tmux server on one Windows host get the same one.
- A transport for a Windows host (`TmuxTransport::with_shell`) sends a line of
  plain words (`[A-Za-z0-9-_./:=+%]`) as it is. Any other line runs as
  `powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand`. UTF-16LE
  base64 leaves nothing for `cmd.exe` or PowerShell to interpret.
- The script starts the program through `ProcessStartInfo`, with arguments
  quoted for the C runtime (`shell::windows_arg_quote`). PowerShell 5.1 drops an
  empty native argument and splits one at an embedded `"`.
- A line longer than `WINDOWS_CMD_LINE_MAX` (8191, `cmd.exe`'s limit) is
  refused with `CommandLineTooLong` before it reaches the host.

## psmux divergences from tmux

The control-mode protocol is the same, but psmux differs from tmux in the
places below. Each is the psmux adapter's answer to `TmuxCompatible`
(`crates/runtime-mux/src/psmux.rs`), never a branch on the binary's name in
shared code.

- **Cold start.** `new-session -d` can fail or return before the server
  answers. The adapter probes with `list-windows` (psmux's `has-session`
  deletes the port file of a server still starting), retries a transient
  `no server running`, and scopes server options with `-t <session>`.
- **No `send-keys -H`.** Input is encoded from `send-keys -l` literal runs and
  key names. An arrow is one named key. Shift+Enter is `C-j`. Any other
  complete escape sequence is one literal with the ESC as a `0x1b` argument.
  Literal runs go out as `-l -N 1 "…"`, and a leading `-` or a `0xNN`-shaped
  argument is re-emitted as a `0xNN` argument.
- **`new-window` keeps only its first token and ignores `-e`.** Environment and
  command are one PowerShell script (`psmux_window_powershell`), run as
  `powershell.exe -NoLogo -EncodedCommand <base64>` so that no shell on the way
  (Windows OpenSSH sets `SHELL=cmd.exe`) reads it.
- **A paste cannot be key-encoded.** It goes out of band through psmux's own
  `send-paste` with a base64 payload (`PsmuxPaste`). A failed paste is dropped
  with a warning: typing it would submit each pasted line.
- **No per-window options.** `set-option -w` stores one value for the whole
  server, so the window stamp (ADR-25) is neither written nor read on psmux. A
  window resolves by name, and only when exactly one live pane has that name.
- **No reply to the attach on argv.** tmux answers it with one block; psmux
  does not, so the drain is asked only of a multiplexer that answers
  (`ControlPolicy::implicit_attach_reply`).
- **Ids are per session.** Every pane or window id a command targets is
  qualified with the session (`SESSION_SCOPED_IDS`): `thurbox:.%2`,
  `thurbox:@7`. A window is killed through its pane (`KILL_BY_PANE`).
- **An unknown pane is answered for the active one.** `register_pane` asks for
  `#{pane_id}` too and keeps the answer only when it names the pane asked for.
- **`%window-close` can name the wrong window.** For psmux a close is a hint
  (`ControlPolicy::confirm_close`): the session's panes are listed, and only a
  registered pane missing from the listing ends.
- **Errors on stdout.** psmux can print `ERROR:` on stdout and exit 0, so a
  structured listing rejects such a line. Captured pane text is not parsed.
- **An exit takes the window.** The adapter sets no `remain-on-exit`, so a dead
  agent reads as `Missing`. A create is confirmed by its own pane id after
  `SPAWN_SETTLE` (`confirm_spawned`): psmux has no `new-window -P`, so the id
  is the one new pane under `tb-<name>`.

### psmux 3.3.8 is the floor

Before 3.3.8, a pane could be born with corrupt std handles after the server
had been typed into, and a pane-slot target could act on the session's active
pane instead. So no pane is named to, or born on, an older psmux
(`VERSION_FLOOR`).

`Server::target` first asks both halves (`admit_server`): the running server's
`#{version}` and the client binary's `-V`. Each answer must be a plain dotted
version at or above the floor. psmux's own words for "no server" or "no
session" are let through. Anything else refuses. The answer is kept for the
control connection it was asked on. The paint's `resize` and the input loop's
`claim_size` and `is_dead` never wait for the question: they send nothing on a
connection not yet admitted.

## Status channel and heartbeat

`SessionBackend`'s `StatusChannel` role (`crates/runtime-mux/src/contract.rs`)
carries a hook's state and a heartbeat process:

- `hook_signal_command` — the command a hook in this backend's panes runs, with
  the state word appended. `None` means the backend has no channel.
- `record_hook_state`, `hook_states`, `take_hook_state_events` — write a pane's
  state, read every pane's state in one round trip, and drain the changes an
  attached connection received.
- `ensure_heartbeat`, `heartbeat_running`, `stop_heartbeat` — keep a program
  running every interval on the backend's machine with no interface attached.

No method has a default body: a backend with no channel answers `None` or
`Err`, and a reader treats an unanswered question as no news, never as idle.
On a tmux-protocol server the channel is the `@thurbox_state` pane option, its
subscription and its poll, and the heartbeat is a detached
`automation-heartbeat` window. Each adapter says whether it has a channel
(`TmuxCompatible::HOOK_STATUS`); psmux does not.

The contract suite (`crates/runtime-mux/tests/support/backend_contract.rs`)
holds every backend to this behaviour. Neither thurbox nor the runtime protocol
calls the role: a session's state is Fleet's, derived from the agent's hooks
through `fleet signal`.
