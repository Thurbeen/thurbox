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
curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/extensions/flow/install.sh | sh
```

or from a checkout: `extensions/flow/install.sh`. The installer is
idempotent **and self-updating** — re-run it any time to pull the latest
`FLOW.md` spec, helper scripts, context-file symlinks, and claude
permission settings (the files it owns), while leaving your own data
(`repos.md`, `agents.toml` entries, the flow session, the `flow-tick`
automation) untouched. It

1. sets up the flow home (`~/flow`): `FLOW.md` spec, helper scripts,
   context-file symlinks, and a `repos.md` routing table (edit it!);
2. adds the `flow` / `flow-worker` / `flow-worker-heavy` entries to
   `~/.config/thurbox/agents.toml` (defaults: claude on haiku for the
   triager, opus / fable for workers — override with `FLOW_CMD`,
   `WORKER_CMD`, `WORKER_HEAVY_CMD` + matching `*_ARGS` env vars);
3. creates the dedicated `flow` session and a `flow-tick` automation
   (every 5 minutes by default, `TICK_CRON` to change) that keeps it
   monitoring workers even while the TUI is closed.

## Use

- Open the `flow` session in the thurbox TUI and type at it — anything
  that isn't `tick`/`status`/`clean` is treated as a brain-dump.
- Dispatchable items spawn a worker immediately (`task-<id>-<slug>`
  session, on a `flow/<slug>` worktree branch whenever the repo is git).
- `status` for a one-screen report; `clean` to groom the backlog.
- Workers self-report: they mark their task done, print a
  `===RESULT===` JSON line, and ping the flow session so the next task
  dispatches without waiting for the cron tick.

## Files

| Path | Purpose |
|------|---------|
| `FLOW.md` | The agent behavior spec (modes, dispatch rules, output contract) |
| `scripts/create-task.sh` | Atomic task create + dispatch |
| `scripts/flow-snapshot.sh` | One-call backlog + sessions view |
| `scripts/parse-result.sh` | Extract the worker `===RESULT===` sentinel |
| `install.sh` | Idempotent, self-updating installer/bootstrapper |

## Uninstall

```bash
thurbox-cli automation list | jq -r '.[] | select(.name=="flow-tick") | .id' \
  | xargs -r -n1 thurbox-cli automation remove
thurbox-cli session list | jq -r '.[] | select(.name=="flow") | .id' \
  | xargs -r -n1 thurbox-cli session delete --force
rm -rf ~/flow   # and remove the flow* entries from agents.toml
```
