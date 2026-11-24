# runtime-herdr

`runtime-herdr` (`crates/runtime-herdr`) is a runtime for people who use
herdr. It runs each Fleet Task in a terminal of
[herdr-core](https://github.com/herdrdev/herdr-core), the terminal runtime
that herdr's desktop app is a client of. Fleet drives it through
[the runtime protocol](README.md#the-runtime-protocol), as it drives
`runtime-mux`. Fleet does not link it.

Its configuration is in
[Fleet's configuration](../fleet/configuration.md#places-on-runtime-herdr).

## How it reaches herdr-core

```text
Fleet ── runtime-herdr <verb> ──▶ herdr-core connect      JSON-RPC 2.0, one message per line
                                    │                      (starts the core when it does not run)
                                    ▼
                                  herdr-core ── holder ── terminal t7
                                                           labels: fleet.key=<key>
                                                                   fleet.place=<place>
                                                                   herdr.space=<space>
                                                                   herdr.tab=<key>
```

- Each call runs `herdr-core connect` and speaks herdr-core's API over its
  stdin and stdout. `connect` starts the core when it does not run.
- The first request is `hello` with protocol generation 1. A core of another
  generation answers Unknown.
- A place's `session` selects a herdr-core session (`HC_SESSION`): a separate
  core, with terminals of its own.
- The runtime needs herdr-core 0.1.22 or newer. CI tests it with the version
  that `nix/herdr-core.nix` pins.

## The verbs

`runtime-herdr` speaks protocol 2. It answers these verbs:

| Verb | What it does in herdr-core |
| --- | --- |
| `manifest` | Reports each place asked about. A place is ready when its core answers `hello`. As every verb does, it starts the core when the core does not run. |
| `start` | Writes the key's start record, then `terminal.run` with the argv, `env` and `dir`, and the labels above, then records the terminal's address and pid. A live terminal of the key, with the process `start` started, is adopted. So is the live terminal of a start cut short before it recorded the address. |
| `inspect` | `running` while the key's terminal is live and holds the process `start` started. `exited` with the terminal's exit code once it has finished, or with no code when the core no longer has it. `none` when the key has no start record. |
| `stop` | `terminal.kill`, then `terminal.rm`, for each terminal of the key at the place. `release: true` also releases the key's workspace at the place. |
| `list` | Every key with a start record at the place, and every key that holds a workspace. |
| `workspace` | Runs `workspace-git`, as `runtime-mux` does, under a root of the place's own. |

The environment goes over the API. It is in no process's argv.

## Persistence

herdr-core's holder processes own the terminals, so a terminal keeps its
process while the core restarts or updates.

| Event | What `inspect` answers |
| --- | --- |
| A core restart or update | `running`: the process is the same. |
| A core stop, a reboot or a lost holder | `exited`: the core marks the terminal finished. Fleet revives the Task. |
| The core removes a finished terminal (after 24 h) | `exited` with no code, from the start record. |
| herdr's app keeps a terminal (`keep`) and the core gives it a new shell | `exited`: the start record holds the first process's pid, and the shell has another. The next `start` ends the shell. |

`inspect` never answers `none` for a key that `start` started and `stop` did
not stop: Fleet reads `none` as a gone worker and does not revive it.

## herdr's desktop app

herdr's app shows each terminal that has the labels `herdr.space` and
`herdr.tab`, and makes a space or a tab for an id it does not know
(herdr-app's `defaults/AGENTS.md`). So a Task appears in the app, in the
space that the place's `space` names, in a tab named after its key. This
was not checked with the app in this repository's tests: they check only
the labels.

## Files

| Path | Content |
| --- | --- |
| `${XDG_STATE_HOME:-~/.local/state}/runtime-herdr/<core>/<place>/<key>.start` | The start record: the key's terminal address and its process's pid. Before `terminal.run`, it holds neither. |
| `${XDG_STATE_HOME:-~/.local/state}/runtime-herdr/<core>/<place>/lock` | Starts and stops at one place hold this lock, one at a time. |
| `${XDG_DATA_HOME:-~/.local/share}/runtime-herdr/workspaces/<place>/` | The workspaces that `workspace-git` makes for the place. |

`<core>` is the core's id from `hello`, so two herdr-core sessions keep
separate records. `<place>` is the place's name. With it and the
`fleet.place` label, two places on one session each start, stop, list and
release only their own terminals and workspaces. The state directory is
`0700`.

## Limits

- Linux and macOS only. On another system, `manifest` reports each place
  not ready.
- Places are herdr-core sessions on this machine. A herdr-core machine
  (`hc machine`) is not a place.
- The herdr 1 terminal interface (the `herdr` program) does not share
  herdr-core's terminals, so it does not show the Tasks.

## Tests

`crates/fleet-cli/tests/e2e_runtime_exec.rs` runs a real herdr-core on a
private runtime directory (`XDG_RUNTIME_DIR`):

- `fleet runtime check` passes against `runtime-herdr`;
- an exited process answers its exit code;
- a key stays running across a core restart, and a start adopts it;
- a key lost to a core stop answers `exited`, and the next `start` runs it
  again;
- a key's terminal has its environment and the labels herdr's app reads;
- two places on one session list only their own keys, and each keeps its
  own terminal and workspace for a key;
- a start cut short before it recorded the terminal is adopted;
- a key that holds only a workspace is listed.
