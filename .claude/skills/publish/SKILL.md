---
name: publish
description: Refactor recent changes, ship as a PR with auto-merge, then monitor CI and auto-fix failures until merged.
user-invocable: true
allowed-tools: Read, Edit, Write, Bash, Glob, Grep, Agent, WebFetch
---

## Publish

Sync, refactor recent changes, sync again, then ship the
current branch as a PR with auto-merge enabled. After
shipping, monitor CI checks and proactively fix any failures
until the PR merges. Be thorough on refactoring but efficient
on shipping.

**Input:** `$ARGUMENTS` optionally describes what was done
(used for the commit message and PR description).

---

### Phase 0 — Pre-flight sync

Sync the branch with the remote default branch before
refactoring to avoid working on stale code.

Run in parallel:

- `git fetch origin`
- `git remote show origin | grep 'HEAD branch' | awk '{print $NF}'`
- `git branch --show-current`

Determine DEFAULT_BRANCH and CURRENT_BRANCH. If on default
branch, STOP: "Create a feature branch first."

Then rebase:

```bash
git rebase origin/<DEFAULT_BRANCH>
```

If conflicts, STOP and show files.

---

### Phase 1 — Refactor (2 passes)

#### Pre-work

Identify recent changes: use `git diff` and `git log` to
find newly implemented or modified code. Establish the list
of files to review.

#### Pass 1 — Structure & Clean Code

Re-read all identified files from disk, then:

1. **Clean Code principles**: intention-revealing names,
   small single-responsibility functions, DRY, remove dead
   code/unused imports, replace magic values with constants.
2. **KISS**: straightforward logic, reduce nesting with early
   returns, avoid premature abstractions.
3. **Readability**: consistent formatting with the project,
   top-down structure, self-documenting code (comments only
   for non-obvious "why").

Apply fixes, then summarize Pass 1 changes.

#### Pass 2 — Coherence & Consistency

Re-read ALL the same files again from disk (fresh read), then:

1. **Cross-file coherence**: naming conventions, patterns,
   and abstractions consistent across files and project.
2. **API & contract consistency**: function signatures, return
   types, error handling coherent between callers/callees.
3. **Logic review**: contradictory logic, redundant conditions,
   unreachable branches, mismatched assumptions.
4. **Import & dependency hygiene**: no circular deps, unused
   imports, or misplaced responsibilities.

Apply fixes, then summarize Pass 2 changes.

---

### Phase 2 — Ship

Execute the shipping process efficiently. Batch commands and
do NOT deliberate.

#### Step 1 — Gather state

Run `git status --porcelain` and
`git diff --stat && git diff --cached --stat` in parallel
to check for uncommitted changes.

#### Step 2 — Commit (skip if clean)

If there are changes:

1. Stage all relevant files (skip `.env`, credentials, large
   binaries). In parallel, check merge-base:
   `git merge-base --is-ancestor HEAD origin/<DEFAULT_BRANCH>`
2. Based on result:
   - Exit 0 (on default) → new conventional commit.
   - Exit 1 (local-only) → amend with `git commit --amend`.
3. Use conventional commit format. If `$ARGUMENTS` was
   provided, use it to inform the commit message. Infer type
   and scope from the diff.

#### Step 3 — Post-refactor sync & push

Sync again to pick up any changes that landed during
refactoring:

```bash
git fetch origin && git rebase origin/<DEFAULT_BRANCH>
```

If conflicts, STOP and show files. Otherwise, in parallel:

- `git push --force-with-lease origin HEAD`
- `gh pr view --json url,title,state 2>/dev/null`

#### Step 4 — PR + auto-merge

- **PR exists**: `gh pr merge --auto --rebase`, print URL.
- **No PR**: `gh pr create` with concise title (<70 chars),
  body with `## Summary` (bullets) and `## Test plan`. If
  `$ARGUMENTS` provided, use it for the summary. Then run
  `gh pr merge --auto --rebase`.

---

### Phase 3 — Monitor & Validate

After shipping, monitor the PR until it merges or a hard-stop
condition is reached. Be proactive: if CI fails, diagnose and
fix the issue, then push again.

This phase adapts to the project: not all repos have CI
checks, deployments, or auto-merge. Detect what applies and
skip what doesn't.

#### Step 0 — Detect project capabilities

```text
Run in parallel:
- gh pr checks --json name,state  → HAS_CHECKS (non-empty list)
- gh pr view --json autoMergeRequest → HAS_AUTO_MERGE (non-null)

If no checks and no auto-merge:
    Skip monitoring entirely → go to Final Output
If no checks but auto-merge is set:
    Wait for merge only (skip Fix step entirely)
```

#### Monitor loop

```text
fix_count = 0
wait = 30  # seconds
round = 0

while round < 10:
    round += 1
    sleep <wait> seconds
    wait = min(wait * 1.5, 120)

    Run in parallel:
    - gh pr view --json state,mergeStateStatus,mergeable
    - gh pr checks --json name,state,conclusion,detailsUrl

    Evaluate:

    A) PR state == MERGED → go to Validate step
    B) All checks passing, merge pending → continue loop
    C) One or more checks failed → go to Fix step
    D) PR closed (not merged) → STOP: "PR was closed"
    E) Merge conflicts → STOP: "Merge conflicts — rebase manually"
```

#### Fix step

```text
if fix_count >= 3:
    STOP: "CI still failing after 3 fix attempts.
    Failing checks: <list>. Manual intervention required."

fix_count += 1

1. Identify failed check(s) from gh pr checks output
2. For each failed check:
   a. Extract run-id from the details URL
   b. Get log: gh run view <run-id> --log-failed
   c. Read error output and diagnose root cause
3. Apply fixes to the codebase
4. Stage and commit:
   fix: resolve CI failure in <check-name>
5. Fetch + rebase:
   git fetch origin && git rebase origin/<DEFAULT_BRANCH>
6. Push: git push --force-with-lease origin HEAD
7. Reset wait = 30
8. Continue monitor loop
```

#### Validate step

After the PR merges:

1. Confirm merge: `gh pr view --json state` → assert MERGED
2. Check for deployment workflows:
   `gh run list --branch <DEFAULT_BRANCH> --limit 5 --json name,status,conclusion`
3. If a deployment run is found and in progress, poll every
   30s (max 5 times) until it completes
4. If no deployment runs exist, skip — not all projects deploy

---

### Final Output

Print a summary (max 8 lines):

- Refactor: Pass 1 + Pass 2 changes (counts)
- Commit: new or amended, with message
- PR: URL
- Auto-merge: enabled / not configured
- CI: passed (or fixed N times) / no checks
- Merge: confirmed / pending
- Deploy: status / no deployment detected
