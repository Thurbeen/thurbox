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
🎯 Next: review task-7-add-rate-limiting (worker finished, PR open)
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
   triager, opus / fable for workers — edit agents.toml to change the
   CLI/model);
3. writes the manifest to `~/.config/thurbox/extensions/flow.toml` and
   activates it, creating the dedicated `flow` session and a `flow-tick`
   automation (every 5 minutes) that keeps it monitoring workers even
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

## Use

- Open the `flow` session in the thurbox TUI and type at it — anything
  that isn't `tick`/`status`/`clean` is treated as a brain-dump.
- Dispatchable items spawn a worker immediately (`task-<id>-<slug>`
  session, on a `flow/<slug>` worktree branch whenever the repo is git).
- **Plan-first dispatch**: every worker prompt carries a mandatory
  planning phase. Before writing any code the worker posts a short PLAN —
  problem statement, concrete acceptance criteria, and an implementation
  approach/architecture — then builds strictly against it, so dispatched
  work stays scoped to what you asked for. The flow agent seeds the
  acceptance criterion (`--accept`) at capture; the worker fills in the
  rest. (Pass `--no-plan` to `create-task.sh` for trivial mechanical
  changes where a plan is overkill.)
- `status` for a one-screen report; `clean` to groom the backlog.
- Workers self-report: they mark their task done, print a
  `===RESULT===` JSON line, and ping the flow session so the next task
  dispatches without waiting for the cron tick.

## Files

| Path | Purpose |
|------|---------|
| `extension.toml` | Manifest: agents, payload files, symlinks, session + automation (the installer) |
| `FLOW.md` | The agent behavior spec (modes, dispatch rules, output contract) |
| `claude-settings.json` | Permission template (`{home}`-substituted into `.claude/settings.json`) |
| `repos.md` | Routing-table seed (installed once, then user-owned) |
| `scripts/create-task.sh` | Atomic task create + dispatch; composes the plan-first worker prompt (`--dry-run` to preview) |
| `scripts/flow-snapshot.sh` | One-call backlog + sessions view |
| `scripts/parse-result.sh` | Extract the worker `===RESULT===` sentinel |
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
