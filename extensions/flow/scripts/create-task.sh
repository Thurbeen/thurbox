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
# Multi-repo (a task spanning several repositories): repeat --add-repo for each
# EXTRA repo beyond --repo. Each extra gets its OWN isolated worktree on the same
# --worktree branch, off its own base (PATH@origin/<base>, default origin/main):
#
#   create-task.sh --title T --description D \
#     --repo /abs/primary --agent flow-worker-heavy --accept "..." \
#     --worktree flow/<slug> --base origin/main \
#     --add-repo /abs/other@origin/main --add-repo /abs/third@origin/master
#
# Use --add-dir /abs/path (repeatable) for a repo that should be attached AS-IS
# (no new branch) — e.g. a read-only reference checkout. The worker sees every
# repo as a sub-directory of a per-session symlink workspace.
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
# through the durable message queue (not pane scraping); the worker passes NO ids
# (thurbox injects THURBOX_SESSION/THURBOX_TASK and auto-stamps provenance + the
# task tag), and flow → worker replies arrive back in the worker's own inbox
# (drained on the `inbox` wake). The flow agent no longer hand-types this
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
ADD_REPOS=() ADD_DIRS=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --title)       TITLE="$2"; shift 2 ;;
    --description) DESC="$2"; shift 2 ;;
    --repo)        REPO="$2"; shift 2 ;;
    --agent)       AGENT="$2"; shift 2 ;;
    --worktree)    WORKTREE="$2"; shift 2 ;;
    --base)        BASE="$2"; shift 2 ;;
    --add-repo)    ADD_REPOS+=("$2"); shift 2 ;;
    --add-dir)     ADD_DIRS+=("$2"); shift 2 ;;
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

