#!/usr/bin/env bash
# provider.sh — the VCS-host adapter layer for ci-shepherd. Every forge-specific
# detail lives here behind a uniform contract, so the snapshot/dispatch scripts
# stay host-neutral. Add a new forge by adding a `<name>_*` function set and a
# detection rule in resolve_provider — nothing else changes.
#
# Usage:  provider.sh <verb> <repo_path> [args...]
#
# Provider resolution: the PROVIDER env var (github|gitlab|bitbucket) wins;
# otherwise it's auto-detected from `git remote get-url origin`.
#
# Verbs (the adapter contract):
#   provider-of <repo>                  -> prints the resolved provider name
#   list <repo> <author>                -> normalized JSON array (see shape below)
#   meta <repo> <number>                -> {"title":..,"branch":..,"base":..,"url":..}
#   checkout <repo> <worktree> <number> -> checks the PR/MR branch out into <worktree>
#   feedback-cmd <repo> <number>        -> prints the shell command the WORKER runs
#                                          to read review feedback
#   comment-cmd <repo> <number>         -> prints a shell command TEMPLATE the worker
#                                          runs to post a summary (body as $1)
#
# Normalized `list` element (all providers map their API onto this):
#   {"number":42,"title":"..","branch":"feat/x","draft":false,
#    "review":"CHANGES_REQUESTED|APPROVED|REVIEW_REQUIRED|none",
#    "ci":"FAIL|PENDING|OK|none",
#    "rebase":"NEEDED|CONFLICT|none","url":".."}
#
# `rebase` tells the shepherd a request can't merge as-is and must be brought up
# to date with its target branch first: NEEDED = the head branch is *behind* the
# base (branch-protection "require branches to be up to date" will block the
# merge); CONFLICT = it has diverged so far it no longer merges cleanly (a rebase
# with conflict resolution is required). Providers that can't tell map to "none".

set -euo pipefail

VERB="${1:-}"; REPO="${2:-}"; shift 2 || true
[ -n "$VERB" ] && [ -n "$REPO" ] || { echo "usage: provider.sh <verb> <repo> [args]" >&2; exit 2; }
[ -d "$REPO" ] || { echo "repo not found: $REPO" >&2; exit 2; }

have() { command -v "$1" >/dev/null 2>&1; }

resolve_provider() {
  if [ -n "${PROVIDER:-}" ] && [ "$PROVIDER" != auto ]; then printf '%s' "$PROVIDER"; return; fi
  url="$(git -C "$REPO" remote get-url origin 2>/dev/null || true)"
  case "$url" in
    *github.com*)    echo github ;;
    *gitlab*)        echo gitlab ;;
    *bitbucket.org*) echo bitbucket ;;
    *) echo "unknown" ;;
  esac
}

# --- GitHub (gh) -----------------------------------------------------------

