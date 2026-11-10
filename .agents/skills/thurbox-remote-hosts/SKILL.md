---
name: thurbox-remote-hosts
description: Remote SSH and WSL hosts: the hosts.toml schema (runtime-mux's) and WSL discovery, how a picked host reaches Fleet, why thurbox reads this machine only and runs only companion shells on a host, the TmuxTransport abstraction in crates/runtime-mux, the host's PATH probe, psmux/Windows-host divergences, and the failure classifiers. Use when working on remote/WSL/Windows hosts, ssh transport, psmux or hosts.toml.
---

# Thurbox remote SSH and WSL hosts

*Working reference indexed by `AGENTS.md`. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## What a host is for

An off-local host is a **remote machine over SSH** or a **local WSL distro**
(`wsl.exe`). Every session is a Fleet Task, and Fleet runs it on the host its
spec names, through the runtime Fleet's `[hosts.<name>]` names. thurbox
itself reads this machine only:

- **Creating a session.** The host step of the new-session flow is always
  shown: it offers this machine at once and adds the hosts
  `fleet runtime hosts --json` lists when Fleet answers. The picked host
  becomes the Task's `spec.host`; this machine is `local`
  (`Fleet::host`, `src/kernel/command/fleet.rs`). A host Fleet does not list
  is refused, with a hint to add `[hosts.<name>]` to Fleet's config. A
  session on another host gets a `dir` workspace: Fleet makes worktrees on
  this machine only.
- **The creation flow's reads** happen on this machine only
  (`src/kernel/repos.rs`). A path typed for another host is remembered and
  forgotten as typed (`src/kernel/command/execute.rs`).
- **Diffs, git stats and plugin runs** refuse a session on another machine
  (`src/kernel/diff.rs`, `SessionRow::workspace_reachable`,
  `kernel::runs::command_for`).
- **Companion shells** (`Ctrl+T`). A Fleet session's shell runs on the
  multiplexer of the `hosts.toml` host its Fleet host is named as, on that
  host's default route (`Terminals::shell_route`,
  `src/kernel/terminal/shells.rs`). No such host means no shell. This is the
  one place thurbox reads `hosts.toml`.
- **Image drops** onto a remote session are refused with a message
  (`refuses_a_remote_image_drop`, `src/coordinator/input.rs`).

The agent's own terminal is the stream `fleet attach` names, not a pane of
thurbox's backends.

## hosts.toml

`~/.config/thurbox/hosts.toml` is seeded commented out, so a fresh install has
no SSH hosts. The seeded file documents every field inline
(`crates/runtime-mux/src/hosts.rs`); the schema is `HostDef`
(`crates/runtime-mux/src/host/host_def.rs`):

```toml
# An SSH host (the default kind):
[[hosts]]
name = "devbox"               # required — backend id "ssh:devbox"
destination = "me@devbox"     # required for ssh — "user@host" or an ~/.ssh/config alias
ssh_opts = ["-o", "ServerAliveInterval=15"]
                              # optional (default []) — extra ssh flags; no ~ expansion
socket = "thurbox"            # optional — the host's `tmux -L` socket
session = "thurbox"           # optional — the host's tmux session name
multiplexer = "tmux"          # optional (default: the platform's — tmux on POSIX, psmux on Windows)
platform = "windows"          # optional — "posix"/"windows"; unset = windows iff multiplexer is psmux
path_prepend = ["~/.local/bin"]
                              # optional — put in front of the host's PATH

# A WSL distro (only needed to override auto-discovery):
[[hosts]]
name = "ubuntu"               # → backend "wsl:ubuntu"
kind = "wsl"                  # required to select the WSL transport
distro = "Ubuntu-22.04"       # optional (default = name) — the wsl.exe distro name
```

Only `name` (plus `destination` for ssh, `kind` for wsl) is required. A host
name may not contain `:`. `share_sessions` (default `true`) still parses; its
one effect is on socket resolution (see *Sockets* below). `worktrees_dir` and
`otel_endpoint` still parse and have no effect since 3.0.
`thurbox-cli config` validates the file, and `thurbox-cli doctor` reads the
cached registry.

