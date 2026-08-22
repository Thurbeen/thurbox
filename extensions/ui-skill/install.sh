#!/usr/bin/env sh
# Thin wrapper kept for the curl|sh one-liner. The real installer lives in
# thurbox itself:
#
#   thurbox-cli extension install ui-skill
#
# This script just forwards to it — using a local checkout when run from one,
# otherwise the official remote source. It installs the `thurbox-ui` agent skill
# into each coding CLI's personal skill directory (claude/codex/opencode/copilot
# and the shared ~/.agents convention), skipping any CLI that isn't installed.
#
# Usage:
#   ./install.sh                  # from a checkout
#   curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/extensions/ui-skill/install.sh | sh
#
# Environment variables:
#   UI_SKILL_HOME=<dir>   override install home (default: <config>/extensions/ui-skill)
#
# To remove it:  thurbox-cli extension uninstall ui-skill

set -eu

command -v thurbox-cli >/dev/null 2>&1 || {
  echo "error: thurbox-cli not found in PATH (install thurbox first)" >&2
  exit 1
}

# Pass --home when UI_SKILL_HOME is set.
set --
[ -n "${UI_SKILL_HOME:-}" ] && set -- --home "$UI_SKILL_HOME"

# Prefer the checkout this script sits in (has extension.toml next to it);
# otherwise install the official "ui-skill" extension from the remote source.
SRC_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd || true)"
if [ -n "$SRC_DIR" ] && [ -f "$SRC_DIR/extension.toml" ]; then
  exec thurbox-cli extension install "$SRC_DIR" "$@"
else
  exec thurbox-cli extension install ui-skill "$@"
fi
