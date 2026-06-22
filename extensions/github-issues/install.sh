#!/usr/bin/env sh
# Thin wrapper kept for the curl|sh one-liner. The real installer lives in
# thurbox itself:
#
#   thurbox-cli extension install github-issues
#
# This script just forwards to it — using a local checkout when run from one,
# otherwise the official remote source. It fetches the manifest + payload, lays
# down ~/github-issues, and activates the github-issues-tick automation — a
# deterministic exec sync (no agent, no session), which thurbox then self-heals.
#
# Usage:
#   ./install.sh                  # from a checkout
#   curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/extensions/github-issues/install.sh | sh
#
# Environment variables:
#   GITHUB_ISSUES_HOME=~/github-issues   install home (passed as --home)
#
# Authenticate afterwards: `gh auth login`. Then add repos to
# ~/github-issues/trackers.md. To turn it off:
#   thurbox-cli extension deactivate github-issues [--force --purge]

set -eu

command -v thurbox-cli >/dev/null 2>&1 || {
  echo "error: thurbox-cli not found in PATH (install thurbox first)" >&2
  exit 1
}

# Pass --home when GITHUB_ISSUES_HOME is set.
set --
[ -n "${GITHUB_ISSUES_HOME:-}" ] && set -- --home "$GITHUB_ISSUES_HOME"

# Prefer the checkout this script sits in (has extension.toml next to it);
# otherwise install the official "github-issues" extension from the remote source.
SRC_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd || true)"
if [ -n "$SRC_DIR" ] && [ -f "$SRC_DIR/extension.toml" ]; then
  exec thurbox-cli extension install "$SRC_DIR" "$@"
else
  exec thurbox-cli extension install github-issues "$@"
fi
