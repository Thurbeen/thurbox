#!/usr/bin/env sh
# Thin wrapper kept for the curl|sh one-liner. The real installer lives in
# thurbox itself:
#
#   thurbox-cli extension install linear
#
# This script just forwards to it — using a local checkout when run from one,
# otherwise the official remote source. It fetches the manifest + payload, lays
# down ~/.config/thurbox/extensions/linear, and activates the linear-tick automation — a deterministic exec
# sync (no agent, no session), which thurbox then self-heals.
#
# Usage:
#   ./install.sh                  # from a checkout
#   curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/extensions/linear/install.sh | sh
#
# Environment variables:
#   LINEAR_HOME=<dir>   override install home (default: <config>/extensions/linear)
#
# Authenticate afterwards: put your Linear personal API key in
# ~/.config/thurbox/extensions/linear/credentials.env as `LINEAR_API_KEY=lin_api_xxom` (Settings → Account →
# Security & access → Personal API keys). Then add teams to ~/.config/thurbox/extensions/linear/trackers.md.
# To turn it off:
#   thurbox-cli extension deactivate linear [--force --purge]

set -eu

command -v thurbox-cli >/dev/null 2>&1 || {
  echo "error: thurbox-cli not found in PATH (install thurbox first)" >&2
  exit 1
}

# Pass --home when LINEAR_HOME is set.
set --
[ -n "${LINEAR_HOME:-}" ] && set -- --home "$LINEAR_HOME"

# Prefer the checkout this script sits in (has extension.toml next to it);
# otherwise install the official "linear" extension from the remote source.
SRC_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd || true)"
if [ -n "$SRC_DIR" ] && [ -f "$SRC_DIR/extension.toml" ]; then
  exec thurbox-cli extension install "$SRC_DIR" "$@"
else
  exec thurbox-cli extension install linear "$@"
fi
