# Runtimes

A runtime is any program that runs one keyed process at a place and tells
Fleet about it.
Fleet reaches every session through it and links no runtime's code.
`runtime-mux` (`crates/runtime-mux`) is the default runtime. How the runtime fits between thurbox
and Fleet is the [overview](../README.md).
How `runtime-mux` reaches hosts, where psmux differs from tmux, and its
status channel are [runtime-mux.md](runtime-mux.md).

This page owns two contracts and one verb:

- [the runtime protocol](#the-runtime-protocol), how Fleet drives a runtime;
- [the terminal stream](#the-terminal-stream), how a view attaches to a
  session on any runtime;
- [workspaces](#workspaces), how a list of URLs becomes the directories a
  session works in.

## The runtime protocol

A runtime is any program. Fleet runs it through `fleet-exec`, one bounded
process per call: `<program> <verb>`, a JSON request on stdin, and on stdout
`{"known": …}` or `{"unknown": {"why": …}}`. Unknown never concludes.

| Verb | Required | Answer |
| --- | --- | --- |
| `manifest` | yes | protocol version and the verbs supported |
| `start` | yes | adopts the one live process holding the key, else starts `argv` with `env` in the workspace made under that key |
| `inspect` | yes | `None`, `Running{id}`, `Exited{id, code}` |
| `stop` | yes | a process already gone counts as stopped; `release: true` also removes the key's workspace |
| `list` | no | every key the runtime holds a process or a workspace for, for the collector |
| `input` | no | types text, optionally submits |
| `capture` | no | screen text, cursor, foreground process, title |
| `exec` | no | a bounded command, `in: host` or `in: workspace` (inside the place), deadline and output cap |
| `workspace` | no | `{key, urls}` → one directory per URL, as the process sees it ([workspaces](#workspaces)) |
| `attach` | no | `{stream: {argv}}` or `{tty: {argv}}`: how to open a terminal stream ([the terminal stream](#the-terminal-stream)) |

Every verb is idempotent by key. The runtime knows no harness, Task or
journal: pause is `stop`, resume and revive are `start` with the harness's
resume argv, idle comes from agent hooks, delivery is proven by the agent's
prompt hook. Fleet reads an agent's state off the `capture` screen with its
own rules, below the hooks; the runtime only returns the screen.

Fleet's config names each runtime in `[runtimes.<name>]`, a `program`, and
each host in `[hosts.<name>]`, the runtime serving it, so a Task's
`spec.host` chooses the program ([configuration](../fleet/configuration.md#native-fleet-configuration-fleettoml)).
[Places](#proposed-places-protocol-version-2) are planned to replace
`[hosts]`.

### Protocol version 1

`runtime-mux` and the reference script answer it, Fleet's `exec` runtime
kind speaks it, and `fleet runtime check` holds both to it in CI. Fleet calls
`workspace` before a Task's first start, bounded by the runtime's
`workspaceTimeoutMs`. It reads the manifest's `workspace.schemes` first and
refuses a URL whose scheme is not listed, without a call. `fleet attach` asks
`attach` once the runtime's `manifest` declares it.

Every request is a JSON object carrying `"protocol": 1`; `manifest` answers
whatever the request says. A runtime that does not speak the version asked
answers Unknown. An answer is `{"known": …}` or `{"unknown": {"why": …}}`
on stdout, with exit status 0; anything else (a program that is not there, a
non-zero exit, a timeout, output that does not parse) is Unknown to Fleet.
A key is 1 to 128 ASCII letters, digits, `.`, `-` or `_`; any other answers
Unknown.

| Verb | Request (besides `protocol`) | Known answer |
| --- | --- | --- |
| `manifest` | — | `{"protocol": 1, "verbs": [...]}`, and `workspace: {schemes, survivesStop}` with the `workspace` verb |
| `start` | `key`, `argv`, `env` (object), `dir` (absolute, optional) | `{"started": {"id", "adopted"}}`, or `{"failed": {"why"}}` when nothing can have started, such as a missing multiplexer |
| `inspect` | `key` | `"none"`, `{"running": {"id"}}` or `{"exited": {"id", "code"}}`, `code` null for a signal |
| `stop` | `key`, `release` (optional) | `"stopped"`, whether or not a process was there |
| `list` | — | `{"keys": [...]}` |
| `input` | `key`, `text`, `submit` | `"sent"`, or `"none"` when no process holds the key |
| `capture` | `key`, `lines` (optional) | `{"screen": {"text", "cursorRow", "cursorCol", "foreground", "title"}}`, or `"none"`; `title` is the one the program set (OSC 0 or 2), and a runtime that cannot read it omits it |
| `exec` | `argv`, `in` (`host`), `dir`, `timeoutMs`, `outputCap` | `{"exited": {"code", "stdout", "stderr"}}`, `{"timedOut": {"stdout", "stderr"}}` or `{"failed": {"why"}}` |
| `attach` | `key` | `{"stream": {"argv"}}`, `{"tty": {"argv"}}`, or `"none"` when no process holds the key |
| `workspace` | `key`, `urls` | `{"made": {"dirs", "adopted"}}`, `{"unsupported": {"url"}}` or `{"failed": {"why"}}` ([Workspaces](#workspaces)) |

`runtime-mux` answers every verb above. It runs its windows on a tmux
server of its own, socket `runtime-mux` (`RUNTIME_MUX_SOCKET` names
another), never on thurbox's interface socket: an inherited
`THURBOX_SOCKET` is ignored. A process is a window named after its key, and
a pane kept after its program exits is how `inspect` reports the code; a
`start` replaces it. Starts and stops on one server run one at a time, so
two starts of one key make one window: they hold a kernel lock on a file in
the private directory tmux keeps the socket in (`<TMUX_TMPDIR or
/tmp>/tmux-<uid>`, made `0700` when missing and refused when another user
can write to it), opened without truncating or following a link. `exec` runs `in: host` only, on this
machine, and kills the command's process group at the deadline. It reads
each stream to its end and keeps `outputCap` bytes, so a long output does
not change how the command ends; a child that left the group and holds a
stream open does not hold the answer past the deadline. The `runtime-mux`
program serves this machine only; the crate's ssh and WSL hosts
([runtime-mux.md](runtime-mux.md)) serve thurbox, which links it. It makes
workspaces by running `workspace-git` on this machine
([its workspaces](#runtime-muxs-workspaces)). A workspace call takes a lock
of its key only, never the server's lock: a first clone or a long setup step
does not hold up a start or a stop of another key. A stop that releases a
workspace takes the key's lock first, then the server's, and lets go of the
server's lock once the process is stopped: the teardown and the release hold
the key only.

thurbox's stream pane (`stream_panes` in its `settings.toml`) is a view: it
opens a local tmux pane through `runtime-mux view --scrollback <lines> --pane
<id>` on its own socket, with its own `scrollback_lines`, and decodes the
frames into the chunks its control-mode pane gives.
Size requests still go to the server through thurbox's own backend.

`runtime-mux` answers `attach` with a `stream`: its own `view` on the key,
naming its socket and `TMUX_TMPDIR`. `runtime-mux view` adopts the key's pane
through its control-mode client, sized by the view's first `resize`, and
encodes what the server sends as frames; a `detach` lets go of the pane and
leaves the process running. On a multiplexer that snapshots a pane in step
with its output (tmux, rmux), the first screen is the first snapshot or
resync, and a `state` frame follows it with the title and modes set before
the view came. On one that cannot (psmux), `hello` names no `snapshot` and
the history and that state arrive as output. The reference script answers `attach` with a raw
tmux client on the key's pane, a `tty` answer. An `attach` argv runs where the view runs, so it names
everything it needs, such as a socket by its path.

What Fleet's `exec` kind does with the answers:

- `start` runs the harness profile's launch argv
  ([harness profiles](../fleet/controllers/task.md#harness-profiles)), with
  `FLEET_SIGNAL_FILE` in `env`;
- a session's id is its key, so it outlives each process started under it;
- readiness, workspace trust and an idle agent come from the agent's signals:
  an agent that reported anything but `blocked` is ready and past any trust
  dialog, and a running agent is at rest once it last reported `idle`. An
  agent that reports nothing before its first turn is read off its
  `capture` screen as its harness profile says
  ([harness profiles](../fleet/controllers/task.md#harness-profiles)).
  Before its first report, a trust dialog on its `capture` is Untrusted, and
  anything else Unknown, so a brief waits;
- delivery is proven by the agent's prompt hook, not by `input`;
- the agent state of a session whose hooks do not say it now is read off its
  `capture` text and title with the harness's screen rules
  ([screen rules](../fleet/controllers/task.md#screen-rules)), at most one
  capture per session every 2 s. A screen state is for a view: readiness,
  rest and delivery still wait for the hooks;
- an `exited` process is lost, since a `stop` leaves `none`: revive is `start`
  with the profile's resume argv on the conversation the hooks reported. Fleet
  first marks the restart in the signal file, so readiness and trust wait for
  the new process's own report;
- `none` proves nothing about a pause, so Fleet never pauses a session on this
  kind: version 1 has no stop that keeps the key.

Agent hooks append to the file Fleet passes as `FLEET_SIGNAL_FILE` in `start`.
Fleet reads it directly on a local host, so a local runtime needs nothing for
signals. A session on another host gets no file: reading one back through the
runtime's `exec` is not implemented, so what Fleet would learn from it, such as
an idle agent or a delivered prompt, stays Unknown, and Unknown never
concludes.

`fleet runtime check -- <program> [args]` is the conformance suite. It prints
one line per case, `ok` or `FAIL` with why, and exits non-zero when any fails:

- `manifest` names protocol 1 and the four required verbs;
- a protocol mismatch answers Unknown;
- an error (a request that is not JSON, a bad key, an unknown verb) answers
  Unknown, never None;
- `inspect` of an absent key answers None, and a stop of it succeeds;
- concurrent starts with one key leave one process: four at once answer
  one id, and only one of them made it;
- two starts with one key leave one process: the same id, the second
  adopted, and the key once in `list` when the runtime has it;
- a stop ends the process;
- an exited process answers Exited with its code, or None;
- with `workspace` declared: two calls with one key answer the same
  directories, the second adopted, and a release leaves a `file://` directory
  in place.

The cases start `sleep` and `sh` in the runtime's place. CI runs it, through
`crates/fleet-cli/tests/e2e_runtime_exec.rs`, against `runtime-mux` and against
[`examples/runtimes/tmux.sh`](../../examples/runtimes/tmux.sh), a reference
runtime in one POSIX `sh` script over tmux and `jq`. It serializes starts and
stops with a kernel lock in tmux's private socket directory, taken with
`flock(1)` or else `perl`, which a killed holder releases.

### Proposed: places (protocol version 2)

**Status**: proposed. No runtime and no Fleet speaks it yet. Version 1,
above, is the protocol in force. The decision is
[ADR-42](../thurbox/architecture.md#adr-42-hosts-belong-to-runtimes).

A place is where a runtime runs a process: this machine, an ssh host, a WSL
distro, a VM. A host's id is `<runtime>/<place>`. `local` is the short form of
`mux/local`.

```text
Fleet config                        Fleet                 runtime
[runtimes.mux]                        │                      │
program = "runtime-mux"               │ manifest {places}    │
[runtimes.mux.places.devbox] ────────▶├─────────────────────▶│ checks each place
kind = "ssh"                          │◀─────────────────────┤ each place: ready, why
destination = "me@devbox"             │                      │
                                      │ start {key, place}   │
Task spec.host = "mux/devbox" ───────▶├─────────────────────▶│ runs it on devbox
                                      │ place = {name,config}│
```

Changes from version 1:

- **`place` on every request.** Every request but `manifest` carries
  `place: {name, config}`, so `list` answers for one place. `config` is the
  place's table from Fleet's config, as JSON. Fleet does not read it. A
  runtime that gets a config it cannot use answers Unknown with why. The
  runtime keeps no place config of its own.
- **`manifest` reports places.** The request carries `places`, every
  configured place with its config. The answer adds
  `places: [{name, platform, dynamic, ready, why}]`:
  - `platform` is `posix` or `windows`;
  - `dynamic` is true when each key gets a new machine, such as a VM;
  - `ready` is false when the place lacks something, and `why` names it and
    the command that fixes it;
  - the runtime may add places it discovers, such as WSL distros, with an
    empty config.

  A runtime with no `[runtimes.<name>.places]` table has one place,
  `default`, with an empty config. An unreachable runtime makes its places
  Unknown, never absent.

New optional verbs:

| Verb | Request (besides `protocol`, `place`) | Known answer |
| --- | --- | --- |
| `signals` | `key`, `from` (a byte offset) | `{"lines": {"text", "next"}}`: the signal file's lines from `from`, and the offset to ask from next |
| `shell` | `key` | `{"stream": {"argv"}}` or `{"tty": {"argv"}}`: a shell in the key's directory on its place, opened like `attach` |
| `browse` | `dir` (absolute, optional: the place's home) | `{"listing": {"dir", "entries": [{"name", "dir", "repo"}]}}`, or `{"failed": {"why"}}` |

- **`signals`.** The runtime sets `FLEET_SIGNAL_FILE` to a file on the place
  and reads it back for Fleet. Fleet asks from the last `next` it got. A
  place without the verb keeps today's rule: what the file would say stays
  Unknown.
- **`shell`.** thurbox's companion shell on a remote place opens through
  Fleet with this answer. thurbox keeps no ssh launcher of its own.
- **`browse`.** The new-session modal browses a place's folders through it,
  and still takes a clone URL, whose branches `git ls-remote` lists on this
  machine. `dir` and `repo` say whether an entry is a directory and whether
  it is a git checkout. A dedicated verb is simpler than `exec` here: an
  `exec` answer is the text of the place's own command, so the reader would
  need one parser per platform. The runtime knows its place, and a VM runtime
  can answer from its own API.

### What a runtime is

A runtime answers one question: run this command, under this key, in this
directory, at this place, and tell me about it. Where and how is its own
business.

`runtime-mux` (`crates/runtime-mux`) runs its processes on this machine, in
tmux, psmux or rmux (ADR-31). A new multiplexer of the tmux family is a
module in `runtime-mux`, not a runtime. A new kind of place is a runtime. No
`runtime-*` crate depends on another; shared code is a crates.io dependency.

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
first `start` answers the missing dependency by name, and other runtimes are opt-in in `[runtimes.<name>]`.

## The terminal stream

![The terminal stream: fleet attach answers a stream or a bridged tty command, the view exchanges frames with it, and runtime-mux view or fleet term-bridge serves it](terminal-stream.svg)

A tmux server keeps the screen, accepts several clients and arbitrates their
sizes; a raw interactive command such as `docker attach` does none of that.
"Run this command in a pty" is therefore not an interface: what a view gets
would depend on the runtime.

What a view needs from a live session, whatever runs it:

1. the screen as it is now, on connect and after a dropped stretch;
2. the output that follows, in order, with size changes at their place in it;
3. a way to type, paste and resize;
4. to leave without stopping the process, and to come back;
5. other viewers at the same time, with one rule for who sizes the screen.

### The protocol

A stream is a process. The view starts it and speaks over its stdin and
stdout; it ends when either side closes. A frame is a type byte, the
payload's length as a big-endian 32-bit number, then the payload: `output`,
`state` and `input` are raw bytes, `paste` is UTF-8 text, and every other
payload is a JSON object. The vocabulary is `PaneChunk` (`crates/runtime-mux/src/contract.rs`),
which carries every item in the output's order on one channel:

| Direction | Type | Frame | Payload |
| --- | --- | --- | --- |
| down | `0x01` | `hello` | `{version, capabilities}`, capabilities from `snapshot`, `scrollback`, `viewers`, `resize` |
| down | `0x02` | `snapshot` | `{cols, rows, cursor: [col, row], normal, alternate}`: normal-screen history and rows, alternate rows or null, styled |
| down | `0x03` | `output` | bytes |
| down | `0x04` | `resized` | `{rows, cols}` |
| down | `0x05` | `sized_elsewhere` | `{elsewhere}`: whether another viewer sizes the screen |
| down | `0x06` | `resync` | a snapshot replacing the view's grid after dropped output |
| down | `0x07` | `state` | title and input modes as terminal bytes, to parse but not count as output |
| down | `0x08` | `exit` | `{"known": {code}}`, `code` null for a signal, or `{"unknown": {why}}` |
| up | `0x81` | `input` | bytes, sent as typed |
| up | `0x82` | `paste` | text, bracketed when the program asked for it |
| up | `0x83` | `resize` | `{rows, cols}`: a request the holder of the screen arbitrates |
| up | `0x84` | `detach` | empty: leave; the process keeps running |

A frame of a type the reader does not know, or a payload over 64 MiB, ends the
stream.

A view's first frame is a `resize`: the end that keeps the screen starts the
stream once it knows the size. `snapshot` comes next when the capability is
there, so a view never starts on an empty grid. Sizing follows ADR-27 everywhere: the viewer
being typed into sizes the screen, the others get `sized_elsewhere`.

`term-stream` (`crates/term-stream`) is a crate with only the frame types and
their codec: no process, no runtime, no transport. thurbox, Fleet and every
`runtime-*` may depend on it, as they may on `serde`.

### The runtime's `attach`

`attach` answers how to open a stream, in one of two shapes:

- `{"stream": {"argv": […]}}`: the command speaks the protocol. This is the
  full answer.
- `{"tty": {"argv": […]}}`: a raw interactive command, for a runtime that has
  nothing better (`docker attach`, `virsh console`, `kubectl attach -it`).

`fleet attach` always hands a view a stream. It passes a `stream` answer
through, and wraps a `tty` answer in `fleet term-bridge -- <argv>`, which runs
the command on a pty of its own and relays it as frames. A bridge keeps no
screen: it has nothing from before it started, so its `hello` names no
`snapshot`, and the view's first `resize` sizes the pty the command starts on,
which is what makes a program draw. That is the price of a runtime without a
screen keeper. A `paste` is bracketed when the command's output last asked
for bracketed paste. A `detach` ends the bridged command, such as a
`docker attach` client, and not the process it reached, even after the
command closed its terminal.

### Who keeps the screen

| Runtime | Screen keeper | `attach` answers |
| --- | --- | --- |
| `runtime-mux` | tmux, psmux, rmux | `stream`: `runtime-mux view <key>`, control mode encoded as frames |
| any runtime, minimal | none | `tty`: bridged by Fleet, no history; the reference script answers it |

## Workspaces

![Workspaces: Fleet passes a list of URLs and gets directories back; each runtime makes them its own way, with workspace-git for git URLs; release preserves work and never removes what it did not make](workspaces.svg)

Fleet hands a runtime a list of URLs and gets one directory per URL back
([the Fleet side](../fleet/architecture.md#workspaces)). Everything else is
the runtime's: where the files live, how they are made, what is installed,
how work is kept when they go. That is where a workspace is customized, by
configuring a runtime or by writing one.

### The verb

`workspace {key, urls}` answers one of:

| Answer | Meaning |
| --- | --- |
| `Made{dirs, adopted}` | one directory per URL, in order, as the process sees it; `adopted` when the key's workspace already existed |
| `Unsupported{url}` | this runtime does not handle that URL |
| `Failed{why}` | nothing was made, or what was made was rolled back |

`stop {key, release: true}` removes the key's workspace with the process.

A runtime that implements `workspace` holds to these rules, and
`fleet runtime check` tests each:

- **Idempotent by key.** A second call with the same key answers the same
  directories, adopted. A partial workspace is never answered: the next call
  rolls it back and makes it again.
- **Never removes what it did not make.** A `file://` directory is used as it
  is and survives every release.
- **Never loses work on release.** Anything the workspace holds that exists
  nowhere else (uncommitted changes, unpushed commits) is preserved before the
  files go, in a durable place outside the workspace. How is the runtime's
  choice. Files the repository ignores are not work: it declares them
  disposable.
- **Never waits for a person.** Release always finishes or answers Unknown.

The `manifest` declares what Fleet may rely on:

- `workspace.schemes`: the URL schemes the runtime handles, such as
  `git+https`, `git+ssh`, `git+file`, `file`.
- `workspace.survivesStop`: the files outlive the process. When false, Fleet
  never pauses an idle session on that runtime, because pause is stop.

A runtime without `workspace` runs only Tasks with an empty list.

### URLs

The URL is the whole request: Fleet adds nothing to it.

| URL | What `workspace-git` makes |
| --- | --- |
| `git+https://host/org/repo.git` | a worktree on a new branch from the default branch |
| `git+https://host/org/repo.git#v2` | the same, from `v2` |
| `git+ssh://git@host/org/repo.git?branch=feat-x#main` | a worktree on `feat-x`, made from `main` when it does not exist |
| `git+file:///home/me/app?branch=feat-x#main` | a worktree of that checkout itself, on its own `feat-x` |
| `git+file:///home/me/app#refs/fleet/preserved/<key>/app` | a worktree on work an earlier release preserved in that checkout |
| `git+file:///srv/mirrors/repo.git#refs/fleet/preserved/<key>/repo` | a worktree on work an earlier release preserved in a bare repository |
| `file:///home/me/notes` | nothing: the directory, as it is |

Restoring preserved work is therefore a new Task whose URL names the preserved
ref; Fleet needs no verb for it.

### workspace-git, the default for git URLs

`workspace-git` is a small program a runtime runs where its files belong. No
runtime links it: each runs the program, so it runs where the runtime's files
are. Its layout:

```text
<root>/<key>/<name>/                  one real worktree per git URL
<root>/<key>/.fleet-workspace.json    written last; without it the workspace is partial
<root>/<key>/.fleet-sources           the checkouts the key's worktrees belong to
<root>/.sources/<digest>.git          a bare source per remote repository, fetched before each make
<root>/.sources/<digest>.lock         one lock per repository a worktree is made from
<root>/.checkouts                     the local checkouts worktrees were made from
```

- A `git+file` URL that names a local checkout, a repository with a working
  tree, gets a worktree of that checkout itself. Nothing is fetched or
  copied:
  - an existing branch is the checkout's own branch;
  - a new branch is made in the checkout;
  - a commit made in the worktree is in the checkout at once;
  - `git -C <checkout> worktree list` lists the worktree.
- Any other git URL, a remote or a bare repository, gets a bare source of its
  own. A source fetches its remote's branches into `refs/remotes/origin/*`,
  and its tags. A fetch therefore never moves a branch that a worktree has
  checked out. Every branch in a source's `refs/heads/` is one that
  `workspace-git` made.
- Each worktree belongs to its repository, the checkout or the source, so its
  branch and its preserved ref are there.
- One lock per repository orders every make, release, rollback and retention
  on it, across runtimes that share a root.
- With one URL the session starts in `<root>/<key>/<name>`; with several the
  first URL's worktree is still the start, and every worktree is a real
  directory. Two URLs with one name get
  `<name>-2`.
- With no `branch`, the branch is named after the key. An existing branch is
  checked out, and one checked out elsewhere gets `<branch>-<n>`, from 2.
- A fragment names the base: a remote branch, a tag, a branch of the source,
  or a ref or commit as given, in that order. A checkout reads its own
  branches before its remote's. A `refs/…` fragment that a fetch
  of branches does not bring, such as preserved work, is fetched by name.
  With no fragment, the base is the remote's default branch; a checkout with
  no remote starts from the branch it has checked out.
- A `file://` URL takes no query and no fragment, and names an existing
  absolute directory. A `%XX` in a path is decoded.
- A key holds one workspace: the same key with other URLs fails.
- On release, a dirty worktree is committed, untracked files included,
  through a temporary index onto `refs/fleet/preserved/<key>/<name>` in its
  repository; then the worktree goes. The temporary index lives in the key's
  directory, never in a checkout's working tree. Branches are never deleted
  on release. The release of a
  key that holds no workspace succeeds.
- On each make it prunes its own retention: preserved refs older than
  `preserve.days` (default 30), in sources and in checkouts, and a source's
  branches not checked out whose head the remote already has. A checkout's
  branches are the user's and are never deleted. A repository another call
  holds is left for the next make.
- Git never waits for a person: `GIT_TERMINAL_PROMPT=0`, and ssh runs in
  batch mode unless `GIT_SSH_COMMAND` or `GIT_SSH` names another command.

The program takes one JSON request on stdin and prints one answer:

| Call | Request | Answer |
| --- | --- | --- |
| `workspace-git make` | `{root, key, urls, preserveDays?}` | `{"made": {dirs, adopted}}`, `{"unsupported": {url}}` or `{"failed": {why}}` |
| `workspace-git release` | `{root, key}` | `"released"` |
| `workspace-git discard` | `{root, key}` | `"discarded"`: the worktrees go, nothing is preserved, for a workspace no session worked in |
| `workspace-git inspect` | `{root, key}` | `{"workspace": {urls, dirs}}`, or `"none"` |

Any other answer is `{"unknown": {why}}`, such as a rollback that did not
finish, and the caller concludes nothing from it. `root` is an absolute
path.

### Customizing a workspace

Each runtime offers what fits its places: `runtime-mux` runs setup and
teardown steps ([its workspaces](#runtime-muxs-workspaces)). A need no
configuration covers is a runtime of one's own: a script that answers
the protocol, held to the same rules by `fleet runtime check`.

### runtime-mux's workspaces

`runtime-mux` runs the `workspace-git` program on this machine, so a session
sees the same paths. Its own config file holds the settings: the file at
`RUNTIME_MUX_CONFIG`, else `${XDG_CONFIG_HOME:-~/.config}/runtime-mux/config.toml`.
A missing file gives the defaults. An unknown key is refused.

```toml
[workspace]
root = "/home/me/.local/share/runtime-mux/workspaces"
program = "workspace-git"
preserve.days = 30

[[workspace.setup]]
match = "git+https://github.com/acme/app.git"
run = ["npm ci"]
```

| Key | Default | Meaning |
| --- | --- | --- |
| `workspace.root` | `${XDG_DATA_HOME:-~/.local/share}/runtime-mux/workspaces` | where `workspace-git` makes workspaces; an absolute path |
| `workspace.program` | `workspace-git` beside the `runtime-mux` program, else on `PATH` | the program it runs |
| `workspace.preserve.days` | 30 | how long preserved work is kept |
| `workspace.setup`, `workspace.teardown` | none | steps, as above |

- A step matches a URL as written, or the URL without its query and fragment.
  Its commands run in order, through `sh -c`, in the directory made for that
  URL.
- Setup runs only in a workspace the call made, not in one it adopted. A
  failing command discards the workspace, with nothing preserved, and the
  call answers `failed`, naming the command and its exit code.
- `stop {release: true}` stops the process first, then runs the teardown
  steps, then releases the workspace. A failing teardown command does not
  stop the release.
- A `workspace-git` that does not start answers `failed`: nothing was made.
  One that starts and gives no answer is Unknown.
