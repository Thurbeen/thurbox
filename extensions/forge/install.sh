#!/usr/bin/env sh
# Thin wrapper kept for the curl|sh one-liner. The real installer now lives in
# thurbox itself:
#
#   thurbox-cli extension install forge
#
# This script just forwards to it — using a local checkout when run from one,
# otherwise the official remote source. It fetches the manifest + payload, lays
# down ~/.config/thurbox/extensions/forge, registers the forge agent in agents.toml, and activates the
# forge session + forge-scan automation (which thurbox then self-heals).
#
# Usage:
#   ./install.sh                  # from a checkout
#   curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/extensions/forge/install.sh | sh
#
# Environment variables:
#   FORGE_HOME=<dir>   override install home (default: <config>/extensions/forge)
#
# To turn forge off:  thurbox-cli extension deactivate forge [--force --purge]

set -eu

command -v thurbox-cli >/dev/null 2>&1 || {
  echo "error: thurbox-cli not found in PATH (install thurbox first)" >&2
  exit 1
}

# Pass --home when FORGE_HOME is set (preserves the old override).
set --
[ -n "${FORGE_HOME:-}" ] && set -- --home "$FORGE_HOME"

# Prefer the checkout this script sits in (has extension.toml next to it);
# otherwise install the official "forge" extension from the remote source.
SRC_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd || true)"
if [ -n "$SRC_DIR" ] && [ -f "$SRC_DIR/extension.toml" ]; then
  exec thurbox-cli extension install "$SRC_DIR" "$@"
else
  exec thurbox-cli extension install forge "$@"
fi
