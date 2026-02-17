---
model: haiku
---

# Sync

Sync the current branch with the remote default branch. Be
efficient — do not deliberate, just execute.

## Execute

Run these two commands in parallel:

- `git fetch origin`
- `git remote show origin | grep 'HEAD branch' | awk '{print $NF}'`

Then rebase onto the default branch:

```bash
git rebase origin/<DEFAULT_BRANCH>
```

If conflicts occur, STOP and show the conflicting files. Ask
the user how to proceed.

## Output

Print one line:
`Rebased on origin/<DEFAULT_BRANCH> — up to date.`
or the conflict details.