# Multi-repo when any extra repo/dir is attached.
MULTI=0
[[ ${#ADD_REPOS[@]} -gt 0 || ${#ADD_DIRS[@]} -gt 0 ]] && MULTI=1

# --- compose the worker prompt body ------------------------------------------
# With --accept we own the full description; the planning phase + footer make
# the plan-first contract identical on every dispatch.
build_description() {
  local branch="${WORKTREE:-flow/<task-slug>}"
  printf 'priority: %s\n' "${PRIORITY:-normal}"
  printf 'repo: %s\n' "${REPO:-unknown}"
  if [[ "$MULTI" -eq 1 ]]; then
    local extra
    for extra in ${ADD_REPOS[@]+"${ADD_REPOS[@]}"}; do printf 'repo (extra, worktree): %s\n' "$extra"; done
    for extra in ${ADD_DIRS[@]+"${ADD_DIRS[@]}"}; do printf 'repo (extra, attached as-is): %s\n' "$extra"; done
  fi
  printf 'accept: %s\n' "$ACCEPT"
  if [[ -n "$DESC" ]]; then
    printf '\n%s\n' "$DESC"
  fi
  if [[ "$WORKER" -eq 1 && "$PLAN" -eq 1 ]]; then
    cat <<'PLAN_BLOCK'

## Planning phase — do this FIRST, before writing any code

Clarify, then plan, then build — strictly in that order. You coordinate with the
flow agent through the thurbox message queue. Thurbox already knows who you are
and which task you're on, so you pass **no ids** — just `--to flow --kind … --body
…`. When flow relays the user's reply, your pane is woken with the word `inbox`;
the moment you see it, read the reply with:

    thurbox-cli message inbox --claim

(the answer/approval is the message body — `--for` defaults to you, no id needed).

1. **Ask clarifying questions ONE AT A TIME.** Unless the task is genuinely
   trivial and unambiguous, ask clarifying questions about scope, edge cases, the
   acceptance bar, and anything underspecified — but send them **one at a time, in
   order, never batched**. Send a SINGLE question, then STOP:

       thurbox-cli message send --to flow --kind questions \
         --body 'Q: ...'

   End your turn and wait — do NOT send the next question, plan, or write code
   yet. Flow relays that one question to the user; when the answer arrives you are
   woken with `inbox` — drain it (above), then send your next question the same
   way (one message, then STOP). Ask **as many as you need** (typically 3+), but
   be adaptive: let each answer shape the next question, and once an answer
   clarifies enough that the remaining questions are moot, drop them and proceed
   straight to the plan.

2. **Plan, then wait for approval.** With the answers in hand, write a structured
   plan and send it to flow for the user to approve — do NOT start coding yet:

       thurbox-cli message send --to flow --kind plan \
         --body '## Problem
       <1–2 sentences>
       ## Acceptance criteria
       <concrete, checkable conditions; refine the accept: line above>
       ## Approach
       <files you will touch, the design, risks / open questions>'

   Then STOP and wait. Flow relays the plan to the user; when you are woken with
   `inbox`, drain it for the decision. If the user approves, implement. If they
   request changes, revise the plan and send an updated `--kind plan` message,
   then wait again. (Do not use an interactive plan mode / approval modal —
   nothing drives it in a headless worker; the plan message IS the gate.)

3. **Implement** strictly against the approved plan. If the work would drift
   outside it, stop and note the change rather than silently expanding scope.
PLAN_BLOCK
  fi
  if [[ "$WORKER" -eq 1 && "$MULTI" -eq 1 ]]; then
    cat <<FOOTER

You are working in a **multi-repo** session: each repository above is its own
sub-directory of your working dir, and the worktree repos are each on a dedicated
branch ${branch} (off their own base). Make each repo's changes in ITS OWN
sub-directory, commit them on that repo's branch, and **open a separate PR per
repo you changed** (do not touch a repo you did not need to change). When
finished: mark this task done (thurbox-cli task edit \$THURBOX_TASK --status done),
then report the result to the flow agent — this also wakes flow so the next task
dispatches immediately (no ids: thurbox tags the message with your task for you):

    thurbox-cli message send --to flow --kind result \\
      --body '{"status":"ok|error","artifact":"...","notes":"...","pr_urls":["...","..."]}'
FOOTER
  elif [[ "$WORKER" -eq 1 ]]; then
    cat <<FOOTER

You are working in a dedicated git worktree on branch ${branch}; commit
your work there and open a PR when the accept criterion is met. When finished:
mark this task done (thurbox-cli task edit \$THURBOX_TASK --status done), then
report the result to the flow agent — this also wakes flow so the next task
dispatches immediately (no ids: thurbox tags the message with your task for you):

    thurbox-cli message send --to flow --kind result \\
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

# A worktree base is only as fresh as the last fetch when it tracks a remote.
fetch_if_remote() {  # $1 = repo dir, $2 = base ref
  [[ "$2" == origin/* ]] && git -C "$1" fetch origin --quiet 2>/dev/null || true
}

ARGS=(task create --title "$TITLE")
[[ -n "$DESC" ]] && ARGS+=(--description "$DESC")
if [[ -n "$REPO" ]]; then
  [[ -n "$AGENT" ]] || { echo "--repo requires --agent" >&2; exit 2; }
  ARGS+=(--repo "$REPO" --agent "$AGENT")
  if [[ -n "$WORKTREE" ]]; then
    [[ -n "$BASE" ]] || BASE="origin/main"
    ARGS+=(--worktree "$WORKTREE" --base "$BASE")
    fetch_if_remote "$REPO" "$BASE"
  fi
  # Multi-repo: forward each extra. A worktree extra is `PATH@base` (default the
  # primary's --base); a plain --add-dir is attached as-is (no branch).
  if [[ "$MULTI" -eq 1 && -z "$WORKTREE" ]]; then
    echo "--add-repo requires --worktree (the shared branch)" >&2; exit 2
  fi
  for extra in ${ADD_REPOS[@]+"${ADD_REPOS[@]}"}; do
    epath="${extra%@*}"; ebase="${extra#*@}"
    [[ "$ebase" == "$extra" ]] && ebase="$BASE"   # no @base → primary base
    ARGS+=(--add-repo "${epath}@${ebase}")
    fetch_if_remote "$epath" "$ebase"
  done
  for extra in ${ADD_DIRS[@]+"${ADD_DIRS[@]}"}; do
    ARGS+=(--add-dir "$extra")
  done
fi

CREATED="$(thurbox-cli "${ARGS[@]}")"
printf '%s\n' "$CREATED"

if [[ -n "$REPO" && "$DISPATCH" -eq 1 ]]; then
  ID="$(printf '%s' "$CREATED" | jq -r .id)"
  thurbox-cli task run "$ID"
fi