github_list() { # <author>
  have gh || { echo "gh (GitHub CLI) not installed" >&2; return 3; }
  ( cd "$REPO" && gh pr list --author "$1" --state open \
      --json number,title,headRefName,isDraft,reviewDecision,statusCheckRollup,mergeStateStatus,mergeable,url ) \
  | jq '[ .[] | {
      number, title, branch: .headRefName, draft: .isDraft, url,
      review: ((.reviewDecision // "") | if . == "" then "none" else . end),
      ci: ((.statusCheckRollup // []) | map(.conclusion // .state // "")
           | if length == 0 then "none"
             elif any(. == "FAILURE" or . == "ERROR" or . == "TIMED_OUT") then "FAIL"
             elif any(. == "PENDING" or . == "IN_PROGRESS" or . == "QUEUED") then "PENDING"
             else "OK" end),
      # mergeStateStatus BEHIND = out of date with base (branch protection blocks);
      # DIRTY / mergeable CONFLICTING = diverged, a conflict-resolving rebase needed.
      rebase: ((.mergeStateStatus // "") as $m | (.mergeable // "") as $g
           | if $m == "BEHIND" then "NEEDED"
             elif $m == "DIRTY" or $g == "CONFLICTING" then "CONFLICT"
             else "none" end) } ]'
}
github_meta() { # <number>
  ( cd "$REPO" && gh pr view "$1" --json title,headRefName,baseRefName,url ) \
  | jq '{title, branch: .headRefName, base: .baseRefName, url}'
}
github_checkout() { ( cd "$2" && gh pr checkout "$3" ); }   # <worktree> <number>
github_feedback_cmd() { printf 'gh pr view %s --comments; gh api "repos/{owner}/{repo}/pulls/%s/comments"' "$1" "$1"; }
github_comment_cmd()  { printf 'gh pr comment %s --body' "$1"; }

# --- GitLab (glab) ---------------------------------------------------------

gitlab_user() { glab api user 2>/dev/null | jq -r '.username' 2>/dev/null || true; }
gitlab_list() { # <author>
  have glab || { echo "glab (GitLab CLI) not installed" >&2; return 3; }
  author="$1"; [ "$author" = "@me" ] && author="$(gitlab_user)"
  # glab proxies the raw GitLab MR API, whose field names we normalize here.
  ( cd "$REPO" && glab mr list --state opened ${author:+--author "$author"} --output json ) \
  | jq '[ .[] | {
      number: .iid, title, branch: .source_branch,
      draft: (.draft // .work_in_progress // false),
      url: .web_url,
      review: (if (.blocking_discussions_resolved == false) then "CHANGES_REQUESTED" else "none" end),
      ci: ((.pipeline.status // (.head_pipeline.status) // "none") as $s
           | if $s == "failed" or $s == "canceled" then "FAIL"
             elif $s == "running" or $s == "pending" or $s == "created" then "PENDING"
             elif $s == "success" then "OK" else "none" end),
      # cannot_be_merged = a real merge conflict; diverged_commits_count>0 = behind
      # the target (fast-forward-only projects need a rebase to merge).
      rebase: (if (.merge_status // "") == "cannot_be_merged" then "CONFLICT"
               elif ((.diverged_commits_count // 0) > 0) then "NEEDED"
               else "none" end) } ]'
}
gitlab_meta() { # <number>
  ( cd "$REPO" && glab mr view "$1" --output json ) \
  | jq '{title, branch: .source_branch, base: .target_branch, url: .web_url}'
}
gitlab_checkout() { ( cd "$2" && glab mr checkout "$3" ); }   # <worktree> <number>
gitlab_feedback_cmd() { printf 'glab mr view %s --comments' "$1"; }
gitlab_comment_cmd()  { printf 'glab mr note %s --message' "$1"; }

# --- Bitbucket Cloud (REST via curl) ---------------------------------------
# Auth: BB_TOKEN (Bearer) or BB_USER + BB_APP_PASSWORD (basic). Checkout is
# plain git (the MR branch lives on origin for same-repo PRs).

bb_slug() { # -> "workspace/repo"
  git -C "$REPO" remote get-url origin 2>/dev/null \
    | sed -E 's#^(git@bitbucket.org:|https://[^/]*bitbucket.org/)##; s#\.git$##'
}
bb_curl() { # <method> <path-suffix> [data]
  base="https://api.bitbucket.org/2.0/repositories/$(bb_slug)"
  if [ -n "${BB_TOKEN:-}" ]; then auth=(-H "Authorization: Bearer $BB_TOKEN")
  elif [ -n "${BB_USER:-}" ] && [ -n "${BB_APP_PASSWORD:-}" ]; then auth=(-u "$BB_USER:$BB_APP_PASSWORD")
  else echo "set BB_TOKEN or BB_USER+BB_APP_PASSWORD for Bitbucket" >&2; return 3; fi
  if [ -n "${3:-}" ]; then
    curl -fsSL "${auth[@]}" -X "$1" -H 'Content-Type: application/json' -d "$3" "$base/$2"
  else
    curl -fsSL "${auth[@]}" -X "$1" "$base/$2"
  fi
}
bitbucket_list() { # <author> (filtered client-side; Bitbucket has no review-decision field)
  have curl || { echo "curl not installed" >&2; return 3; }
  author="$1"
  bb_curl GET "pullrequests?state=OPEN&pagelen=50" \
  | jq --arg author "$author" '[ .values[]
      | select($author == "@me" or $author == "*" or (.author.nickname // "") == $author)
      | { number: .id, title, branch: .source.branch.name,
          draft: (.draft // false), url: .links.html.href,
          review: ([.participants[]? | select(.state == "changes_requested")] | if length>0 then "CHANGES_REQUESTED" else "none" end),
          ci: "none",
          # Bitbucket list payload exposes neither ahead/behind nor merge state,
          # so we cannot tell from here; the shepherd agent can inspect a
          # specific PR if needed.
          rebase: "none" } ]'
}
bitbucket_meta() { # <number>
  bb_curl GET "pullrequests/$1" \
  | jq '{title, branch: .source.branch.name, base: .destination.branch.name, url: .links.html.href}'
}
bitbucket_checkout() { # <worktree> <number>
  branch="$(bitbucket_meta "$2" | jq -r '.branch')"
  ( cd "$1" && git fetch origin "$branch" --quiet && git checkout -B "$branch" FETCH_HEAD )
}
bitbucket_feedback_cmd() { printf 'provider.sh _bb-comments %q %s' "$REPO" "$1"; }
bitbucket_comment_cmd()  { printf 'provider.sh _bb-comment %q %s' "$REPO" "$1"; }

# What the agent reads to DECIDE how to handle this repo's forge each time:
# the remote, the best forge guess, and which clients are installed. For a
# built-in forge the agent can just use the fast paths below; for anything else
# it improvises and supplies the commands to dispatch-fix.sh itself.
describe() {
  url="$(git -C "$REPO" remote get-url origin 2>/dev/null || echo none)"
  printf 'remote: %s\n' "$url"
  printf 'forge-guess: %s\n' "$PROV"
  printf 'clis:'
  for c in gh glab tea hub; do
    if have "$c"; then printf ' %s=yes' "$c"; else printf ' %s=no' "$c"; fi
  done
  if have curl; then printf ' curl=yes'; else printf ' curl=no'; fi
  printf '\n'
  case "$PROV" in
    github|gitlab|bitbucket)
      printf 'builtin: yes — fast paths: provider.sh {list,meta,checkout,feedback-cmd,comment-cmd}\n' ;;
    *)
      printf 'builtin: no — decide the commands yourself and pass --branch/--checkout-cmd/--feedback-cmd/--comment-cmd to dispatch-fix.sh\n' ;;
  esac
}

PROV="$(resolve_provider)"

case "$VERB" in
  provider-of)   echo "$PROV" ;;
  describe)      describe ;;
  list)          "${PROV}_list" "${1:?author}" ;;
  meta)          "${PROV}_meta" "${1:?number}" ;;
  checkout)      "${PROV}_checkout" "${1:?worktree}" "${2:?number}" ;;
  feedback-cmd)  "${PROV}_feedback_cmd" "${1:?number}" ;;
  comment-cmd)   "${PROV}_comment_cmd" "${1:?number}" ;;
  # Internal helpers so the Bitbucket worker has a real read/comment command.
  _bb-comments)  bb_curl GET "pullrequests/${1:?number}/comments" | jq -r '.values[]? | "- \(.user.display_name): \(.content.raw)"' ;;
  _bb-comment)   bb_curl POST "pullrequests/${1:?number}/comments" "$(jq -nc --arg b "${2:?body}" '{content:{raw:$b}}')" >/dev/null && echo "commented" ;;
  *) echo "unknown verb: $VERB" >&2; exit 2 ;;
esac
