#!/usr/bin/env sh
# Regenerate the Thurbox demo media (docs/media/thurbox-demo.{gif,mp4}).
#
# Fully automated and reproducible: drives the REAL thurbox TUI via VHS, but
# every session runs a deterministic in-binary "demo agent" (canned transcript)
# instead of a real coding-agent CLI — so there is no API key, no network, and
# no nondeterminism. Runs entirely isolated from your real thurbox:
#
#   * TMUX_TMPDIR points at a throwaway dir, so the demo's `thurbox-dev` tmux
#     server lives in its OWN socket directory — separate from any dev build of
#     thurbox you may already be running (which also uses the `thurbox-dev`
#     socket NAME, just under the default /tmp/tmux-UID dir). Without this, the
#     cleanup `kill-server` would tear down your live dev sessions.
#   * XDG_DATA_HOME / XDG_CONFIG_HOME point at a throwaway temp dir
#
# Requirements: cargo, git, tmux, and vhs (https://github.com/charmbracelet/vhs)
# with ffmpeg + ttyd available to it.
#
# Usage:  scripts/demo/record.sh

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
export THURBOX_BIN   # consumed by scripts/demo/thurbox.tape

# --- Isolated environment ----------------------------------------------------
DEMO_HOME=$(mktemp -d "${TMPDIR:-/tmp}/thurbox-demo.XXXXXX")
export XDG_DATA_HOME="$DEMO_HOME/data"
export XDG_CONFIG_HOME="$DEMO_HOME/config"
export TMUX_TMPDIR="$DEMO_HOME/tmux"     # isolate the tmux socket DIRECTORY
CFG_DIR="$XDG_CONFIG_HOME/thurbox-dev"   # dev_build subdir
mkdir -p "$CFG_DIR" "$XDG_DATA_HOME" "$TMUX_TMPDIR"

cleanup() {
    # Killing the isolated tmux server reaps the demo-agent panes. The extra
    # pkill is a narrowly-scoped safety net keyed on the internal-only
    # `__demo-agent` argv pointed at THIS build's binary, so it can never touch
    # an unrelated thurbox the user is running in another worktree.
    tmux -L thurbox-dev kill-server >/dev/null 2>&1 || true
    pkill -f "$THURBOX_BIN __demo-agent" >/dev/null 2>&1 || true
    rm -rf "$DEMO_HOME"
}
trap cleanup EXIT INT TERM

# --- Demo agent registry: each "agent" replays a scenario --------------------
# Multiple agents let pre-seeded sessions show different transcripts. `default`
# is "demo" so any extra session created during recording also replays.
cat > "$CFG_DIR/agents.toml" <<EOF
default = "demo"

[[agents]]
name = "demo"
command = "$THURBOX_BIN"
args = ["__demo-agent", "default"]

[[agents]]
name = "demo-snake"
command = "$THURBOX_BIN"
args = ["__demo-agent", "snake"]

[[agents]]
name = "demo-refactor"
command = "$THURBOX_BIN"
args = ["__demo-agent", "refactor"]
EOF

# --- A throwaway git repo for the sessions to live in ------------------------
DEMO_REPO="$DEMO_HOME/playground"
git init -q "$DEMO_REPO"
git -C "$DEMO_REPO" -c user.email=demo@thurbox -c user.name=demo \
    commit -q --allow-empty -m "init"

# --- Pre-seed a couple of sessions so the TUI opens populated ----------------
echo "==> Seeding demo sessions ..."
"$CLI_BIN" session create --name snake    --repo-path "$DEMO_REPO" --agent demo-snake    >/dev/null
"$CLI_BIN" session create --name refactor --repo-path "$DEMO_REPO" --agent demo-refactor >/dev/null

# --- Record -----------------------------------------------------------------
echo "==> Recording with VHS ..."
vhs "$SCRIPT_DIR/thurbox.tape"

echo "==> Done. Updated:"
echo "    docs/media/thurbox-demo.gif"
echo "    docs/media/thurbox-demo.mp4"
