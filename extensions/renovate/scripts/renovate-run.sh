#!/usr/bin/env bash
# renovate-run.sh — run Renovate against the CURRENT working directory, only
# ever on its **local** platform. This is the one place that invokes Renovate,
# and it hard-codes `--platform=local`: Renovate never talks to a git host, never
# needs a token, and never opens a branch/PR itself. It detects dependencies and
# (in `apply` mode) writes the updated manifests + lockfiles straight into the
# working tree. The thurbox worker then reviews, tests, commits, and — if you
# want a reviewable PR — pushes the branch and opens it (that part is the
# worker's job, not Renovate's).
#
# Usage (run from inside the repo/worktree you want updated):
#   renovate-run.sh apply  [--strategy patch|minor|major|all] [--config FILE]
#   renovate-run.sh lookup [--strategy patch|minor|major|all] [--config FILE]
#
#   apply   write the updates into the working tree (default)
#   lookup  resolve available updates but write nothing (a dry run, for the
#           monitor or a quick preview)
#
# --strategy layers a RENOVATE_CONFIG overlay on top of the base config file:
#   patch -> only patch updates       (major + minor disabled)
#   minor -> patch + minor updates    (major disabled)        [default]
#   major | all -> everything
#
# --config defaults to ~/.config/thurbox/extensions/renovate/renovate-config.json (Renovate's own config,
# passed via RENOVATE_CONFIG_FILE). Set GITHUB_COM_TOKEN in the environment to
# raise GitHub API limits / fetch changelogs; it is optional and read-only.

set -euo pipefail

MODE="${1:-apply}"
case "$MODE" in
  apply|lookup) shift ;;
  --*) MODE="apply" ;;
  *) echo "unknown mode: $MODE (expected 'apply' or 'lookup')" >&2; exit 2 ;;
esac

HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
HOME_DIR="$(CDPATH= cd -- "$HERE/.." && pwd)"
STRATEGY="minor"
CONFIG="$HOME_DIR/renovate-config.json"
while [ $# -gt 0 ]; do
  case "$1" in
    --strategy) STRATEGY="$2"; shift 2 ;;
    --config)   CONFIG="$2"; shift 2 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

# Map the per-repo strategy onto a Renovate config overlay (later packageRules
# win, so disabling rules layered last take effect).
case "$STRATEGY" in
  patch) OVERLAY='{"packageRules":[{"matchUpdateTypes":["major","minor"],"enabled":false}]}' ;;
  minor) OVERLAY='{"packageRules":[{"matchUpdateTypes":["major"],"enabled":false}]}' ;;
  major|all) OVERLAY='{}' ;;
  *) echo "unknown strategy: $STRATEGY (expected patch|minor|major|all)" >&2; exit 2 ;;
esac

# Resolve the Renovate entrypoint: a global install wins, else fall back to npx.
if command -v renovate >/dev/null 2>&1; then
  RENOVATE=(renovate)
elif command -v npx >/dev/null 2>&1; then
  RENOVATE=(npx --yes renovate)
else
  echo "error: neither 'renovate' nor 'npx' found on PATH (install Node >= 20)" >&2
  exit 3
fi

ARGS=(--platform=local)
[ "$MODE" = "lookup" ] && ARGS+=(--dry-run=lookup)

[ -f "$CONFIG" ] || { echo "config file not found: $CONFIG" >&2; exit 2; }

echo ">> renovate ${MODE} (strategy=${STRATEGY}, platform=local, cwd=$(pwd))" >&2
RENOVATE_CONFIG_FILE="$CONFIG" \
RENOVATE_CONFIG="$OVERLAY" \
LOG_LEVEL="${LOG_LEVEL:-info}" \
  exec "${RENOVATE[@]}" "${ARGS[@]}"
