# Flow — a focus-protecting triage agent for thurbox

> **Status: experimental.** Flow is a brand-new feature under active
> testing — expect the behavior spec, scripts, and installer to change
> between releases.

Flow keeps you in flow state. You brain-dump tasks at a cheap, fast triage
agent; it captures everything into the thurbox task list, dispatches real
work to worker sessions (each in its own git worktree), monitors them
quietly, cleans the backlog, and always ends with the single next thing to
focus on:

```text
---
Needs you: PR #42 has a failing migration — approve the schema change?
🎯 Next: review "Add rate limiting · #7" (worker finished, PR open)
```

Flow is **agent-agnostic**, like thurbox itself: the triager and the
workers are plain `agents.toml` entries (`flow`, `flow-worker`,
`flow-worker-heavy`), so each can be claude, codex, gemini, opencode,
vibe, … The behavior lives in [FLOW.md](FLOW.md), a plain context file
surfaced to whatever CLI you pick via symlinks (`CLAUDE.md`, `AGENTS.md`,
`GEMINI.md` → `FLOW.md`).

## Install

```bash
thurbox-cli extension install flow
```

That single command is the installer — it reads flow's
[`extension.toml`](extension.toml) manifest and:

1. sets up the flow home (`~/flow`, override with `--home`): `FLOW.md`
   spec, helper scripts, context-file symlinks, claude permission
   settings, and a `repos.md` routing table (edit it!);
2. registers the `flow` / `flow-worker` / `flow-worker-heavy` entries in
   `~/.config/thurbox/agents.toml` (defaults: claude on haiku for the
   triager, opus for workers — edit agents.toml to change the
   CLI/model);
3. writes the manifest to `~/.config/thurbox/extensions/flow.toml` and
   activates it, creating the dedicated `flow` session and a `flow-tick`
   automation (every 10 minutes) that keeps it monitoring workers even
   while the TUI is closed.

It's idempotent — re-run it any time to pull the latest spec/scripts,
while leaving your own data (`repos.md`, edited agents) untouched.

`install flow` fetches from the official source; you can also install
from a local checkout or any URL:

```bash
thurbox-cli extension install ./extensions/flow        # local directory
thurbox-cli extension install https://example.com/ext/flow   # custom source
```

A `curl … install.sh | sh` one-liner still works (it's now a thin shim
that calls `thurbox-cli extension install`), needed only to bootstrap on
a box where you'd rather pipe a script:

```bash
curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/extensions/flow/install.sh | sh
```

### Self-healing

The flow session and `flow-tick` automation are **managed**: thurbox
re-creates them automatically if they're ever deleted (on TUI startup and
on every automation tick), so flow can't be half-removed by accident.
Deleting the flow session/automation by hand is therefore a no-op — they
come back. To turn flow off for good, run:

```bash
thurbox-cli extension deactivate flow         # tear down + stop self-heal
thurbox-cli extension deactivate flow --purge # also remove the manifest
```

Re-enable any time with `thurbox-cli extension activate flow` (no full
reinstall needed). `thurbox-cli extension list` shows whether flow is
active and healthy.

### Updating

Flow is **pinned to your thurbox version**: a bare-name install fetches
the copy that matches your binary's release tag. After you upgrade
thurbox, `extension list`/`status` mark flow `stale` (and self-heal prints
a one-line nudge at startup) because the on-disk copy predates the new
binary. Refresh it with:

```bash
thurbox-cli extension update flow      # re-fetch the version matching your thurbox
thurbox-cli extension update --all     # update every installed extension
```

`update` re-lays flow's payload from its recorded source but keeps files
you've edited — `repos.md` and a customised `.claude/settings.json` are
preserved unless you pass `--force`. To pin an older flow, install from a
tagged URL (`…/thurbox/v0.112.0/extensions/flow`) instead.

## Use

- Open the `flow` session in the thurbox TUI and type at it — anything
  that isn't `tick`/`status`/`clean` is treated as a brain-dump.
- Dispatchable items spawn a worker immediately (a session named after the
  task title, `<title> · #<id>`, on a `flow/<slug>` worktree branch whenever
  the repo is git).
