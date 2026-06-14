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
# the worker (1) ask clarifying questions ONE AT A TIME — push a single question
# via `thurbox-cli message send --kind questions` and WAIT for its answer before
# sending the next (flow relays each question to the user and sends the answer
# back), adaptively, dropping later questions once an answer clarifies enough —
# then (2) push a written plan via
# `--kind plan` and WAIT for the user's approval (relayed by flow), then
# (3) implement strictly against the approved plan. Worker → flow handoffs go
# through the durable message queue (not pane scraping); flow → worker replies
# arrive as normal session input. The flow agent no longer hand-types this
# boilerplate; it passes the structured flags and the script keeps the contract
# identical across every dispatch.
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

Clarify, then plan, then build — strictly in that order. You report back to the
flow agent through the thurbox message queue (replace <id> with THIS task's id);
flow surfaces each message to the user and relays the user's reply to you as a
new message in this session.

1. **Ask clarifying questions ONE AT A TIME.** Unless the task is genuinely
   trivial and unambiguous, ask clarifying questions about scope, edge cases, the
   acceptance bar, and anything underspecified — but send them **one at a time, in
   order, never batched**. Send a SINGLE question, then STOP:

       thurbox-cli message send --to flow --kind questions --task <id> \
         --body 'Q: ...'

   End your turn and wait — do NOT send the next question, plan, or write code
   yet. Flow relays that one question to the user and sends the answer back as a
   new message here; resume only when it arrives, then send your next question
   the same way (one message, then STOP). Ask **as many as you need** (typically
   3+), but be adaptive: let each answer shape the next question, and once an
   answer clarifies enough that the remaining questions are moot, drop them and
   proceed straight to the plan.

2. **Plan, then wait for approval.** With the answers in hand, write a structured
   plan and send it to flow for the user to approve — do NOT start coding yet:

       thurbox-cli message send --to flow --kind plan --task <id> \
         --body '## Problem
       <1–2 sentences>
       ## Acceptance criteria
       <concrete, checkable conditions; refine the accept: line above>
       ## Approach
       <files you will touch, the design, risks / open questions>'

   Then STOP and wait. Flow relays the plan to the user; resume only when the
   reply arrives. If the user approves, implement. If they request changes,
   revise the plan and send an updated `--kind plan` message, then wait again.
   (Do not use an interactive plan mode / approval modal — nothing drives it in a
   headless worker; the plan message IS the gate.)

3. **Implement** strictly against the approved plan. If the work would drift
   outside it, stop and note the change rather than silently expanding scope.
PLAN_BLOCK
  fi
  if [[ "$WORKER" -eq 1 ]]; then
    cat <<FOOTER

You are working in a dedicated git worktree on branch ${branch}; commit
your work there and open a PR when the accept criterion is met. When finished:
mark this task done (thurbox-cli task edit <id> --status done), then report the
result to the flow agent (replace <id> with THIS task's id) — this also wakes
flow so the next task dispatches immediately:

    thurbox-cli message send --to flow --kind result --task <id> \\
      --body '{"status":"ok|error","artifact":"...","notes":"...","pr_url":"..."}'
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
