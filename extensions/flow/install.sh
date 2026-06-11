#!/usr/bin/env sh
# Install the thurbox "flow" extension: an agent-agnostic triage agent that
# captures brain-dumps into thurbox tasks, dispatches workers, and protects
# your focus. See FLOW.md for the behavior spec.
#
# Usage:
#   ./install.sh                  # from a checkout
#   curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/extensions/flow/install.sh | sh
#
# Environment variables:
#   FLOW_HOME=~/flow              flow agent home directory
#   FLOW_CMD=claude               CLI for the triager (any agents.toml-able CLI)
#   FLOW_ARGS="--model claude-haiku-4-5"        extra args for the triager
#   WORKER_CMD=claude             CLI for the default worker
#   WORKER_ARGS="--model claude-opus-4-8"       extra args for the worker
#   WORKER_HEAVY_CMD=claude       CLI for the heavy worker
#   WORKER_HEAVY_ARGS="--model claude-fable-5"  extra args for the heavy worker
#   TICK_CRON="*/5 * * * *"       tick automation schedule
#
# Idempotent: existing agents.toml entries, the flow session, and the
# flow-tick automation are left untouched on re-run.

set -eu

FLOW_HOME="${FLOW_HOME:-$HOME/flow}"
FLOW_CMD="${FLOW_CMD:-claude}"
FLOW_ARGS="${FLOW_ARGS:---model claude-haiku-4-5}"
WORKER_CMD="${WORKER_CMD:-claude}"
WORKER_ARGS="${WORKER_ARGS:---model claude-opus-4-8}"
WORKER_HEAVY_CMD="${WORKER_HEAVY_CMD:-claude}"
WORKER_HEAVY_ARGS="${WORKER_HEAVY_ARGS:---model claude-fable-5}"
TICK_CRON="${TICK_CRON:-*/5 * * * *}"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/thurbox"
AGENTS_TOML="$CONFIG_DIR/agents.toml"
RAW_BASE="https://raw.githubusercontent.com/Thurbeen/thurbox/main/extensions/flow"

say()  { printf '%s\n' "$*"; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

command -v thurbox-cli >/dev/null 2>&1 || die "thurbox-cli not found in PATH (install thurbox first)"
command -v jq >/dev/null 2>&1 || die "jq is required"

# --- flow home: spec, scripts, context-file symlinks, routing table --------

mkdir -p "$FLOW_HOME/scripts"

# Source files: prefer the checkout next to this script, fall back to GitHub
# (pipe-to-shell install).
SRC_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd || true)"
fetch() { # fetch <relpath> <dest>
  if [ -n "$SRC_DIR" ] && [ -f "$SRC_DIR/$1" ]; then
    cp "$SRC_DIR/$1" "$2"
  elif command -v curl >/dev/null 2>&1; then
    curl -fsSL "$RAW_BASE/$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$RAW_BASE/$1" -O "$2"
  else
    die "cannot fetch $1 (no local checkout, curl, or wget)"
  fi
}

fetch FLOW.md "$FLOW_HOME/FLOW.md"
for s in create-task.sh flow-snapshot.sh parse-result.sh; do
  fetch "scripts/$s" "$FLOW_HOME/scripts/$s"
  chmod +x "$FLOW_HOME/scripts/$s"
done
say "installed spec + scripts into $FLOW_HOME"

# Surface FLOW.md to every CLI's context-file convention. Skip anything that
# is already a regular file (never clobber user content).
for ctx in CLAUDE.md AGENTS.md GEMINI.md; do
  if [ -f "$FLOW_HOME/$ctx" ] && [ ! -L "$FLOW_HOME/$ctx" ]; then
    say "skip: $FLOW_HOME/$ctx exists (not a symlink), leaving it alone"
  else
    ln -sf FLOW.md "$FLOW_HOME/$ctx"
  fi
done

if [ ! -f "$FLOW_HOME/repos.md" ]; then
  cat > "$FLOW_HOME/repos.md" <<'EOF'
# Repo routing table

Used by the flow agent to map brain-dump keywords to repositories.
Add one row per repo you work on; the flow agent appends rows as it
learns new paths.

| name | path | base | keywords |
|------|------|------|----------|
| example | /home/me/repositories/example | main | example, sample |
EOF
  say "seeded $FLOW_HOME/repos.md (edit it!)"
fi

# --- agents.toml: flow / flow-worker / flow-worker-heavy aliases -----------

toml_args() { # "$@"-style words -> "a", "b" list
  out=""
  for w in $1; do
    out="$out\"$w\", "
  done
  printf '%s' "${out%, }"
}