- **Plan-first dispatch**: every worker prompt carries a mandatory
  planning phase — clarify, then plan, then build. Before writing any code
  the worker (1) asks clarifying questions **one at a time** — a single question,
  then it waits for your answer before sending the next, adaptively (often 3+,
  but fewer if an early answer makes later ones moot) — (2)
  writes a structured plan — problem, concrete acceptance criteria, approach —
  and waits for your **approval**, then (3) builds strictly against the
  approved plan, so dispatched work stays scoped to what you asked for. The flow
  agent seeds the acceptance criterion (`--accept`) at capture; the worker fills
  in the rest. (Pass `--no-plan` to `create-task.sh` for trivial mechanical
  changes where a plan is overkill.)
- **Event-driven relay via a message queue**: workers hand the `flow` session
  clean, structured payloads through the durable `thurbox-cli message` queue —
  `--kind questions`, `--kind plan`, `--kind result` — instead of flow scraping
  their terminals. Workers pass **no ids**: thurbox injects each session's
  identity (`THURBOX_SESSION`/`THURBOX_TASK`) at spawn and auto-stamps the
  sender + task tag. Each push also wakes flow, so it surfaces the question or
  plan under "Needs you" immediately; you type your answer / approval naturally
  and flow relays it back with `message reply <message_id>` — thurbox routes it
  to that message's sender, so flow never handles a worker's session id. Flow is
  a pure pass-through: it never answers, invents, or approves — it just wires the
  worker to you and back. Several workers can be mid-conversation at once, each
  tagged by its `#<id>`.
- `status` for a one-screen report; `clean` to groom the backlog.
- A tick prints the **board** — a quick-glance table of all live
  `flow` / worker (`… · #<id>`) sessions with status, age, and the task they're
  working (`scripts/flow-summary.sh`) — **only when something needs attention**
  (a surfaced question/plan/result, an error/blocker, a stale reset, an orphan
  session, or a fresh dispatch). A quiet tick prints just one minimal line
  (`tick: N running, M todo`), so routine ticks don't interrupt you. The
  `flow-tick` automation is a **safety net** that fires every 10 minutes: worker
  pushes drive the interactive loop; the cron tick just drains anything a missed
  wake left queued and grooms stale state.
- Workers self-report: they mark their task done and send a `--kind result`
  message, which wakes flow so the next task dispatches without waiting for the
  cron tick.

## Files

| Path | Purpose |
|------|---------|
| `extension.toml` | Manifest: agents, payload files, symlinks, session + automation (the installer) |
| `FLOW.md` | The agent behavior spec (modes, dispatch rules, output contract) |
| `claude-settings.json` | Permission template (`{home}`-substituted into `.claude/settings.json`) |
| `repos.md` | Routing-table seed (installed once, then user-owned) |
| `scripts/create-task.sh` | Atomic task create + dispatch; composes the plan-first worker prompt (`--dry-run` to preview) |
| `scripts/flow-snapshot.sh` | One-call backlog + sessions view |
| `scripts/flow-summary.sh` | At-a-glance board table (printed atop every tick) |
| `scripts/parse-result.sh` | Fallback-only: extract a `===RESULT===` sentinel from a worker that died without sending a `result` message |
| `install.sh` | Thin shim → `thurbox-cli extension install` (curl\|sh bootstrap) |

## Uninstall

`uninstall` reverses `install` — it tears down the session + automation,
removes the `flow*` agents from `agents.toml`, and deletes the manifest:

```bash
thurbox-cli extension uninstall flow            # keeps ~/flow (your repos.md etc.)
thurbox-cli extension uninstall flow --purge    # also deletes ~/flow
```

To only switch flow off (keeping it installed for a later `activate`):

```bash
thurbox-cli extension deactivate flow           # stop self-heal, keep files
```

> Note: a plain `session delete` / `automation remove` is **not** enough on
> its own — while flow is active, thurbox self-heals those resources.
> `deactivate` (or `uninstall`) is what stops the self-heal.
