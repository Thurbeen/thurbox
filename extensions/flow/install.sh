#!/usr/bin/env sh
# Thin wrapper kept for the curl|sh one-liner. The real installer now lives in
# thurbox itself:
#
#   thurbox-cli extension install flow
#
# This script just forwards to it — using a local checkout when run from one,
# otherwise the official remote source. It fetches the manifest + payload, lays
# down ~/flow, registers the flow agents in agents.toml, and activates the flow
# session (which thurbox then self-heals). Flow is event-driven — worker pushes
# over the mailbox queue wake it; there is no scheduled automation.
#
# Usage:
#   ./install.sh                  # from a checkout
#   curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/extensions/flow/install.sh | sh
#
# Environment variables:
#   FLOW_HOME=~/flow              install home (passed as --home)
#
# To turn flow off:  thurbox-cli extension deactivate flow [--force --purge]

set -eu

command -v thurbox-cli >/dev/null 2>&1 || {
  echo "error: thurbox-cli not found in PATH (install thurbox first)" >&2
  exit 1
}

# Pass --home when FLOW_HOME is set (preserves the old override).
set --
[ -n "${FLOW_HOME:-}" ] && set -- --home "$FLOW_HOME"

# Prefer the checkout this script sits in (has extension.toml next to it);
# otherwise install the official "flow" extension from the remote source.
SRC_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd || true)"
if [ -n "$SRC_DIR" ] && [ -f "$SRC_DIR/extension.toml" ]; then
  exec thurbox-cli extension install "$SRC_DIR" "$@"
else
  exec thurbox-cli extension install flow "$@"
fi
