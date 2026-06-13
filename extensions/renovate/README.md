# Renovate — keep your local repos on up-to-date dependencies

> **Status: experimental.** Renovate (the thurbox extension) is new and under
> active testing — expect the behavior spec, scripts, and installer to change
> between releases.

This extension keeps the dependencies in your local repos current with minimal
fuss. A quiet `renovate` session sweeps a watch list on a schedule and, for each
repo that isn't already mid-update, dispatches a worker that runs
[Renovate](https://github.com/renovatebot/renovate) on its **local platform**,
runs your project's tests, commits the bumps to a fresh branch, and opens a
review PR — then pings the monitor so the next repo dispatches. Dependency
upkeep becomes background work you only look at when a PR is ready or a major
upgrade needs a decision.

```text
---
Needs you: example — major bump of `serde` 1→2 needs your call (tests pass)
🎯 Next: review the open PR for myrepo (12 patch/minor bumps, CI green)
```

## Renovate runs only locally

The worker always invokes Renovate with `--platform=local`: **no hosted bot, no
token, no Renovate-opened PRs.** Renovate simply rewrites the dependency
manifests + lockfiles in an isolated worktree; the thurbox worker owns the rest
(test, commit, push, open a PR). That keeps it forge-agnostic *and*
agent-agnostic, like thurbox itself — only **git** is baked in. The monitor and
worker are plain `agents.toml` aliases (`renovate`, `renovate-worker`), so each
can be claude, codex, gemini, opencode, vibe, … The behavior lives in
[RENOVATE.md](RENOVATE.md), surfaced to whatever CLI you pick via context-file
symlinks (`CLAUDE.md`/`AGENTS.md`/`GEMINI.md` → `RENOVATE.md`).

## Update strategy

Two layers control how far versions move:

- **Per repo** — the `strategy` column in `repos.md`: `patch`, `minor`
  (patch + minor, the default), `major`, or `all`.
- **Global** — `renovate-config.json`, Renovate's own config (grouping, version
  ranges, ignored deps, lockfile maintenance). The worker passes it via
  `RENOVATE_CONFIG_FILE`; the per-repo strategy is layered on top.

## Requirements

- `git`, `jq`, and `thurbox-cli` on `PATH`.
- **Node ≥ 20** — Renovate runs via `npx --yes renovate` (a global `renovate`
  install is used if present). Set `GITHUB_COM_TOKEN` to raise GitHub API limits
  / fetch changelogs (optional, read-only).
- For the worker to open a review PR: the forge client authenticated
  (`gh auth login` / `glab auth login`). Without one the worker leaves the
  branch committed locally and says so.

## Install

Renovate is a thurbox **extension** (a declarative `extension.toml` manifest):

```bash
thurbox-cli extension install renovate
```

or from a checkout: `thurbox-cli extension install ./extensions/renovate` (the
`install.sh` / curl one-liner is a thin shim over this). Override the home with
`--home <dir>`. Installing:

1. lays down the renovate home (`~/renovate`): the `RENOVATE.md` spec, helper
   scripts, context-file symlinks, claude permission settings, a `repos.md`
   watch list, and a `renovate-config.json` (both seeded once, then user-owned);
2. registers the `renovate` / `renovate-worker` agents in
   `~/.config/thurbox/agents.toml` (defaults: claude on haiku for the monitor,
   opus for the worker — edit the entries to change the model/CLI);
3. activates the dedicated `renovate` session and a `renovate-tick` automation
   (weekly, Monday 09:00) — both **self-healed** by thurbox at startup and on
   every tick, so deleting them is a no-op until you `deactivate`.

Re-running `extension install` refreshes the installer-owned files. `thurbox-cli
extension list` / `status renovate` show what's active.

## Use

- Add the repos to keep current to `~/renovate/repos.md` (one row each;
  `strategy` defaults to `minor`, `provider` to `auto`).
- The monitor sweeps on its schedule; trigger one now by sending `tick` to the
  renovate session (or open it in the TUI).
- `status` for a one-screen report; `clean` to prune merged update worktrees.
- An updater is a thurbox **task** named `update <repo> deps …`; its worker runs
  in an isolated worktree on a fresh `renovate/updates-<ts>` branch and
  self-reports via a `===RESULT===` sentinel, pinging the monitor when it
  finishes.

## How the worker gets its branch

A renovate run starts a **brand-new** branch, so — unlike ci-shepherd — there's
no branch to adopt: `dispatch-update.sh` uses thurbox's native `--worktree`,
which runs `git worktree add -b renovate/updates-<ts> origin/main`. The worker
runs Renovate locally there, tests, commits, and pushes that branch for review.
`clean` removes the worktree once the PR has merged (refusing if it has
uncommitted work).

## Files

| Path | Purpose |
|------|---------|
| `RENOVATE.md` | The agent behavior spec (modes, eligibility rules, output contract) |
| `renovate-config.json` | Renovate's own config (global update strategy), passed via `RENOVATE_CONFIG_FILE` |
| `repos.md` | Watch list: repos + per-repo strategy/provider |
| `scripts/renovate-snapshot.sh` | One-call local view: watched repos + in-flight branches/tasks/sessions |
| `scripts/dispatch-update.sh` | Create a fresh update worktree + create and run the updater task |
| `scripts/renovate-run.sh` | Run Renovate (`--platform=local` only); maps strategy → config overlay |
| `scripts/parse-result.sh` | Extract the worker `===RESULT===` sentinel |
| `extension.toml` | Declarative install + runtime manifest (`thurbox-cli extension install` reads this) |
| `claude-settings.json` | Pre-approved permissions template (laid down as `.claude/settings.json`) |
| `install.sh` | Thin curl/sh shim over `thurbox-cli extension install` |

## Uninstall

```bash
thurbox-cli extension deactivate renovate   # off-switch: tears down session + automation
thurbox-cli extension uninstall renovate    # also removes the agents + manifest
thurbox-cli extension uninstall renovate --purge  # ...and deletes ~/renovate
```

> Remove any leftover update worktrees first (`git -C <repo> worktree remove …`)
> if you `--purge` while updates are mid-flight.