append_agent() { # append_agent <name> <cmd> <args>
  name="$1" cmd="$2" args="$3"
  if grep -q "name = \"$name\"" "$AGENTS_TOML" 2>/dev/null; then
    say "agents.toml: '$name' already present, skipping"
    return
  fi
  {
    printf '\n[[agents]]\n'
    printf 'name = "%s"\n' "$name"
    printf 'command = "%s"\n' "$cmd"
    [ -n "$args" ] && printf 'args = [%s]\n' "$(toml_args "$args")"
    # Session pinning/resume flags are claude's; other CLIs resume the last
    # session in cwd themselves (see the agents.toml seed for examples).
    if [ "$cmd" = "claude" ]; then
      printf 'resume_args = ["--resume", "{id}"]\n'
      printf 'fork_args = ["--resume", "{id}", "--fork-session"]\n'
      printf 'new_session_args = ["--session-id", "{id}"]\n'
    fi
  } >> "$AGENTS_TOML"
  say "agents.toml: added '$name' ($cmd $args)"
}

mkdir -p "$CONFIG_DIR"
if [ ! -f "$AGENTS_TOML" ]; then
  # Same content thurbox seeds on first run, so later runs read it unchanged.
  cat > "$AGENTS_TOML" <<'EOF'
# Thurbox coding-agent definitions (seeded by the flow extension installer).

default = "claude"

[[agents]]
name = "claude"
command = "claude"
resume_args = ["--resume", "{id}"]
fork_args = ["--resume", "{id}", "--fork-session"]
new_session_args = ["--session-id", "{id}"]

[[agents]]
name = "codex"
command = "codex"

[[agents]]
name = "gemini"
command = "gemini"

[[agents]]
name = "opencode"
command = "opencode"

[[agents]]
name = "aider"
command = "aider"

[[agents]]
name = "vibe"
command = "vibe"
EOF
  say "seeded $AGENTS_TOML"
fi

append_agent flow "$FLOW_CMD" "$FLOW_ARGS"
append_agent flow-worker "$WORKER_CMD" "$WORKER_ARGS"
append_agent flow-worker-heavy "$WORKER_HEAVY_CMD" "$WORKER_HEAVY_ARGS"

# --- claude nicety: pre-approve the flow tooling so ticks run unattended ---

if [ "$FLOW_CMD" = "claude" ] && [ ! -f "$FLOW_HOME/.claude/settings.json" ]; then
  mkdir -p "$FLOW_HOME/.claude"
  cat > "$FLOW_HOME/.claude/settings.json" <<EOF
{
  "permissions": {
    "allow": [
      "Bash(thurbox-cli:*)",
      "Bash($FLOW_HOME/scripts/create-task.sh:*)",
      "Bash($FLOW_HOME/scripts/flow-snapshot.sh:*)",
      "Bash($FLOW_HOME/scripts/parse-result.sh:*)",
      "Bash(./scripts/create-task.sh:*)",
      "Bash(./scripts/flow-snapshot.sh:*)",
      "Bash(./scripts/parse-result.sh:*)",
      "Bash(jq:*)",
      "Read(//${FLOW_HOME#/}/**)",
      "Edit(//${FLOW_HOME#/}/**)",
      "Write(//${FLOW_HOME#/}/**)"
    ]
  }
}
EOF
  say "wrote $FLOW_HOME/.claude/settings.json (unattended tick permissions)"
fi

# --- bootstrap: dedicated session + tick automation ------------------------

FLOW_UUID="$(thurbox-cli session list 2>/dev/null | jq -r '.[] | select(.name == "flow") | .id' | head -1)"
if [ -n "$FLOW_UUID" ]; then
  say "session 'flow' already exists ($FLOW_UUID)"
else
  FLOW_UUID="$(thurbox-cli session create --name flow --agent flow --repo-path "$FLOW_HOME" | jq -r .id)"
  say "created session 'flow' ($FLOW_UUID)"
fi

if thurbox-cli automation list 2>/dev/null | jq -e '.[] | select(.name == "flow-tick")' >/dev/null; then
  say "automation 'flow-tick' already exists"
else
  thurbox-cli automation create --name flow-tick \
    --trigger "cron:$TICK_CRON" \
    --session "$FLOW_UUID" \
    --prompt "tick" >/dev/null
  say "created automation 'flow-tick' ($TICK_CRON)"
fi

say ""
say "flow extension installed."
say "  1. Edit $FLOW_HOME/repos.md with your repos."
say "  2. Open the 'flow' session in thurbox and brain-dump at it,"
say "     or: thurbox-cli session send $FLOW_UUID \"your dump\""
say "  3. Map flow-worker / flow-worker-heavy in $AGENTS_TOML to any CLI."
