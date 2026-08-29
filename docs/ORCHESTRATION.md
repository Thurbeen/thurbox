# Orchestration: the control-plane pattern

Design rationale for running agent sessions across more than one repo.
For the primitives themselves, see [FEATURES.md](FEATURES.md); for how
they are built, see [ARCHITECTURE.md](ARCHITECTURE.md).

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
  generated from the GitHub API so it cannot drift, plus hand-written
  context files (`registry/context/<repo>.md`) holding the judgement a
  generated index can't: what a project is *for*, how it relates to the
  others, what its current goals are.
- **The orchestration.** Reusable recipes
  (`orchestration/playbooks/<name>.md`) and one log per run
  (`orchestration/runs/<date>-<slug>.md`).

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

1. Clarify the goal. Pick a playbook, or write one from the template.
2. Open a run log, named `<YYYY-MM-DD>-<slug>.md`.
3. For each unit of work, launch a worker session with one
   self-contained prompt, in its own worktree on the target repo.
4. Record every session — name, repo, prompt intent, outcome, PR — in
   the run log **as it happens**, not at the end. A run log written
   afterwards is a summary; one written during is the source of truth
   for what happened, and it survives the lead session dying.
5. Drain results from the mailbox as workers report.
6. Review the PRs. Delete each session as it closes out.

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

### The mailbox, not polling

Workers report by mailing the lead:

```sh
thurbox-cli message send --to <lead> --kind result --body '<PR url>'
```

The lead drains its inbox exactly once:

```sh
thurbox-cli message inbox --for <lead> --claim --json
```

`send` **wakes** the recipient by default, so the lead never polls.
thurbox injects `THURBOX_SESSION` into each session's environment, and
both `--from` and `inbox --for` default to it, so a worker needs no ids
to mail home and the lead needs none to read its own mail.

**Why not poll `gh pr list`?** Three reasons, and the third is the one
that matters:

- It is **exact**. A message is addressed to the lead by a worker that
  knows it finished. A PR query infers completion from a side effect.
- It is **immediate**. `send` wakes the lead; a poll runs on its own
  cadence and adds latency proportional to the interval.
- It can say **"not applicable"**. A worker that correctly concludes
  there was nothing to do reports that in one message. A PR poll cannot
  distinguish "no PR because there was nothing to fix" from "no PR
  because the worker is still thinking" — and those demand opposite
  responses from the lead.

`--kind` is a free-form tag, so a run can distinguish `result` from
`questions` or `plan` without the lead parsing prose.

### `--parent`

Spawn workers with `--parent "$THURBOX_SESSION"` and the lead/worker
tree is recorded rather than remembered. `session list --parent <uuid>
--json` enumerates a run's workers afterwards — including the ones that
never reported, which are exactly the ones you need to find.

### Deliberately no automations

thurbox has `[[automations]]` (ADR-8b), and this pattern does not use
them.

The one scheduled candidate is the registry sync. It regenerates the
index from the GitHub API, then commits and pushes. That is a write to
`main` on a cadence, with no reader — so a human runs it and reads the
diff. Nothing else in the pattern is periodic: a run starts because
someone has a goal.

Restraint here is part of the pattern, not an omission from it. A
control plane that fires unattended writes is a second actor in the
system, and the whole point of the run log is that there is one.

---

## Constraints worth stating

Three facts shape any headless orchestration built on thurbox. Each
costs real time to rediscover.

### The status field is not a completion signal

`session get`/`list --json` **do** carry the `working`/`blocked`/`done`/
`idle` state that agent hooks report through `session signal`, in
`hook_state`. What they cannot carry is any guarantee that it is
*current*: `hook_state` is latched — whatever was written last, by an
agent that may since have crashed, been interrupted, or never have been
wired to report at all. Polling it for completion is how a lead waits
forever on a worker that finished an hour ago, or declares one done
because its agent was never instrumented in the first place.

