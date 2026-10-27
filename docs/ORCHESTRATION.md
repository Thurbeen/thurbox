# Orchestration: the control-plane pattern

Design rationale for running agent sessions across more than one repo.
For the primitives themselves, see [FEATURES.md](FEATURES.md); for how
they are built, see [ARCHITECTURE.md](ARCHITECTURE.md).

The maintained implementation is **[Fleet](https://github.com/Thurbeen/fleet)**:
a control plane you install with one command, whose lead session, Mission
Control, turns a goal into a queue of tasks and runs one worker session per
task. Its README and `AGENTS.md` are the operating manual; this file is the
rationale behind the shape, and the thurbox primitives it rests on.

---

## The problem

One repo, one agent session, one task: nothing to orchestrate. The repo
holds the code, the session holds the context, and the pull request is
the record of what happened.

At N repos this breaks in a specific place. The work still lands in the
repos — that part is fine — but the *plan* has nowhere to live. A goal
that spans three repos is not a file in any of them. Neither is the
context that makes the goal legible: what each project is for, which
ones depend on each other, which are dormant. Neither is the log of what
you actually launched last Tuesday and what came back.

In practice that state ends up in a chat transcript, which is not
durable, not diffable, and not readable by the next session. The
question is not "how do I run agents in parallel" — thurbox already does
that. It is **where does the plan live, and what reads it?**

---

## The control plane

Give the plan its own repo. A **control plane** is a repo that holds two
things and nothing else:

- **The map.** An always-current index of your repos (`registry/`),
  generated from your forge accounts so it cannot drift, plus hand-written
  context files (`registry/context/<repo>.md`) holding the judgement a
  generated index can't: what a project is *for*, how it relates to the
  others, what its current goals are.
- **The orchestration.** A task queue (`orchestration/queue/<topic>/`),
  reusable recipes (`orchestration/playbooks/<name>.md`) and one log per
  run (`orchestration/runs/<date>-<topic>.md`).

The defining rule is what the control plane *doesn't* hold:

> **The control plane holds the plan and the log. It never holds the
> workers' branches.**

Each unit of work becomes one thurbox worker session, targeting a real
repo in its own git worktree, driven by a single self-contained prompt.
Workers share no context with the control plane and none with each
other, so every prompt restates the goal, the constraints, and what
"done" means, from scratch.

**Why split generated from hand-written?** They have different failure
modes. An index of repo names, default branches, and archived flags goes
stale the moment you rename something, so it must be regenerated and
never hand-edited. Why a project exists cannot be generated at all, and
it changes on a human timescale. Storing them in one file guarantees
that regenerating the half that must be fresh destroys the half that
must be preserved.

**Why a separate repo?** Because the plan outlives every branch it
spawns. A run log in one of the worker repos would be a foreign artifact
there, would collide with the very branches it describes, and would go
looking for a home the moment the run touches a second repo. Separating
them also makes the invariant enforceable rather than aspirational: if
the control plane has no worktrees, work cannot accidentally happen in
it.

---

## The run loop

Fleet drives it with `uv run fleet queue`:

1. **Topic.** `queue topic add` keeps the prompt verbatim.
2. **Briefs.** `queue add` makes one task per branch, each with a
   self-contained `BRIEF.md`: the goal, the constraints, what done means.
   Workers share no context with the lead or with each other.
3. **Plan and dispatch.** `queue plan` says what is ready and why the
   rest waits; `queue dispatch` launches every ready task at once, each in
   its own worktree on the target repo.
4. **Watch and collect.** `queue watch` folds thurbox's event stream into
   the records; `queue collect` reads the `result.md` each worker writes
   and checks its artifact. Only that closes a task.
5. **Shepherd and refuel.** `queue shepherd` follows every open change
   request and merges only where the operator allowed it; `queue refuel`
   restarts a worker that stopped on a spent quota window.
6. **Land and reap.** When the forge says every artifact merged, the task
   is landed and `queue reap` releases its session.

The run log opens with the topic and stays the lead's: the goal in its
words, the decisions, what went wrong.

---

## Why this is a thurbox pattern

The shape above isn't invented; it's what falls out of thurbox's
primitives once you use them at more than one repo. Each piece is doing
load-bearing work.

### Worktree-per-session

A worker session creates a git worktree on its own branch
(`--worktree-branch`). Two workers on the same repo cannot collide, and
an abandoned worker costs you a directory, not a dirty checkout on a
branch someone else needs. This is what makes "one unit of work, one
session" safe to say — without it, parallelism across a shared checkout
is a merge conflict waiting for a scheduler.

### The lead is a real session

The control plane installs itself as an extension with one long-lived
`[[sessions]]` entry (ADR-21). Two properties follow, and the pattern
needs both.

It **self-heals**: active extensions are recorded in SQLite and their
sessions are recreated at TUI startup and on every `automation tick`.
Delete the lead session and it comes back. A control plane that
evaporates when someone tidies up their session list is not a control
plane.

And because it *is* a real session rather than an ad-hoc terminal, it
can be addressed. Workers can mail it. It has a UUID to hand out as
`--parent`. It gets `THURBOX_SESSION` in its environment like any other
session. The lead being a first-class session is the precondition for
everything in the next two sections.

### A report the worker writes, not polling

Completion is something the worker writes on purpose. In Fleet that is a
`result.md` in the task's queue directory, with an outcome, the artifact
and what the worker learned; `fleet queue collect` reads it and checks the
artifact against the forge, or against git for a pushed commit. A turn
ending is not a task finishing.

A lead you build yourself can use thurbox's mailbox for the same report:

```sh
thurbox-cli message send --to <lead> --kind result --body '<PR url>'
thurbox-cli message inbox --for <lead> --claim --json
```

thurbox injects `THURBOX_SESSION` into each session's environment, and
both `--from` and `inbox --for` default to it, so neither side needs ids.
`send` wakes the recipient unless given `--no-wake`; Fleet's reconciler
uses `--no-wake` to leave Mission Control a note it cannot type in.

**Why not poll `gh pr list`?** Either report beats it:

- It is **exact**. The report comes from a worker that knows it
  finished. A PR query infers completion from a side effect.
- It can say **"not applicable"**. A worker that correctly concludes
  there was nothing to do says so. A PR poll cannot distinguish "no PR
  because there was nothing to fix" from "no PR because the worker is
  still thinking" — and those demand opposite responses from the lead.

Fleet picks the file over the message because a file is durable: it
survives the lead session restarting, and the lead reads it on its own
schedule instead of being interrupted.

### `watch`, for everything that is not a report

A report is how a worker says it finished. `thurbox-cli watch` is how a
driver learns everything the worker was never going to mail — that it went
blocked on a permission, that its pane died, that somebody deleted it:

```sh
thurbox-cli watch --json --initial | while read -r line; do …; done
```

It streams an **append-only log**, not a sampled diff. Every writer that
changes what a watcher reports appends its event in the same transaction as the
change, so two transitions in the same instant are two events. That is not a
detail: `working → blocked → working` is what an auto-answered permission looks
like, and a sampler that reads the row every 250 ms sees neither edge.

Each line carries a `seq` (monotonic, never reused), the `event`
(`present`/`created`/`changed`/`gone`), a `reason` saying which kind it was —
`spawned`/`registered`/`restored`,
`state`/`stopped`/`started`/`updated`/`lost`/`exited`/`relaunched`,
`soft_deleted`/`force_deleted`/`forgotten` — the `from_state` → `to_state` of the
transition, an `exited` event's `exit_code` and pane tail (`detail`), and the
same gating fields the table above lists, so acting on a `blocked` needs no
follow-up `session get`. `hook_state_contradicted` is `null`
(not checked) unless you pass `--verify`.

A driver that persists the last `seq` it handled restarts with
`--since <seq>` and gets exactly what it missed. `--session` narrows the stream
to one session, `--for-secs` bounds it, and it exits the moment its reader
closes the pipe.

`gone` used to be one word for both deletes. It is now two, and the difference
is the one that matters to a driver: `soft_deleted` can be restored,
`force_deleted` had its worktrees and window torn down. A third, `forgotten`,
touched nothing at all: a session mirrored through another host left this
instance's list (`[remote] transitive_sessions = false`) and goes on running at
its owner.

`lost` is a session whose pane went with nobody asking — a multiplexer crash
takes every pane on its server, and nothing inside that server is left to
write an event. `watch` looks at the local windows every few seconds
and reports a running session whose window is gone, once, as `changed`/`lost`
with the last state in `from_state`. A lost session names no pane
(`backend_id` is empty): the dead pane's id is free for the server to hand to
another session. A parked session is not lost, and a
`session restart` brings a lost one back. It covers **local sessions only**:
a peer's mirror pass copies a host's rows, not its events, so a `--host`
session's loss reaches nobody unless a `watch` runs on that host. Run `watch`
with the environment the sessions were created in — a watcher that resolves a
different tmux socket (another `TMUX_TMPDIR`, a private `/tmp`) finds no server
and reports every local session lost.

`exited` and `relaunched` come from a multiplexer whose windows close with their
agent (psmux), where a vanished window is otherwise all anyone sees. The agent's
window reports its own exit status and last screen on the way out. A relaunch by
the interface or by `restart --if-missing` is logged too. After three of those
within ten minutes the session is not relaunched again: it is marked `lost` and
waits for an operator's `session restart`. That `restart --if-missing` answers
`"held_back": true`. Sessions on tmux keep their dead pane and never produce
either event.

### `--parent`

Spawn workers with `--parent "$THURBOX_SESSION"` and the lead/worker
tree is recorded rather than remembered. `session list --parent <uuid>
--json` enumerates a run's workers afterwards — including the ones that
never reported, which are exactly the ones you need to find.

When a lead is replaced, move its workers with `session reparent <worker>
<new-lead>` (`--clear` makes one top-level). The new parent must be active, and
a link that would make a cycle is refused.

### A supervised loop, not automations

thurbox has `[[automations]]` (ADR-8b), and Fleet does not use them for its
periodic work. That is its reconciler, `uv run fleet reconcile`: one
supervised process holding a lock and a heartbeat, brought back by a user
service or by the lead's session start, and stopped only by the operator
(`stop` leaves a down flag `ensure` honours; `start` clears it). It runs
`watch`, `collect`, `shepherd`, `refuel` and the read-only `plan` on
their own intervals, because a worker that died on a token limit fires no
hook to say so.

Restraint is still part of the pattern. The reconciler never dispatches,
cancels or reorders, and every effect it has goes through `fleet queue`.
Rebuilding the map (`uv run fleet sync-registry`) is a command the operator
runs, and it writes only local, gitignored files.

---

## Constraints worth stating

Five facts shape any headless orchestration built on thurbox. Each
costs real time to rediscover.

### Bounded waits and read-side evidence

```sh
thurbox-cli session wait worker --state done,blocked --timeout 120 --json
thurbox-cli session wait worker --pane-matches 'Ready|Proceed\?' --timeout 30 --json
thurbox-cli session send worker 'run the checks' --wait-turn --timeout 30 --strict --json
```

A wait returns one event in the `watch` shape: a matching current state is a
`present` event; otherwise every event after its entry cursor is considered in
order. Pane text is the plain `session capture --lines 1000` text, checked at
most once per second for that session alone, and returned as `detail` with
`event: "pane_match"`. Either condition matching ends the wait. An event is
judged as of the moment it happened: a `done` the user has looked at since is
still `done` to a wait and to a state hook, whereas `watch` reports the row's
acknowledgement (`idle`). An acknowledgement writes no event, so a wait for
`idle` is not answered by the user looking at a `done` session. Exit 0 means
matched, 4 means timeout (a structured error, no matching event), 6 means the
session was deleted, stopped or lost, 1 is another failure and 3 an ambiguous
reference. Bad flags or an invalid regex exit 2.

`send --wait-turn` takes its cursor **before** sending and waits for the next
`working` edge, even if the agent is already working; text left in the
mailbox for a busy composer is waited for the same way. It preserves the send
result and adds the matching event as `turn`, and a timeout (4) or a session
that left (6) keeps the send result too, with `turn: null` and `error`, so a
driver never reads either as "the text was not delivered". Only agents whose hooks report
`working` can confirm this; a silent agent times out. `--strict` makes an
unconfirmable submission (`submitted: null`) fail. Without it the existing
success code for null is preserved. Neither option is used with `--no-enter`.
Remote sends delegate these flags to the host CLI; the host must support them.
A wait reads the mirrored event log where `watch` does; its remote latency is
therefore the host poll/mirror cadence. Remote text waits use the one session's
backend capture, never a stream of every pane.

`session get`, `session list`, `watch` and waits add `last_output_at` (epoch
milliseconds, null when the backend cannot report it) and `transcript_path`
(an existing local file, otherwise null). Both are local evidence: a row on a
host answers null for each rather than costing a round trip to the host on
every listing and `watch` batch. Output times come from a bounded metadata
listing per local backend, using the agent window's own session stamp
rather than a potentially reused pane-id hint. Backends without reliable
ownership stamps report null. This does not attach or stream output; servers
reporting whole seconds have that precision. A listing failure stays null.
Claude-family transcripts use the session's persisted `CLAUDE_CONFIG_DIR`,
falling back to the agent's default directory. Other families and remote rows
have null transcripts; a coincident local path is never a remote transcript.
Thurbox does not parse agent transcripts to infer state.

These fields are supervision evidence. A `background` state remains deferred
until a reproduction demonstrates both start and end hooks in the parent
session; a state with no observable end could remain stuck forever.

### The status field is not a completion signal

`session get`/`list --json` **do** carry the `working`/`blocked`/`done`/
`idle` state that agent hooks report through `session signal`, in
`hook_state`. What they cannot carry is any guarantee that it is
*current*: `hook_state` is latched — whatever was written last, by an
agent that may since have crashed, been interrupted, or never have been
wired to report at all. Polling it for completion is how a lead waits
forever on a worker that finished an hour ago, or declares one done
because its agent was never instrumented in the first place.

So headless completion detection is still a **report** the worker writes
— a result file or a mailbox message — or a printed sentinel the lead
greps out of `session capture`. What the state fields
are for is *supervision* — noticing that a worker is stuck, blocked, or
gone — and each one comes with what it takes to judge it:

| field | what it answers |
|---|---|
| `hook_state` | the raw last report, unchanged and unfiltered |
| `hook_state_at`, `hook_state_age_secs` | when it was made, and how long ago |
| `hook_reported` | whether anything has *ever* reported (silence ≠ idle) |
| `hook_coverage`, `hook_states_reportable` | what this agent can report at all |
| `hook_coverage_source` | which name that answer came from: `name`, `hook_schema`, or `detection` (the pane, for a row that declares no agent — coverage then reads `presumed`, never `full`) |
| `hook_blocked_is_heuristic` | whether its `blocked` is a text match on a notification body |
| `hook_corroboration`, `hook_state_contradicted` | what actually holds the pane, and whether it agrees — `missing` (the listing holds no window of the session's) contradicts a `working`/`blocked` row; `unknown` (the listing did not answer) does not |
| `detected_agent` | which registered agent is in the pane, when it is not the row's own |
| `state`, `state_source` | the best answer available, and where it came from |
| `stopped` | whether the session is parked — `session stop`, no pane |
| `ready` | whether the session can take input now, for every agent — see *Readiness* below |
| `reports_as` | the agent the hook fields were read against, when a driver declared one |

`state` is always a word, and one of `SessionState`'s — the single
vocabulary `session get`, `session list`, `thurbox-cli watch` and the
interface all derive through, so no two of them answer differently for
one row. Besides the five an agent can signal it can read `unreported`
(nothing has reported for this session), `uncovered` (its agent is wired
to report nothing), `stopped` (the session is parked) or `running` (an
agent holds the pane and has not signalled — `session get`'s probe only).
None of those four is a state an agent can signal, and `state_source` is
null for the first three. The piped (TOON) `session list` shows this
column, and so does the interface — its session list derives through the
same `Assessment`, so a driver reconciling the screen with a `session
get` never has to reconcile two vocabularies.

`agent`, `reports_as` and `detected_agent` are three different facts and
no two of them substitute for each other: what the row was created as,
what a driver *declared* it runs, and what is observably in the pane
right now. Detection is deliberately never written back as `reports_as`
— a declaration is durable, an observation is not, and deriving one from
the other would make a passing process permanent.

A finished turn reads `done` until somebody looks at it and `idle`
after: the interface stamps `seen_at` when focus moves off, and every
read verb folds that in. It is a stored fact, not a guessed timeout —
the two folds that *are* timing (a `working` session gone quiet, an
unreachable host) need a live terminal, so headless answers never
invent them.

A **parked** session (`session stop`: pane killed, row and checkout
kept) stays in `session list` and is told apart there — `stopped: true`
and `state: "stopped"` on both read verbs, so a liveness check is the
poll you were already doing rather than a pane probe. Its `backend_id`
still names the window it had: that is what the row records, and
`session start` replaces it with the new pane's id. `send`, `key` and
`capture` refuse a parked session by name instead of reaching for the
window that is gone.

Parking also **clears the latched state and refuses a new one**. A parked
session has no process, so a heartbeat's pane-option poll or a mirror pass
carrying a host's last word would be reporting a turn on a session that cannot
be in one — and every watcher of that row would see a transition that did not
happen. `session signal` against a parked session fails rather than silently
doing nothing, and says to `session start` it first.

There is deliberately **no staleness timeout**. A turn may legitimately
run for an hour, so any bound thurbox picked would report live work as
finished; the age is published instead and the policy is yours.

`session get` checks the pane by default (one multiplexer query plus one
`ps`); `session list` does not unless you pass `--verify`, since that
cost is per session. A remote session answers `unavailable` — its pane
lives on its own host's multiplexer. `thurbox-cli session doctor` is the
same information as a verdict, plus whether the wiring is installed at
all; it exits non-zero when a session's wiring is broken. An agent
thurbox ships no hooks for but which is signalling anyway — a driver
calling `session signal` itself — is a warning, not a failure, and a
`--command` session is "no hooks expected": thurbox never had an agent
there to wire, so there is nothing to find broken.

If your driver launches an agent *inside* such a session, say so with
`session create --reports-as <agent>` or `session reports-as <ref>
<agent>`. Every field in the table is then read against that agent
instead of the command's file stem — including
`hook_blocked_is_heuristic`, which otherwise reports `false` for a pane
running claude and tells a supervisor the block signal is structured
when it is a text match on a notification body.

#### Readiness

`ready` is the one agent-agnostic answer to "can I send this session input
now", on `session get` and `session list` alike. A caller needs no agent
knowledge of its own to read it:

| `ready` | when |
|---|---|
| `true` | the last hook report is `idle` or `done`; or **no hook has reported yet** (`state: "running"`, `state_source: "process"`), an agent holds the pane, and its input line shows an empty, drawn composer — one prompt glyph with the cursor after it |
| `false` | `working`, `blocked` or `background`; parked (`stopped`); the pane holds no agent (`shell`, `dead`, `missing`) or contradicts the report; or a fresh agent whose composer is not drawn yet |
| `null` | nothing can say: the pane was not looked at (`session list` without `--verify`, before any report), it is remote and unreported, or its input line could not be read |

The composer read covers the window between launch and the first hook, which
some agents (Codex) only fire at their first turn: it is made only for a session
no hook has reported on, and only by a verb that probes the pane. Once a hook
has reported, the agent's own word decides and the screen is not read. Poll
`session get` until `ready` is `true`; `null` is "ask again", never "go".

`session capture --json` adds the pane's live state alongside its text —
`cursor_row`/`cursor_col`, `foreground_process` and `foreground_command`,
`foreground_cwd` — which is what a lead reading a worker's screen needs
to tell "waiting at a prompt" from "still printing", and which agent CLI
is actually in the foreground.

### A driver that launches its own agent can still report state

thurbox wires status hooks at launch, for an agent it knows from
`agents.toml`. A harness that must own the agent launch itself — asking
thurbox for a bare interactive shell and starting the agent inside that
pane — therefore gets no hooks, and its sessions would read as never
having reported anything.

Three things close that, and all are **stable contract**:

- **`thurbox-cli agent launch-args <name>` reports what to launch.**
  The hooks are *arguments* — the `hooks` extension installs them by
  appending to the agent's `args` in `agents.toml` (`--settings
  <hooks>.json` for claude) — so an agent started any other way simply
  has none. This prints the `command`, `args` and `env` thurbox itself
  would use; pass the args through and the hooks are there. With
  `--session <ref>` it resolves for that session: the conversation id is
  pinned to the row's, the host adapts the args, and the environment
  carries the `THURBOX_SESSION` its `session signal` will report under.
- **`THURBOX_SESSION` is in the pane's environment**, and every child
  process inherits it. So anything running in the pane — the driver, the
  agent, one of the agent's own hooks — can call
  `thurbox-cli session signal --state <working|blocked|done|idle|background>` with
  **no arguments**: identity resolves from the environment. From outside
  the pane, pass `--session <uuid>`. This is the supported way to report
  state for an agent thurbox did not launch. `session exec` carries the
  *target* session's identity, not the calling driver's, so a signal run
  through it lands on the session it names.
- **Failing that, the pane is read anyway.** A session that never
  signalled but whose pane's foreground process is an agent the registry
  knows reports `state: "running"` with `state_source: "process"` and
  `hook_corroboration: "foreign-agent"`. It is coarser than a hook by
  design — process inspection can say an agent is there, never what it
  is doing — but it is the difference between a session that reads as
  empty and one that reads as alive.

### Read the exit status before parsing the output

Errors are **structured documents on stdout**, not lines on stderr —
AXI principle 6, because an agent reads one stream and a message on
stderr is one it has to be told to capture. The exit code carries the
verdict: `0` success, `1` the command ran and failed, `2` the invocation
was wrong, `3` a session reference (or the window it names) matched more than one
candidate, `4` a wait timed out, `5` a key, name, run or pane is already held, and `6` the subject disappeared
during the call.

Error documents include `error_kind` beside the human-readable `error`.
Kinds are `not_found`, `ambiguous`, `usage`, `unreachable`, `unreached`,
`undetermined`, `refused`, `conflict`, `timeout`, `gone`, and `other`.
`not_found` means the addressed row or required pane does not exist at lookup;
`gone` means a subject left during an operation. `unreached` and
`undetermined` remain distinct: the latter may already have performed a write,
so reconcile before retrying. Unknown kinds are `other`. A peer on an older
build sends no kind: its answer is `other` too, except thurbox's own two
absence sentences (`Session not found: …`, `Deleted session not found: …`),
which stay `not_found` so a host one release behind still lets a teardown fall
through. No other wording authorizes a destructive fallback. `session reap` or
`session restore` of a UUID that is live here, not deleted, answers `conflict`.
The exit status is still authoritative: commands that render a failure report
may exit nonzero without an `error` field.

The consequence is a trap worth naming. `thurbox-cli … --json | jq -r
'.field'` exits **0 with empty output** when the command failed: `jq`
parsed the error object perfectly well, found no such field, and the
pipeline reports `jq`'s status rather than thurbox's. Capture first and
branch on the status, or read `.error`:

```bash
out=$(thurbox-cli session get "$ref" --json) || {
  printf 'thurbox: %s\n' "$(jq -r .error <<<"$out")" >&2
  exit 1
}
id=$(jq -r .id <<<"$out")
```

`set -o pipefail` does not help here — the failing command exits 1 but
`jq` succeeds, so the pipeline's status is `jq`'s either way.

### The environment `session exec` runs under

`session exec <ref> -- <cmd>` runs in the session's directory, on the
machine it lives on, and under the session's own environment: whatever
`session create --env` recorded for it, plus the `THURBOX_*` identity
its pane carries. The calling process's own `THURBOX_*` variables are
**scrubbed** — a driver running inside one session must not lend the
child that session's identity, which is what made
`session exec <worker> -- thurbox-cli session signal --state done`
record for the *driver* and exit 0. The environment actually used is in
the result's `env`.

### Fast-forward the base branch before creating a worktree

A worktree inherits whatever the *local* base branch points at, not what
the remote does. A stale local `main` yields a worker that does
perfectly correct work against a month-old tree and opens a conflicting
PR — a failure that looks like a bad agent and is really a bad base.

Fetch and fast-forward before `session create`, and verify
`git rev-list --count main..origin/main` is `0`.

---

## The reference implementation

**[Fleet](https://github.com/Thurbeen/fleet)** is all of the above,
maintained. It is also the worked example of a thurbox extension — the one
the rest of the documentation points at, since `extensions/` in the thurbox
repo holds only the two built-ins.

```sh
curl -LsSf https://raw.githubusercontent.com/Thurbeen/fleet/main/install.sh | sh
```

On Windows, `irm https://raw.githubusercontent.com/Thurbeen/fleet/main/install.ps1 | iex`.
It needs thurbox with tmux (psmux on Windows), git and an agent CLI;
`gh` or `glab` is optional, and a local-only fleet works without either.
Then start Mission Control in thurbox and run `/fleet-onboarding`. The
[README](https://github.com/Thurbeen/fleet#readme) and
[`AGENTS.md`](https://github.com/Thurbeen/fleet/blob/main/AGENTS.md) are
the operating manual.

Everything a running fleet writes — the queue, run logs, the generated map
and its facts, the operator's settings — is gitignored, so the public repo
holds only the machinery and does not back that content up. Merging is
explicit: `shepherd` squash-merges only the repositories the operator's own
`auto-merge.conf` names, and the tracked example names none.

Fleet is an extension, not a thurbox feature: nothing in thurbox knows it
exists. The part worth copying is the arrangement, not the files. Two of
its choices are that arrangement rather than its own taste.

**The lead's job description is a payload file, not a prompt.** The
manifest's `[[files]]` lays down `FLEET.rendered.md`, rendered from the
checkout's `FLEET.md`, in the extension home, and three `[[symlinks]]`
surface it as `CLAUDE.md`, `AGENTS.md` and `GEMINI.md`, so the lead reads
what it is *for* whichever CLI is behind it. That text is deliberately not
the repo's own `AGENTS.md`: one says what the session is for — hold the
plan and the log, never the branches — and the other says how to work
inside the checkout. A lead whose invariant lives only in the conversation
that stated it keeps that invariant exactly as long as the conversation.

**The manifest ships as `extension.toml.in` because no token spells "my
clone".** `[[sessions]] repo_path` has to name the checkout — that is
where `registry/` and `orchestration/` are — and `{home}` is
substituted, but it resolves to the extension home
(`<config>/extensions/<name>/`). A committed file cannot hardcode a path
that exists on one machine, so it ships a `__REPO_PATH__` placeholder that
`uv run fleet install-extension` renders with the clone's absolute path
before installing. A leading `~` would not help: it names a home
directory, not a clone, so the placeholder is the permanent answer.

`version --json` advertises these additive event-consumer features as
`session_wait`, `session_state_hooks` and `session_pre_launch`, so a driver
checks a boolean rather than scraping `--help`.
