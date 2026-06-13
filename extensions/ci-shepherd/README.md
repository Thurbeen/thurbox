# CI-shepherd — auto-address failing CI and review comments on your change requests

> **Status: experimental.** CI-shepherd is new and under active testing —
> expect the behavior spec, scripts, and installer to change between releases.

CI-shepherd watches your open **change requests** — GitHub PRs, GitLab MRs,
Bitbucket PRs — and whenever one picks up **failing CI**, a
**changes-requested review**, or falls **behind its target branch** (a rebase
branch-protection requires before merge), dispatches a worker session to address
it: the worker checks out the request branch, rebases it onto the target if it's
behind, fixes the feedback, pushes to the same branch (so the request updates),
and leaves a comment — then pings the shepherd so the next one dispatches. It
turns review round-trips into background work.

```text
---
Needs you: #51 (api): worker asks whether to drop the v1 field or deprecate it
🎯 Next: #48 (ui) — CI green after the fix, ready for your re-review
```

It is **forge-agnostic** *and* **agent-agnostic**, like thurbox itself. The only
thing baked in is **git** — *how* to talk to a repo's host is decided by the
**shepherd agent**, fresh each tick, from what the repo actually is. The monitor
and fixer are plain `agents.toml` entries (`shepherd`, `shepherd-worker`), so
each can be claude, codex, gemini, opencode, vibe, … The behavior lives in
[SHEPHERD.md](SHEPHERD.md), surfaced to whatever CLI you pick via context-file
symlinks (`CLAUDE.md`/`AGENTS.md`/`GEMINI.md` → `SHEPHERD.md`).

## Forges: any git host

There are two paths, and the agent picks the right one per repo automatically:

| Path | Forges | How |
|------|--------|-----|
| **Built-in fast path** | GitHub (`gh`), GitLab (`glab`), Bitbucket (`curl`+token) | `provider.sh` lists requests + supplies branch/checkout/comment commands; the agent just dispatches by number. |
| **Agent-driven** | **any other git forge** — Gitea/Forgejo/Codeberg, Azure DevOps, Sourcehut, self-hosted GitHub/GitLab, … | `provider.sh describe` hands the agent the remote + installed clients; the agent lists the requests itself and passes the commands it chose to `dispatch-fix.sh`. |

So there is **no forge allow-list** — anything reachable by a git remote and a
CLI/REST API works, because the agent reasons about it at runtime rather than
relying on hardcoded support. The three built-ins are just an optimization
(cheap, deterministic) for the common hosts. The provider fast-path **contract**
is documented at the top of [`scripts/provider.sh`](scripts/provider.sh) if you
want to *promote* a forge you use often into a built-in.

> **Verification status:** the GitHub fast path is verified end-to-end; the
> GitLab and Bitbucket fast paths are implemented against their documented APIs
> but not yet run live; the agent-driven path depends on the agent + the forge's
> own CLI/API.

## Requirements

- `git`, `jq`, and `thurbox-cli` on `PATH`.
- The client for each forge you watch, authenticated: `gh auth login` /
  `glab auth login`, or `BB_TOKEN` (or `BB_USER`+`BB_APP_PASSWORD`) for
  Bitbucket.

## Install

CI-shepherd is a thurbox **extension** (a declarative `extension.toml` manifest):

```bash
thurbox-cli extension install ci-shepherd
```

or from a checkout: `thurbox-cli extension install ./extensions/ci-shepherd`
(the `install.sh` / curl one-liner is a thin shim over this). Override the home
with `--home <dir>`. Installing:

1. lays down the shepherd home (`~/ci-shepherd`): `SHEPHERD.md` spec, helper
   scripts (incl. the provider adapter), context-file symlinks, claude
   permission settings, and a `repos.md` watch list (seeded once, then
   user-owned);
2. registers the `shepherd` / `shepherd-worker` agents in
   `~/.config/thurbox/agents.toml` (defaults: claude on haiku for the monitor,
   opus for the worker — edit the entries to change the model/CLI);
3. activates the dedicated `shepherd` session and a `shepherd-tick` automation
   (every 15 minutes) — both **self-healed** by thurbox at startup and on every
   tick, so deleting them is a no-op until you `deactivate`.

Re-running `extension install` refreshes the installer-owned files. `thurbox-cli
extension list` / `status ci-shepherd` show what's active.

## Use

- Add the repos to watch to `~/ci-shepherd/repos.md` (one row each; `author`
  defaults to `@me`, `provider` to `auto`).
- The shepherd sweeps on its schedule; trigger one now by sending `tick` to the
  shepherd session (or open it in the TUI).
- `status` for a one-screen report; `clean` to prune merged/closed worktrees.
- A fixer is a thurbox **task** named `fix #<n>: …`; its worker runs in an
  isolated worktree on the request branch and self-reports via a `===RESULT===`
  sentinel, pinging the shepherd the moment it finishes.

## How the fixer gets the request branch

thurbox's own `--worktree` always runs `git worktree add -b`, which fails on a
branch that already exists — and a request's head branch always does. So
`dispatch-fix.sh` adopts the branch itself into a shepherd-owned worktree under
`~/ci-shepherd/worktrees/<repo>-<provider>-<n>`, then spawns the fixer there.
The checkout is git-universal: for a built-in forge it uses the provider's
checkout (`gh pr checkout` / `glab mr checkout` / `git fetch`), and for any
other forge it uses the agent-supplied `--checkout-cmd` (or a plain
`git fetch origin <branch>` fallback). The worker pushes straight to the request
branch; `clean` removes the worktree once the request is no longer actionable
(refusing if it has uncommitted work).

## Files

| Path | Purpose |
|------|---------|
| `SHEPHERD.md` | The agent behavior spec (modes, actionability rules, output contract) |
| `scripts/provider.sh` | The forge adapter layer — normalizes GitHub/GitLab/Bitbucket onto one contract (incl. the `rebase` behind/conflict signal) |
| `scripts/classify.sh` | Pure request → action-flag classifier (`CHANGES-REQ`/`CI-FAIL`/`REBASE`/…); unit-tested by `classify.bats` |
| `scripts/shepherd-snapshot.sh` | One-call view: watched requests (with action flags) + fixer tasks/sessions/worktrees |
| `scripts/dispatch-fix.sh` | Prepare a request-branch worktree + create and run the fixer task |
| `scripts/parse-result.sh` | Extract the worker `===RESULT===` sentinel |
| `extension.toml` | Declarative install + runtime manifest (`thurbox-cli extension install` reads this) |
| `claude-settings.json` | Pre-approved permissions template (laid down as `.claude/settings.json`) |
| `install.sh` | Thin curl/sh shim over `thurbox-cli extension install` |

## Uninstall

```bash
thurbox-cli extension deactivate ci-shepherd   # off-switch: tears down session + automation
thurbox-cli extension uninstall ci-shepherd    # also removes the agents + manifest
thurbox-cli extension uninstall ci-shepherd --purge  # ...and deletes ~/ci-shepherd
```

> Remove any leftover fixer worktrees first (`git -C <repo> worktree remove …`)
> if you `--purge` while fixers are mid-flight.
