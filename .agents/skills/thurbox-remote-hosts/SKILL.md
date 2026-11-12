---
name: thurbox-remote-hosts
description: Remote SSH and WSL hosts in thurbox: the hosts.toml schema (runtime-mux's) and WSL discovery, how a picked host reaches Fleet, and why thurbox reads this machine only and runs only companion shells on a host. Use when working on remote/WSL/Windows hosts in thurbox or on hosts.toml. The transport, psmux and the failure classifiers are crates/runtime-mux's.
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
  session on another host gets a `file://` workspace URL: the creation flow
  reads branches on this machine only, so it never offers a worktree there.
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
  (`holds_a_remote_image_paste`, `src/coordinator/input.rs`).

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
name may not contain `:`. `thurbox-cli config` validates the file.

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

## The transport

The transport, the host's `PATH` probe, psmux's divergences and the failure
classifiers are `crates/runtime-mux`'s; `src/backend.rs` re-exports it and
injects thurbox's settings (`mux_settings`). RMUX ≥ 0.10.0 is opt-in:
`docs/thurbox/config.md` → *Multiplexer requirements and RMUX setup*.
