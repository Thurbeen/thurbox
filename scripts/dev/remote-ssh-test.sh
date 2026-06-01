#!/usr/bin/env bash
#
# Exercise thurbox's remote-SSH backend end-to-end against a throwaway Podman
# container that runs sshd + tmux + git — without touching your real ~/.ssh or
# ~/.config. All state lives under target/remote-ssh-test/ (gitignored) and, for
# the automated smoke test, an isolated XDG home in a temp dir.
#
# Usage:
#   scripts/dev/remote-ssh-test.sh up        # build image + start container
#   scripts/dev/remote-ssh-test.sh test      # isolated headless e2e (asserts ssh:podman)
#   scripts/dev/remote-ssh-test.sh hosts     # print the hosts.toml block for manual TUI testing
#   scripts/dev/remote-ssh-test.sh ssh       # open a shell on the container
#   scripts/dev/remote-ssh-test.sh down      # remove the container
#   scripts/dev/remote-ssh-test.sh clean     # remove container + all local state
#
# Env overrides: THURBOX_SSH_TEST_PORT (default 2222),
#                THURBOX_SSH_TEST_DIR  (default <repo>/target/remote-ssh-test)
#
# Requires: podman, ssh-keygen, cargo, python3.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORKDIR="${THURBOX_SSH_TEST_DIR:-$REPO_ROOT/target/remote-ssh-test}"
IMAGE="thurbox-remote-test"
CONTAINER="thurbox-remote"
PORT="${THURBOX_SSH_TEST_PORT:-2222}"
KEY="$WORKDIR/id_ed25519"
REMOTE_REPO="/srv/repo"

log() { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

ssh_remote() {
  ssh -p "$PORT" -i "$KEY" \
    -o IdentitiesOnly=yes -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=4 \
    root@localhost "$@"
}

# Emit a hosts.toml [[hosts]] block pointing at the container. Absolute paths
# only — thurbox passes ssh_opts to `ssh` via Command (no shell ~ expansion).
hosts_block() {
  cat <<EOF
[[hosts]]
name = "podman"
destination = "root@localhost"
ssh_opts = [
  "-p", "$PORT",
  "-i", "$KEY",
  "-o", "IdentitiesOnly=yes",
  "-o", "StrictHostKeyChecking=no",
  "-o", "UserKnownHostsFile=/dev/null",
  "-o", "LogLevel=ERROR",
  "-o", "ControlMaster=auto",
  "-o", "ControlPersist=10m",
  "-o", "ServerAliveInterval=15",
]
EOF
}

cmd_up() {
  command -v podman >/dev/null || die "podman not found"
  mkdir -p "$WORKDIR"
  if [ ! -f "$KEY" ]; then
    log "generating throwaway keypair at $KEY"
    ssh-keygen -t ed25519 -N "" -C thurbox-remote-test -f "$KEY" >/dev/null
  fi
  cp "$KEY.pub" "$WORKDIR/authorized_keys"

  cat > "$WORKDIR/Containerfile" <<'EOF'
FROM docker.io/library/debian:bookworm-slim
RUN apt-get update && \
    apt-get install -y --no-install-recommends openssh-server tmux git ca-certificates procps && \
    rm -rf /var/lib/apt/lists/* && \
    mkdir -p /run/sshd /root/.ssh && chmod 700 /root/.ssh
COPY authorized_keys /root/.ssh/authorized_keys
RUN chmod 600 /root/.ssh/authorized_keys && \
    git config --global user.email test@thurbox && \
    git config --global user.name thurbox-test && \
    git config --global init.defaultBranch main && \
    mkdir -p /srv/repo && cd /srv/repo && git init -q && \
    printf '# remote test repo\n' > README.md && \
    git add -A && git commit -qm "init" && \
    git branch -f feature/example main
EXPOSE 22
CMD ["/usr/sbin/sshd","-D","-e"]
EOF

  log "building image $IMAGE"
  podman build -t "$IMAGE" "$WORKDIR" >/dev/null
  podman rm -f "$CONTAINER" >/dev/null 2>&1 || true
  log "starting container $CONTAINER on port $PORT"
  podman run -d --name "$CONTAINER" --hostname thurbox-remote -p "$PORT:22" "$IMAGE" >/dev/null

  log "waiting for sshd"
  for _ in $(seq 1 20); do ssh_remote true 2>/dev/null && break; sleep 1; done
  ssh_remote true 2>/dev/null || die "container did not become reachable"
  log "ready: $(ssh_remote 'hostname; tmux -V' | tr '\n' ' ')"
  echo
  log "add this to ~/.config/thurbox-dev/hosts.toml for manual TUI testing:"
  hosts_block
}

cmd_hosts() { hosts_block; }

cmd_ssh() { ssh_remote "${@:-bash -l}"; }

# Remove any worktrees/branches/tmux state the test left on the container.
remote_reset() {
  ssh_remote '
    cd /srv/repo || exit 0
    git worktree list --porcelain | awk "/^worktree/ {print \$2}" | grep -v "^/srv/repo$" \
      | while read -r w; do git worktree remove --force "$w"; done
    git worktree prune
    git branch -D test/e2e 2>/dev/null || true
    tmux -L thurbox-dev kill-server 2>/dev/null || true
  ' 2>/dev/null || true
}

cmd_test() {
  command -v python3 >/dev/null || die "python3 not found"
  ssh_remote true 2>/dev/null || die "container not reachable — run '$0 up' first"

  # Fully isolated XDG home: never touches the user's real config/db, and no
  # running TUI watches this database.
  local xdg; xdg="$(mktemp -d)"
  trap 'rm -rf "$xdg"; remote_reset' RETURN
  mkdir -p "$xdg/config/thurbox-dev"
  hosts_block > "$xdg/config/thurbox-dev/hosts.toml"
  cat > "$xdg/config/thurbox-dev/agents.toml" <<'EOF'
default = "shell"
[[agents]]
name = "shell"
command = "bash"
EOF

  remote_reset
  log "creating a remote session via thurbox-cli (isolated DB)"
  local out
  out="$(cd "$REPO_ROOT" && \
    XDG_CONFIG_HOME="$xdg/config" XDG_DATA_HOME="$xdg/data" \
    cargo run -q --bin thurbox-cli -- session create \
      --name e2e --host podman --repo-path "$REMOTE_REPO" \
      --agent shell --worktree-branch test/e2e --base-branch main 2>/dev/null)"

  local id backend_type
  id="$(printf '%s' "$out" | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])')" \
    || die "session create failed: $out"
  backend_type="$(cd "$REPO_ROOT" && \
    XDG_CONFIG_HOME="$xdg/config" XDG_DATA_HOME="$xdg/data" \
    cargo run -q --bin thurbox-cli -- session get "$id" 2>/dev/null \
    | python3 -c 'import sys,json;print(json.load(sys.stdin).get("backend_type",""))')"

  # Confirm the artifacts really live on the remote.
  local remote_window remote_wt
  remote_window="$(ssh_remote 'tmux -L thurbox-dev list-windows -t thurbox-dev -F "#{window_name}" 2>/dev/null' | grep -c '^tb-e2e$' || true)"
  remote_wt="$(ssh_remote 'cd /srv/repo && git worktree list | grep -c test-e2e' 2>/dev/null || echo 0)"

  echo
  log "backend_type = $backend_type   remote tb-e2e windows = $remote_window   remote worktrees = $remote_wt"
  if [ "$backend_type" = "ssh:podman" ] && [ "$remote_window" -ge 1 ] && [ "$remote_wt" -ge 1 ]; then
    printf '\033[1;32mPASS\033[0m remote SSH session created on the container\n'
  else
    printf '\033[1;31mFAIL\033[0m expected ssh:podman + remote window + remote worktree\n'
    return 1
  fi
}

cmd_down() {
  podman rm -f "$CONTAINER" >/dev/null 2>&1 && log "removed container $CONTAINER" || log "no container to remove"
}

cmd_clean() {
  cmd_down
  podman rmi -f "$IMAGE" >/dev/null 2>&1 || true
  rm -rf "$WORKDIR"
  log "removed image + $WORKDIR"
}

case "${1:-}" in
  up)    cmd_up ;;
  test)  cmd_test ;;
  hosts) cmd_hosts ;;
  ssh)   shift; cmd_ssh "$@" ;;
  down)  cmd_down ;;
  clean) cmd_clean ;;
  *) sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
