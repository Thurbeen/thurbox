---
name: publish
description: Refactor recent changes then ship as a PR with auto-merge.
user-invocable: true
allowed-tools: Read, Edit, Write, Bash, Glob, Grep, Agent
---

## Publish

Sync, refactor recent changes, sync again, then ship the
current branch as a PR with auto-merge enabled. Be thorough
on refactoring but efficient on shipping.

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

### Final Output

Print a short summary (max 5 lines):

- Refactor: Pass 1 + Pass 2 changes (counts)
- Commit: new or amended, with message
- PR: URL
- Auto-merge: enabled