**Loading.** `config::hosts` (`src/config/hosts.rs`) wraps
`runtime_mux::hosts`: `load_all` is the configured hosts plus
`discover_wsl_hosts()` (deduplicated; a configured entry wins).
`cached_registry` is the one read every thurbox caller shares. WSL distros are
discovered with `wsl.exe -l -q`, on Windows and, through interop, from inside a
distro. Discovery never offers the distro thurbox runs in: a **loopback**
(`HostDef::is_wsl_loopback`, keyed on `$WSL_DISTRO_NAME`) is this machine, and
it is dropped from both the discovered and the configured halves (the
configured one with a warning).

**Data.** `HostDef` (with `kind: HostKind {Ssh, Wsl}`) and `HostRegistry` are
pure data in `crates/runtime-mux`, re-exported through `session`. A route is
read by one parser, `session::Route` (machine × multiplexer; grammar in
`docs/thurbox/config.md` → *Multiplexer choice and existing sessions*). A host
key (`ssh:<host>`) resolves through `HostRegistry::resolve`, and
`HostDef::default_route` gives its multiplexer.

## The transport (crates/runtime-mux)

`crates/runtime-mux` is the multiplexer runtime; `src/backend.rs` re-exports
it and injects thurbox's settings (`mux_settings`). Each tmux-protocol adapter
(`TmuxBackend`, `PsmuxBackend`, `RmuxBackend`, all over
`tmux_compat::server::Server<M>`) runs over a transport-neutral
`tmux_compat::transport::TmuxTransport`: an optional `shell::HostLauncher` plus
the multiplexer binary.

- The local backend launches `<mux> -L <socket> …`. An SSH backend launches
  `ssh <dest> <mux> -L <host socket> …`. A WSL backend launches
  `wsl.exe -d <distro> <mux> -L <host socket> …` (`HostLauncher::Wsl`). Every
  launcher comes from one conversion, `HostLauncher::for_host`.
- A WSL distro is "SSH without the ssh": tmux, git and the shell run inside the
  distro at native Linux paths, so POSIX quoting (`shell::posix_quote`) and the
  control-mode protocol are the same. An argument containing whitespace goes
  through `wsl.exe --exec` instead (`shell::wsl_command`,
  `git::host_shell_c`).
- Every `wsl.exe` starts off the interface's terminal (`shell::wsl_exe`:
  `CREATE_NO_WINDOW` on Windows, `setsid` on Unix). A `wsl.exe` child otherwise
  reads its parent console's keyboard input.
- For tmux on a POSIX host, the multiplexer runs behind a `sh -c` that sets
  `TMUX_TMPDIR` to the host's data dir (ADR-34). A server lost from its socket
  is found again with `SIGUSR1` before any `new-session`.
- SSH is fail-fast: `shell::SSH_HARDENING_OPTS` (`BatchMode=yes`,
  `ConnectTimeout`, `ServerAlive*`), plus `SSH_MULTIPLEX_OPTS`
  (`ControlMaster=auto`, socket under `~/.ssh`) when that directory exists.
  Both are appended after the user's `ssh_opts`, whose first occurrence wins.
  A broken host never prompts on the TUI's terminal.
- `wiring::configured` (`src/backend.rs`) registers one backend per adapter on
  this machine and on every host, named by route (`ssh:<name>:<mux>`,
  `wsl:<name>:<mux>`). Looking a backend up is a map read;
  `check_available`/`ensure_ready` are deferred to first use and run on a
  worker, never on the loop (ADR-P12).
- A connection that has gone bad stays open and carries nothing, so the loop
  never waits on one (ADR-P24). A paint's resize is sent, not asked
  (`ControlMode::send_command_detached`), and `Server::ctrl_command_within`
  bounds the wait for the control lock as well as for the answer.
- Several instances on one server size a pane by turns (ADR-27): a resize is
  honoured only for a window whose `@thurbox_sizer` is this backend's, and input
  or focus claims it (`claim_size`). psmux keeps last-writer-wins.

**Sockets.** A remote operation acts only on a socket the host vouches for
(`instance::known_host_socket`): `hosts.toml`'s `socket`, else one recorded by
`learn_host_socket`, else this build's default for a host with
`share_sessions = false`. Any other host is refused with "socket unknown for
host '…'", because this build's default is a guess about another machine.

## The host's PATH

ssh and `wsl.exe -e` give a command a non-login `PATH`.
`backend::host_path` (`crates/runtime-mux/src/host_path.rs`) probes the host's
login `PATH` (`$SHELL -lc` and `/bin/sh -lc`) once per host, caches it
(failures included), and bounds it with `timeout` and a 15 s kill.
`TmuxTransport::with_path` assigns `path_prepend`, then the login shells'
`PATH`, then the launcher's, de-duplicated, in front of every multiplexer
command. No answer means no change plus a warning. Tests never probe; seed one
with `host_path::seed`.

