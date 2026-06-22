#!/usr/bin/env bash
# dispatch-update.sh — create a thurbox task on a FRESH dependency-update branch
# and spawn its updater worker in the SAME call, so capture -> session is atomic.
#
# Unlike ci-shepherd (which adopts an existing PR branch), a renovate run starts
# a brand-new branch, so thurbox's native --worktree does the job: it runs
# `git worktree add -b renovate/updates-<ts> origin/main` for us. The worker runs
# Renovate's LOCAL platform inside that worktree, tests the result, commits, and
# (optionally) opens a review PR.
#
# Usage:
#   dispatch-update.sh --repo /abs/repo [--strategy patch|minor|major|all] \
#     [--provider auto|github|gitlab|bitbucket|...] [--agent renovate-worker] \
#     [--base origin/main] [--no-dispatch]
#
# Prints the created task JSON; when dispatched, a second JSON line with the
# `task run` outcome.

set -euo pipefail

HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
HOME_DIR="$(CDPATH= cd -- "$HERE/.." && pwd)"
RENOVATE_RUN="$HOME_DIR/scripts/renovate-run.sh"

REPO="" STRATEGY="minor" PROVIDER="auto" AGENT="renovate-worker" BASE="origin/main" DISPATCH=1
while [ $# -gt 0 ]; do
  case "$1" in
    --repo)        REPO="$2"; shift 2 ;;
    --strategy)    STRATEGY="$2"; shift 2 ;;
    --provider)    PROVIDER="$2"; shift 2 ;;
    --agent)       AGENT="$2"; shift 2 ;;
    --base)        BASE="$2"; shift 2 ;;
    --no-dispatch) DISPATCH=0; shift ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done
[ -n "$REPO" ] || { echo "--repo is required" >&2; exit 2; }
[ -d "$REPO" ] || { echo "repo not found: $REPO" >&2; exit 2; }
case "$STRATEGY" in patch|minor|major|all) : ;; *) echo "bad --strategy: $STRATEGY" >&2; exit 2 ;; esac

NAME="$(basename "$REPO")"
# A timestamped branch is unique per dispatch, so `git worktree add -b` never
# collides with an earlier run's branch.
BRANCH="renovate/updates-$(date +%Y%m%d-%H%M%S)"

# A remote-tracking base is only as fresh as the last fetch.
if [ "${BASE#origin/}" != "$BASE" ]; then
  git -C "$REPO" fetch origin --quiet 2>/dev/null || true
fi

DESC="$(cat <<EOF
You are the dependency-update worker for **$NAME** (strategy: \`$STRATEGY\`).
Your working directory is already a fresh worktree on \`$BRANCH\`, branched from
\`$BASE\`. Renovate must only ever run **locally** — never against a git host.

accept: dependencies updated to the latest allowed versions, the project still
builds + its tests pass, and a review PR is open (or, if no forge client is
available, the branch is committed locally and you say so).

Do, in order:
1. Apply updates with Renovate's local platform (writes changes into THIS
   worktree, no bot/token/PR):
   $RENOVATE_RUN apply --strategy $STRATEGY
   If it reports no updates available, there is nothing to do — skip to the
   result with {"status":"ok","notes":"no updates available"}.
2. Review \`git diff\`. Run the project's own build/test/lint (detect it:
   \`cargo test\` / \`npm test\` / \`pytest\` / a Makefile target / CI config) so
   the bumps don't break anything. If one update breaks the build and you can't
   fix it minimally, drop just that update (revert its hunk) and note it.
3. Commit: \`git add -A && git commit -m "chore(deps): update dependencies ($STRATEGY)"\`.
4. Push the branch and open a PR for review (do NOT merge). Work the forge out
   from \`git remote -v\` and use its client (gh/glab) or REST API; provider hint
   is \`$PROVIDER\`. Title it "chore(deps): dependency updates"; summarize what
   changed. If no forge client/credentials are available, leave the branch
   committed locally and report that under \`notes\`.
If you are blocked (a major upgrade that needs a human decision, test failures
you can't resolve), do NOT guess — stop and report it in the result
\`question\` field.

When finished: mark this task done (thurbox-cli task edit \$THURBOX_TASK --status done),
print a final line \`===RESULT===\` followed by one line of JSON:
{"status":"ok|error","artifact":"...","notes":"...","pr_url":"..."}
then notify the renovate monitor so the next repo dispatches immediately:
thurbox-cli session send "\$(thurbox-cli session list --json | jq -r '.[] | select(.name=="renovate") | .id')" "tick"
EOF
)"

CREATED="$(thurbox-cli task create --title "update $NAME deps ($STRATEGY)" \
  --description "$DESC" --repo "$REPO" --agent "$AGENT" \
  --worktree "$BRANCH" --base "$BASE")"
printf '%s\n' "$CREATED"

if [ "$DISPATCH" -eq 1 ]; then
  ID="$(printf '%s' "$CREATED" | jq -r .id)"
  thurbox-cli task run "$ID"
fi
