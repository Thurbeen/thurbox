# runtime-herdr

`runtime-herdr` (`crates/runtime-herdr`) is a runtime for people who use
[herdr](https://github.com/herdrdev/herdr). It runs each Fleet Task in a
pane of the user's own herdr. The user sees the Task in herdr's interface,
with herdr's agent status. Fleet drives it through
[the runtime protocol](README.md#the-runtime-protocol), as it drives
`runtime-mux`. Fleet does not link it.

Its configuration is in
[Fleet's configuration](../fleet/configuration.md#places-on-runtime-herdr).

## How it reaches herdr

```text
Fleet ── runtime-herdr <verb> ──▶ herdr status server --json   finds the socket
                                  herdr server                 starts it when it is not running
                                  herdr's socket API           JSON lines, one request per connection
                                    │
                                    ▼
                    herdr workspace "fleet" ── tab <key> ── pane <key>: the Task's process
```

- `runtime-herdr` asks `herdr status server --json` for the server's socket.
  A place's `session` selects a herdr named session (`herdr --session`).
- When the server does not run, `start` starts it with `herdr server`, in
  a process group of its own. The other verbs do not start it.
- Each other call is one request on herdr's socket API.
- The runtime needs herdr 0.9.1 or newer. CI tests it with herdr 0.9.5,
  the version `nix/herdr.nix` pins.

## The verbs

`runtime-herdr` speaks protocol 2. It answers these verbs:

| Verb | What it does in herdr |
| --- | --- |
| `manifest` | Reports each place asked about. A place is ready when `herdr status` answers. |
| `start` | Makes a tab labelled with the key, in the place's herdr workspace, with `layout.apply`. The tab's pane, also labelled with the key, runs the argv with the request's `env` and `dir`. A live pane of the key is adopted. |
| `inspect` | `running` while the key's pane holds the process that `start` started. Else `exited`, or `none` when the key has no start record. |
| `stop` | Closes every pane labelled with the key, and removes the key's records. `release: true` also releases the key's workspace. |
| `list` | Every key with a start record. |
| `workspace` | Runs `workspace-git`, as `runtime-mux` does. |

The environment goes over herdr's socket. It is in no process's argv.

## What herdr cannot do, and what the runtime does instead

| herdr | runtime-herdr |
| --- | --- |
| herdr keeps no exit code: a pane whose process exits is removed. | The pane runs the argv under `/bin/sh`, which writes the exit code to a file. `inspect` reads it. A process that a signal ends reports 128 plus the signal's number, as `sh` gives it. |
| A herdr restart ends every process and brings back the layout only, with new shells in the old panes. | `start` records the pane and its herdr terminal id. A restored pane has a new terminal id, so `inspect` answers `exited` and Fleet revives the Task. The next `start` closes the restored pane. |
| A reboot ends the server. | The start record survives it, so `inspect` answers `exited`, never `none`. |

`inspect` never answers `none` for a key that `start` started and `stop` did
not stop: Fleet reads `none` as a gone worker and does not revive it.

## Files

| Path | Content |
| --- | --- |
| `${XDG_STATE_HOME:-~/.local/state}/runtime-herdr/<server>/<key>.start` | The start record: the key's pane id and terminal id. |
| `${XDG_STATE_HOME:-~/.local/state}/runtime-herdr/<server>/<key>.exit` | The exit code of the key's last process. |
| `${XDG_STATE_HOME:-~/.local/state}/runtime-herdr/<server>/lock` | Starts and stops on one server hold this lock, one at a time. |
| `${XDG_DATA_HOME:-~/.local/share}/runtime-herdr/workspaces/` | The workspaces that `workspace-git` makes. |

`<server>` is a digest of the server's socket path, so two herdr sessions
keep separate records. The directory is `0700`.

## Limits

- Linux and macOS only. On another system, `manifest` reports each place
  not ready.
- Places are herdr sessions on this machine. A herdr machine profile
  (`herdr --machine`) is not a place.
- A server that `runtime-herdr` starts runs in the caller's cgroup. Under a
  systemd service, a stop of the service can stop it. Start herdr yourself
  to keep it out of the service.

## Tests

`crates/fleet-cli/tests/e2e_runtime_exec.rs` runs a real herdr on a private
config directory (`XDG_CONFIG_HOME`):

- `fleet runtime check` passes against `runtime-herdr`;
- an exited process answers its exit code;
- a key lost to a herdr restart answers `exited`, and the next `start` runs
  it again.
