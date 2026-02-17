---
model: sonnet
---

# Ship

Ship the current branch: commit (or amend), sync with
remote, push, and create/update a PR. Be efficient — batch
commands together, run independent commands in parallel, and
do NOT deliberate or explore. Just execute.

---

## Step 1 — Gather state (single parallel batch)

Run ALL of these commands in parallel in a single tool-call
round:

- `git status --porcelain`
- `git diff --stat && git diff --cached --stat`
- `git log --oneline -5`
- `git remote show origin | grep 'HEAD branch' | awk '{print $NF}'`
- `git branch --show-current`

From the results, determine: `DEFAULT_BRANCH`,
`CURRENT_BRANCH`, whether there are uncommitted changes, and
whether you're on the default branch. If on the default
branch, STOP and warn: "You're on the default branch. Create
a feature branch first."

---

## Step 2 — Commit or amend (skip if no changes)

If there are changes:

1. Run `git add` for all relevant files (skip `.env`,
   credentials, large binaries) AND
   `git merge-base --is-ancestor HEAD origin/<DEFAULT_BRANCH>`
   in parallel.
2. Based on the merge-base result:
   - Exit code 0 (HEAD is on default branch) → create a
     **new** conventional commit.
   - Exit code 1 (HEAD is local-only) → **amend** with
     `git commit --amend`.
3. Use conventional commit format. Infer type and scope from
   the diff. Do not ask — just pick the best fit.

---

## Step 3 — Sync, push, and check PR (single batch)

Run these sequentially in a single chained command:

```bash
git fetch origin && git rebase origin/<DEFAULT_BRANCH>
```

If rebase conflicts occur, STOP and show conflicting files.
Otherwise, immediately run in parallel:

- `git push --force-with-lease origin HEAD`
- `gh pr view --json url,title,state 2>/dev/null`

---

## Step 4 — PR + auto-merge

Based on the `gh pr view` result from Step 3:

- **PR exists**: Run `gh pr merge --auto --rebase` and print
  the PR URL.
- **No PR**: Run `gh pr create` targeting the default branch
  with a concise title (<70 chars) and a body with
  `## Summary` (bullet points) and `## Test plan` sections.
  Then immediately run `gh pr merge --auto --rebase`.

---

## Final Output

Print a short summary (no more than 5 lines):

- Commit: new or amended, with the message
- Rebase: clean or skipped
- Push: done
- PR: URL
- Auto-merge: enabled
