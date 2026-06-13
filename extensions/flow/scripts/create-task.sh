#!/usr/bin/env bash
# create-task.sh — create a thurbox task and (when dispatchable) spawn its
# worker in the SAME call, so capture → session is atomic and immediate.
#
# Usage:
#   create-task.sh --title T --description D            # plain todo
#   create-task.sh --title T --description D \
#     --repo /abs/path --agent flow-worker \
#     [--accept "done when ..."] [--priority high|normal|low] \
#     [--worktree BRANCH] [--base origin/main] [--no-plan] [--no-dispatch]
#   create-task.sh ... --dry-run        # print the composed description, create nothing
#
# Plan-first dispatch: when an acceptance criterion is supplied (--accept) the
# script OWNS the worker prompt. It composes a standardized description —
#   priority/repo/accept header → the user's words → a mandatory **Planning
#   phase** → the result/notify footer — so every worker follows the same
# clarify → plan → build contract BEFORE touching code. The planning phase makes
# the worker (1) ask >=3 clarifying questions via a ===QUESTIONS=== sentinel and
# WAIT (the flow agent relays them to the user and sends the answers back), then
# (2) build the plan in its CLI's plan mode (Claude Code: /plan / EnterPlanMode)
# when it has one, then (3) implement strictly against it. The flow agent no
# longer hand-types this boilerplate; it passes the structured flags and the
# script keeps the contract identical across every dispatch.
#
# Without --accept the legacy behavior is preserved: --description is used
# verbatim, with no header/planning/footer composition (plain todos, or any
# direct caller that builds its own body).
#
# --no-plan drops only the planning section (still composes header + footer) for
# trivial mechanical work where a plan is overkill.
#
# Worktrees are always based on the REMOTE default branch: with --worktree and
# no --base, origin/main is assumed. When the base is a remote-tracking ref
# (origin/...), the repo is fetched first so the base is current.
#
# Prints the created task JSON; when dispatched, a second JSON line with the
# `task run` outcome ({"spawned": "..."} / {"reused": "..."}).

set -euo pipefail

TITLE="" DESC="" REPO="" AGENT="" WORKTREE="" BASE="" ACCEPT="" PRIORITY=""
DISPATCH=1 PLAN=1 DRYRUN=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --title)       TITLE="$2"; shift 2 ;;
    --description) DESC="$2"; shift 2 ;;
    --repo)        REPO="$2"; shift 2 ;;
    --agent)       AGENT="$2"; shift 2 ;;
    --worktree)    WORKTREE="$2"; shift 2 ;;
    --base)        BASE="$2"; shift 2 ;;
    --accept)      ACCEPT="$2"; shift 2 ;;
    --priority)    PRIORITY="$2"; shift 2 ;;
    --no-plan)     PLAN=0; shift ;;
    --no-dispatch) DISPATCH=0; shift ;;
    --dry-run)     DRYRUN=1; shift ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done
[[ -n "$TITLE" ]] || { echo "--title is required" >&2; exit 2; }

# A worker is dispatched when we know both the repo and the agent.
WORKER=0
[[ -n "$REPO" && -n "$AGENT" ]] && WORKER=1

# --- compose the worker prompt body ------------------------------------------
# With --accept we own the full description; the planning phase + footer make
# the plan-first contract identical on every dispatch.
build_description() {
  local branch="${WORKTREE:-flow/<task-slug>}"
  printf 'priority: %s\n' "${PRIORITY:-normal}"
  printf 'repo: %s\n' "${REPO:-unknown}"
  printf 'accept: %s\n' "$ACCEPT"
  if [[ -n "$DESC" ]]; then
    printf '\n%s\n' "$DESC"
  fi
  if [[ "$WORKER" -eq 1 && "$PLAN" -eq 1 ]]; then
    cat <<'PLAN_BLOCK'

## Planning phase — do this FIRST, before writing any code

Clarify, then plan, then build — strictly in that order.

1. **Ask clarifying questions first.** Unless the task is genuinely trivial and
   unambiguous, ask **at least 3** clarifying questions (more if it needs them)
   about scope, edge cases, the acceptance bar, and anything underspecified.
   Print them as ONE block, then STOP:

       ===QUESTIONS===
       {"questions": ["<q1>", "<q2>", "<q3>"]}

   End your turn and wait — do NOT plan or write code yet. The flow agent relays
   your questions to the user and sends the answers back to this session; resume
   only when they arrive.

2. **Plan.** With the answers in hand, build a structured plan. If your CLI has
   a plan mode (Claude Code: `/plan` / EnterPlanMode), use it; otherwise post the
   plan as a message. Cover:
   - **Problem** — restate the problem in 1–2 sentences.
   - **Acceptance criteria** — the concrete, checkable conditions for "done".
     Start from the `accept:` line above and make each item testable.
   - **Approach** — the implementation approach / architecture: the files you'll
     touch, the design, and any risks or open questions.

3. **Implement** strictly against the plan. If the work would drift outside it,
   stop and note the change rather than silently expanding scope.
PLAN_BLOCK
  fi
  if [[ "$WORKER" -eq 1 ]]; then
    cat <<FOOTER

You are working in a dedicated git worktree on branch ${branch}; commit your
work there and open a PR when the accept criterion is met. When finished: mark
this task done (thurbox-cli task edit <id> --status done), print a final line
\`===RESULT===\` followed by one line of JSON:
{"status":"ok|error","artifact":"...","notes":"...","pr_url":"..."}
then notify the flow agent so the next task dispatches immediately:
thurbox-cli session send "\$(thurbox-cli session list | jq -r '.[] | select(.name=="flow") | .id')" "tick"
FOOTER
  fi
}

if [[ -n "$ACCEPT" ]]; then
  DESC="$(build_description)"
fi

if [[ "$DRYRUN" -eq 1 ]]; then
  printf '%s\n' "$DESC"
  exit 0
fi

ARGS=(task create --title "$TITLE")
[[ -n "$DESC" ]] && ARGS+=(--description "$DESC")
if [[ -n "$REPO" ]]; then
  [[ -n "$AGENT" ]] || { echo "--repo requires --agent" >&2; exit 2; }
  ARGS+=(--repo "$REPO" --agent "$AGENT")
  if [[ -n "$WORKTREE" ]]; then
    [[ -n "$BASE" ]] || BASE="origin/main"
    ARGS+=(--worktree "$WORKTREE" --base "$BASE")
    # A remote-tracking base is only as fresh as the last fetch.
    if [[ "$BASE" == origin/* ]]; then
      git -C "$REPO" fetch origin --quiet 2>/dev/null || true
    fi
  fi
fi

CREATED="$(thurbox-cli "${ARGS[@]}")"
printf '%s\n' "$CREATED"

if [[ -n "$REPO" && "$DISPATCH" -eq 1 ]]; then
  ID="$(printf '%s' "$CREATED" | jq -r .id)"
  thurbox-cli task run "$ID"
fi