So headless completion detection is still the **mailbox**, or a printed
sentinel the lead greps out of `session capture`. What the state fields
are for is *supervision* — noticing that a worker is stuck, blocked, or
gone — and each one comes with what it takes to judge it:

| field | what it answers |
|---|---|
| `hook_state` | the raw last report, unchanged and unfiltered |
| `hook_state_at`, `hook_state_age_secs` | when it was made, and how long ago |
| `hook_reported` | whether anything has *ever* reported (silence ≠ idle) |
| `hook_coverage`, `hook_states_reportable` | what this agent can report at all |
| `hook_blocked_is_heuristic` | whether its `blocked` is a text match on a notification body |
| `hook_corroboration`, `hook_state_contradicted` | what actually holds the pane, and whether it agrees |
| `state`, `state_source` | the best answer available, and where it came from |

There is deliberately **no staleness timeout**. A turn may legitimately
run for an hour, so any bound thurbox picked would report live work as
finished; the age is published instead and the policy is yours.

`session get` checks the pane by default (one multiplexer query plus one
`ps`); `session list` does not unless you pass `--verify`, since that
cost is per session. A remote session answers `unavailable` — its pane
lives on its own host's multiplexer. `thurbox-cli session doctor` is the
same information as a verdict, plus whether the wiring is installed at
all; it exits non-zero when no state can reach thurbox from a session.

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

Two things close that, and both are **stable contract**:

- **`THURBOX_SESSION` is in the pane's environment**, and every child
  process inherits it. So anything running in the pane — the driver, the
  agent, one of the agent's own hooks — can call
  `thurbox-cli session signal --state <working|blocked|done|idle>` with
  **no arguments**: identity resolves from the environment. From outside
  the pane, pass `--session <uuid>`. This is the supported way to report
  state for an agent thurbox did not launch.
- **Failing that, the pane is read anyway.** A session that never
  signalled but whose pane's foreground process is an agent the registry
  knows reports `state: "running"` with `state_source: "process"` and
  `hook_corroboration: "foreign-agent"`. It is coarser than a hook by
  design — process inspection can say an agent is there, never what it
  is doing — but it is the difference between a session that reads as
  empty and one that reads as alive.

### Fast-forward the base branch before creating a worktree

A worktree inherits whatever the *local* base branch points at, not what
the remote does. A stale local `main` yields a worker that does
perfectly correct work against a month-old tree and opens a conflicting
PR — a failure that looks like a bad agent and is really a bad base.

Fetch and fast-forward before `session create`, and verify
`git rev-list --count main..origin/main` is `0`.

---

## The template

A working, public implementation of everything above:
<https://github.com/Thurbeen/fleet-template>

```text
registry/
  owners.txt                 GitHub owners to index, one per line
  repos.generated.yaml       generated; never hand-edited
  context/_TEMPLATE.md       copy this to add a project
orchestration/
  playbooks/_TEMPLATE.md     copy this to add a recipe
  runs/_TEMPLATE.md          copy this per run
scripts/
  sync-registry.sh           regenerate the index via `gh`
  install-extension.sh       render extension.toml, then install
extension.toml.in            manifest template (rendered at install time)
FLEET.md                     standing context for the long-lived session
```

Click **"Use this template"**, edit `registry/owners.txt`, then:

```sh
./scripts/sync-registry.sh
./scripts/install-extension.sh
```

That leaves you with a working control-plane session.

**Why `extension.toml.in` and not `extension.toml`?** A `[[sessions]]`
entry needs an absolute `repo_path`, and `resolved_for_home` substitutes
only the `{home}` token — it does not expand `~`, so a tilde is taken
literally and the session lands in a directory named `~`. A template
cannot hardcode a path that exists on one machine, so it ships a
`__REPO_PATH__` placeholder that `install-extension.sh` renders from
`git rev-parse --show-toplevel` before calling `extension install`.
