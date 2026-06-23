#!/usr/bin/env sh
# Thin wrapper kept for the curl|sh one-liner. The real installer now lives in
# thurbox itself:
#
#   thurbox-cli extension install renovate
#
# This script just forwards to it — using a local checkout when run from one,
# otherwise the official remote source. It fetches the manifest + payload, lays
# down ~/.config/thurbox/extensions/renovate, registers the renovate agents in agents.toml, and activates
# the renovate session + renovate-tick automation (which thurbox then
# self-heals).
#
# Usage:
#   ./install.sh                  # from a checkout
#   curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/extensions/renovate/install.sh | sh
#
# Environment variables:
#   RENOVATE_HOME=<dir>   override install home (default: <config>/extensions/renovate)
#
# Renovate runs via `npx --yes renovate` (needs Node >= 20). Authenticate your
# forge client(s) afterwards if you want workers to open review PRs:
# gh auth login / glab auth login. To turn it off:
#   thurbox-cli extension deactivate renovate [--force --purge]

set -eu

command -v thurbox-cli >/dev/null 2>&1 || {
  echo "error: thurbox-cli not found in PATH (install thurbox first)" >&2
  exit 1
}

# Pass --home when RENOVATE_HOME is set (preserves the override).
set --
[ -n "${RENOVATE_HOME:-}" ] && set -- --home "$RENOVATE_HOME"

# Prefer the checkout this script sits in (has extension.toml next to it);
# otherwise install the official "renovate" extension from the remote source.
SRC_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd || true)"
if [ -n "$SRC_DIR" ] && [ -f "$SRC_DIR/extension.toml" ]; then
  exec thurbox-cli extension install "$SRC_DIR" "$@"
else
  exec thurbox-cli extension install renovate "$@"
fi
