#!/usr/bin/env sh
# Regenerate the Thurbox Automations feature demo
# (docs/media/automations-demo.{gif,mp4}).
#
# Like scripts/demo/record.sh, this drives the REAL thurbox TUI via VHS with a
# deterministic in-binary "demo agent" (canned transcript) — no API key, no
# network, no nondeterminism — fully isolated from your real thurbox:
#
#   * TMUX_TMPDIR points at a throwaway dir so the demo's `thurbox-dev` tmux
#     server lives in its own socket directory.
#   * XDG_DATA_HOME / XDG_CONFIG_HOME point at a throwaway temp dir.
#
# It pre-seeds a single session so the always-present Automations pane shows
# beneath the session list, then records creating an automation from the pane.
#
# Requirements: cargo, git, tmux, and vhs (https://github.com/charmbracelet/vhs)
# with ffmpeg + ttyd available to it.
#
# Usage:  scripts/demo/record-automations.sh

set -eu

# --- Locate the repo root (this script lives in scripts/demo/) ---------------
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
cd "$REPO_ROOT"

# --- Preflight: required tools ----------------------------------------------
missing=
for tool in cargo git tmux vhs; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        missing="$missing $tool"
    fi
done
if [ -n "$missing" ]; then
    echo "error: missing required tool(s):$missing" >&2
    echo "  vhs:  https://github.com/charmbracelet/vhs (needs ffmpeg + ttyd)" >&2
    exit 1
fi

# --- Build the dev binaries (version 0.0.0-dev => dev_build cfg) -------------
echo "==> Building thurbox (dev) ..."
cargo build --bin thurbox --bin thurbox-cli

THURBOX_BIN="$REPO_ROOT/target/debug/thurbox"
CLI_BIN="$REPO_ROOT/target/debug/thurbox-cli"
export THURBOX_BIN   # consumed by scripts/demo/automations.tape

# --- Isolated environment ----------------------------------------------------
DEMO_HOME=$(mktemp -d "${TMPDIR:-/tmp}/thurbox-auto-demo.XXXXXX")
export XDG_DATA_HOME="$DEMO_HOME/data"
export XDG_CONFIG_HOME="$DEMO_HOME/config"
export TMUX_TMPDIR="$DEMO_HOME/tmux"     # isolate the tmux socket DIRECTORY
CFG_DIR="$XDG_CONFIG_HOME/thurbox-dev"   # dev_build subdir
mkdir -p "$CFG_DIR" "$XDG_DATA_HOME" "$TMUX_TMPDIR"

cleanup() {
    tmux -L thurbox-dev kill-server >/dev/null 2>&1 || true
    pkill -f "$THURBOX_BIN __demo-agent" >/dev/null 2>&1 || true
    rm -rf "$DEMO_HOME"
}
trap cleanup EXIT INT TERM

# --- Demo agent registry: the seeded session replays a canned transcript -----
cat > "$CFG_DIR/agents.toml" <<EOF
default = "demo"

[[agents]]
name = "demo"
command = "$THURBOX_BIN"
args = ["__demo-agent", "default"]
EOF

# --- A throwaway git repo for the session to live in -------------------------
DEMO_REPO="$DEMO_HOME/playground"
git init -q "$DEMO_REPO"
git -C "$DEMO_REPO" -c user.email=demo@thurbox -c user.name=demo \
    commit -q --allow-empty -m "init"

# --- Pre-seed one session so the TUI opens populated -------------------------
echo "==> Seeding demo session ..."
"$CLI_BIN" session create --name api --repo-path "$DEMO_REPO" --agent demo >/dev/null

# --- Record -----------------------------------------------------------------
echo "==> Recording with VHS ..."
vhs "$SCRIPT_DIR/automations.tape"

echo "==> Done. Updated:"
echo "    docs/media/automations-demo.gif"
echo "    docs/media/automations-demo.mp4"