The same probe names each multiplexer on that `PATH` (`@mux` lines). The
new-session picker offers only those (`host_path::multiplexers_found`: never
waited for, re-asked after `MUX_TTL`; `seed_multiplexers` in tests).

## Platforms and psmux

A host's **platform** is its own field (`HostDef::platform`, ADR-13 "The
host's platform is its own dimension"). `platform = "windows"` declares a
native Windows host on any multiplexer. An entry that sets none reads as
Windows when `multiplexer = "psmux"`, else POSIX; a WSL distro is always POSIX.
The default shell and the `/bin/sh -lc` login wrap follow the platform, never
`cfg(windows)` or the multiplexer's name. Tests simulate the Windows build with
`shell::simulate_local`.

A Windows host has no POSIX shell: runtime-mux's `HostShell` sends a script
as `powershell -EncodedCommand` (UTF-16LE base64), so no outer shell can
rewrite it. thurbox runs no git on a host. `git::reportable_stderr` drops
OpenSSH's post-quantum advisory from a local git that reaches a remote over
ssh.

psmux is a native-Windows tmux clone (ConPTY) that speaks the same control-mode
protocol, `%N` pane ids and `-L` sockets. Its divergences are each a body in
the psmux adapter, never a branch on the binary's name. **Read the psmux
divergences subsection of ADR-13 in `docs/thurbox/architecture.md` before
touching this path.** In short:

- Naming or spawning a pane needs psmux ≥ 3.3.8, checked by `admit_server`.
- Keystrokes are re-encoded from primitives psmux supports
  (`psmux_send_keys_commands`).
- psmux does not join `new-window` trailing tokens or honour its `-e`, so env
  and command fold into one PowerShell script (`psmux_window_powershell`),
  launched as `powershell.exe -NoLogo -EncodedCommand …`. A script past
  `shell::WINDOWS_CMD_LINE_MAX` (8191) is refused.
- One-shot commands are quoted for a `cmd.exe` or PowerShell login shell
  (`TmuxTransport::with_shell` → `shell::HostShell::launch`).
- psmux has no per-window options, so the ADR-25 window stamp is neither
  written nor read there.
- psmux answers the argv `attach-session` with no `%begin`/`%end` block, so
  `ControlMode::start` drains one only where `implicit_attach_reply` says one
  is sent.
- psmux has no `remain-on-exit`, and answers no `-P`: a spawn learns its pane id
  by diffing the `tb-<name>` panes listed before and after `new-window`.
- psmux rejects tmux's `=window` exact-name target and can print `ERROR:` on
  stdout while exiting 0; structured listings reject such output.
- A cold `new-session -d` can refuse or return before a server answers, so the
  adapter probes with `list-windows` and retries transient `no server running`
  replies with a bounded final wait. Global `set-option -g` commands include
  `-t <session>`.

RMUX ≥ 0.10.0 is opt-in. Installation, version evidence and protocol limits
live in `docs/thurbox/config.md` → *Multiplexer requirements and RMUX setup*.

## Failure classification

- **"Holds nothing" and "did not answer" are different answers.**
  `Server::discover_answered` answers empty only on the multiplexer's own
  refusal; an unreachable host is an error.
- **The layer decides, not the wording.** `ssh` exits 255 for its own failures
  and passes a remote command's status through, so 255 means the question
  never arrived. `wsl.exe` has no such convention.
- **Do not widen `answered_absent`.** Each adapter answers it in its own
  multiplexer's exact words, matched from the first byte
  (`tmux_answered_absent` is the default). Only `(No such file or directory)`
  is absence; `(Permission denied)`, `(Connection refused)` and
  `(Connection reset by peer)` are a server that may be alive.
- **Known limit.** tmux's absence is a claim about the socket, not the
  machine. A server bound to a missing path in a directory thurbox looks in is
  brought back with `SIGUSR1` (ADR-34); one moved elsewhere is reachable only
  through that socket.

## Testing

`scripts/dev/e2e/linux-container.sh up` starts a throwaway Podman container
(sshd + tmux + git) for the SSH path; `scripts/dev/e2e/windows-vm.sh test`
probes psmux delivery. `wsl.exe`'s argument passing is not exercised in CI (no
WSL runner); its construction is unit-tested (`transport::tests::wsl_*`).
