---
name: publish
description: Refactor the changes made in this session, ship them as a PR with auto-merge, then monitor CI and auto-fix failures until merged. Scopes itself to the repos this session actually worked in.
allowed-tools: read edit write bash grep find agent fetch_content
---

## Publish

Refactor the changes made in this session, ship them as a PR
with auto-merge, then watch CI until the PR merges (or a
hard-stop is reached). Be thorough on refactoring, efficient
on shipping.

**Input:** the user message appended after `/skill:publish`
(if any).

The input optionally describes what was done, used for the commit
message and PR description, and/or names the repo(s) to
publish. It may be empty.

Everything here is plain `git` / `gh` run through Bash. There
are no helper scripts: when a command's output is ambiguous or
a step fails, read the actual error and adapt — do not retry a
canned pipeline.

---

### Phase 0 — Scope (which repos)

Publishing too much is the worst failure mode of this skill.
Scope comes from **session context only** — never from scanning
the filesystem.

Build the candidate list in priority order:

1. Repos explicitly named in the input.
2. Repos containing files this session created or edited
   (look back at the conversation, not at the disk).
3. The repo containing the current working directory.
4. Only if cwd is not itself a repo but a multi-repo workspace
   (a directory of symlinks that each resolve to a git repo —
   e.g. a thurbox symlink workspace): the member repos.

Hard rules:

- **Never** enumerate repos beyond the list above. No scanning
  `~`, no "all accessible directories", no sibling worktrees,
  no `find`/Glob sweeps for `.git`.
- A candidate **qualifies** only if it is on a feature branch
  (not the default branch) and has uncommitted changes or
  commits ahead of `origin/<default>`.
- A qualifying repo whose changes this session did **not**
  make (pre-existing dirt, someone else's branch) is **not**
  published. List it under "skipped" in the final output so
  the user can decide.
  Exception: the cwd repo when the user invoked `/skill:publish`
  with no other context — they are standing in it, publish it.
- Zero qualifying repos → STOP: "Nothing to publish."

State the final scope in one line ("Publishing: <repos>;
skipped: <repos + reason>") before doing any work, then run
Phases 1–4 for each scoped repo sequentially.

---

### Phase 1 — Sync

Per repo, before touching code:

1. `git fetch origin`.
2. Determine the default branch (e.g.
   `git symbolic-ref --short refs/remotes/origin/HEAD`; if
   unset, ask `gh repo view` — pick whichever works, don't
   parse `git remote show` output).
3. If on the default branch, STOP for this repo: "Create a
   feature branch first."
4. Rebase onto `origin/<default>`. On conflict: abort the
   rebase, record the conflicting files, skip this repo's
   remaining phases, continue with the next repo.

---

### Phase 2 — Refactor (2 passes)

Review only the changes being shipped: the diff against
`origin/<default>` plus uncommitted changes. Establish the
file list from that diff.

#### Pass 1 — Structure & Clean Code

Re-read the identified files from disk, then:

1. **Clean Code**: intention-revealing names, small
   single-responsibility functions, DRY, remove dead code and
   unused imports, replace magic values with constants.
2. **KISS**: straightforward logic, early returns over
   nesting, no premature abstractions.
3. **Readability**: match project formatting, comments only
   for non-obvious "why".

Apply fixes, then summarize.

#### Pass 2 — Coherence & Consistency

Re-read the same files fresh, then:

1. **Cross-file coherence**: naming, patterns, abstractions
   consistent with each other and the project.
2. **API consistency**: signatures, return types, error
   handling coherent between callers and callees.
3. **Logic review**: contradictions, redundant conditions,
   unreachable branches, mismatched assumptions.

Apply fixes, then summarize. These summaries are intermediate
— continue straight into Phase 3.

---

### Phase 3 — Ship

Execute efficiently; do not deliberate.

1. **Stage deliberately.** Review `git status` and stage the
   files that belong to this work — never `git add -A`
   blindly. Leave out secrets and junk (`.env`, credentials,
   keys, large binaries, scratch files).
2. **Commit.** If `HEAD` is already on the remote (or shared
   with the default branch), create a new conventional commit;
   if the tip commit is local-only, amend it. Use the input
   to inform the message; infer type and scope from the diff.
   Skip if nothing to commit.
3. **Re-sync.** `git fetch origin` and rebase again to pick up
   anything that landed meanwhile (same conflict handling as
   Phase 1).
4. **Push.** `git push --force-with-lease origin HEAD`.
5. **PR.** If `gh pr view` finds an open PR, reuse it.
   Otherwise `gh pr create` — title under 70 chars, body with
   `## Summary` bullets and a `## Test plan`.
6. **Auto-merge.** `gh pr merge --auto --rebase`. If the repo
   doesn't allow it, note that and move on — do not merge
   without auto-merge unless the user asked.

Record the PR URL for Phase 4.

---

### Phase 4 — Monitor & validate

Watch the PR until it merges or hits a hard stop. Adapt to the
repo: first check whether it has CI checks
(`gh pr checks`) and whether auto-merge actually got enabled
(`gh pr view --json autoMergeRequest`). No checks and no
auto-merge → nothing to monitor, go to Final Output.

**Monitor loop** — up to ~10 rounds, sleeping 30s at first and
backing off toward 2 minutes between polls. Each round, check
PR state and checks via `gh pr view` / `gh pr checks`:

- Merged → validate (below), then next repo.
- Closed without merging → record and stop this repo.
- Merge conflict → record "rebase manually" and stop this repo.
- A check failed → fix (below).
- Otherwise → keep waiting.

**Fixing a failed check** — at most 3 fix attempts per repo,
then record "manual intervention required" and stop this repo.
Pull the failing job's log (`gh run view <id> --log-failed`),
diagnose the root cause from the actual error — don't guess
from the check name. Apply the fix, commit
(`fix: resolve CI failure in <check>`), rebase, push, and
resume monitoring from a fresh 30s wait.

**Validate after merge** — look for a deployment workflow on
the default branch (`gh run list --branch <default>`). If one
is running, poll briefly (a few rounds) until it concludes;
if none exists, skip — not every repo deploys.

---

### Final Output

One block per repo (and only now — nothing earlier is a
terminal summary):

- Refactor: Pass 1 + Pass 2 changes (counts)
- Commit: new or amended, with message
- PR: URL
- CI: passed / fixed N times / no checks / failed
- Merge: confirmed / pending / conflict
- Deploy: status / no deployment detected

If any repos were skipped in Phase 0, list them with the
reason.
